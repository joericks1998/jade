//! The backend half of inference: what `src/infer/prelude.jde` asks of
//! `__infer`, written into the program from the model's plan.
//!
//! Four files, one job each:
//!
//! - *`tables.rs`*: the tokenizer's pieces, merges and added tokens, as string
//!   constants, and the Unicode classes its splitter asks about.
//! - *`map.rs`*: `map` opens each region of the model's memory by the name
//!   dovata's record gave at build time and maps it.
//! - *`metal.rs`*: `setup` builds a Metal device, compiles `kernels.metal` and
//!   makes a pipeline per kernel; `step` runs one forward step as one command
//!   buffer.
//! - *`next.rs`*: `next` gives one token of a reply per call. The prelude
//!   calls it until a stop token or the limit, so a pipe can hand each piece
//!   on as it comes.
//!
//! This file answers each `__infer` operation and holds what the four share.
//!
//! One lock covers the model's memory and the GPU, so two tasks dereferencing
//! prompts take turns: the KV cache holds one conversation at a time. Every
//! entry point takes the lock, calls an inner function that may raise, and
//! gives the lock back. The lock is not held between tokens, since a pipe runs
//! the program's own code there; a conversation whose tokens another prompt
//! displaced reads them back in at its next token.
//!
//! macOS on Apple Silicon only, for now; `build::compile` refuses a program that
//! runs a model anywhere else before this directory is reached.

mod map;
mod metal;
mod next;
mod tables;

use inkwell::module::Linkage;
use inkwell::types::{BasicTypeEnum, FunctionType};
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};

use super::{Cx, F};
use crate::codegen::libcache::shared::{Slot, State};
use crate::infer::plan::InferPlan;
use crate::mir::{InferFn, Ty};

/// How many tokens a reply runs to unless `set_max_tokens` says otherwise.
const DEFAULT_MAX_TOKENS: i64 = 256;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function performs one of the prelude's operations.
    ///
    /// Inputs:
    /// - `f`: the function being written.
    /// - `m`: the operation.
    /// - `args`: its operands.
    ///
    /// Outputs: its value and type.
    pub fn infer_op(
        &self,
        f: &F<'_, 'ctx>,
        m: InferFn,
        args: &[BasicValueEnum<'ctx>],
    ) -> (BasicValueEnum<'ctx>, Ty) {
        // The model's plan is the program's.
        self.mark_program_dependent();
        let t = match m {
            InferFn::Data | InferFn::Taint | InferFn::Text => Ty::Str,
            InferFn::Config | InferFn::CharClass | InferFn::MaxTokens | InferFn::SetMaxTokens => {
                Ty::Int
            }
            InferFn::Ready => Ty::Bool,
            InferFn::Map | InferFn::Setup => Ty::Nil,
            InferFn::Next | InferFn::Conversation => Ty::Int,
        };
        // The reply's length is the one setting a program without a model has.
        let limit = self.infer_global("jade.infer.max_tokens", Slot::Word(0));
        match m {
            InferFn::MaxTokens => {
                let v = f.load_i64(limit);
                return (f.select_int(f.eq(v, f.i64(0)), f.i64(DEFAULT_MAX_TOKENS), v).into(), t);
            }
            InferFn::SetMaxTokens => {
                let n = args[0].into_int_value();
                let n = f.select_int(f.slt(n, f.i64(1)), f.i64(1), n);
                f.store(limit, n);
                return (n.into(), t);
            }
            _ => {}
        }
        // `build::compile` reads the plan before any program that runs a model is
        // built, so this is a compiler bug, said as an error rather than a crash.
        if self.infer.is_none() {
            f.raise(
                "internal compiler error: a program that runs a model was built without its plan",
            );
            return (crate::codegen::memory::layout::zero(self.llty(&t)), t);
        }
        let v = match m {
            InferFn::Data => f.callv(self.infer_data_fn(), args),
            InferFn::Config => {
                let split = self.plan().split;
                let k = args[0].into_int_value();
                f.select_int(f.eq(k, f.i64(0)), f.i64(split), f.i64(0)).into()
            }
            InferFn::CharClass => f.callv(self.infer_char_class_fn(), args),
            InferFn::Taint => f.callv(self.infer_taint_fn(), args),
            InferFn::Text => {
                f.retain(&Ty::Str, args[0]);
                args[0]
            }
            InferFn::Ready => f.callv(
                self.infer_locked_fn("ready", Some(self.i1t().into()), &[], |f, _| {
                    let r = f.load_i64(f.cx.infer_ready());
                    f.ret(f.ne(r, f.i64(0)));
                }),
                args,
            ),
            InferFn::Map => {
                let fun = self.infer_locked_fn("map", None, &[], |f, _| {
                    f.call(f.cx.infer_map_fn(), &[]);
                    f.ret_void();
                });
                f.call(fun, &[]);
                f.i8(0).into()
            }
            InferFn::Setup => {
                let fun = self.infer_locked_fn("setup", None, &[], |f, _| {
                    let pool = f.call_ptr(f.cx.objc_fn("objc_autoreleasePoolPush"), &[]);
                    f.call(f.cx.infer_setup_fn(), &[]);
                    f.call(f.cx.objc_fn("objc_autoreleasePoolPop"), &[pool.into()]);
                    f.ret_void();
                });
                f.call(fun, args);
                f.i8(0).into()
            }
            InferFn::MaxTokens | InferFn::SetMaxTokens => unreachable!("answered above"),
            InferFn::Next => {
                let (ptr, i64t) = (self.ptrt().into(), self.i64t().into());
                let fun = self.infer_locked_fn("next", Some(i64t), &[ptr, i64t, i64t], |f, ps| {
                    let r = f.call_int(f.cx.infer_next_fn(), ps);
                    f.ret(r);
                });
                f.callv(fun, args)
            }
            InferFn::Conversation => {
                let i64t = self.i64t().into();
                let fun = self.infer_locked_fn("conversation", Some(i64t), &[], |f, _| {
                    let at = f.cx.infer_global("jade.infer.conversations", Slot::Word(0));
                    let id = f.add(f.load_i64(at), f.i64(1));
                    f.store(at, id);
                    f.ret(id);
                });
                f.callv(fun, args)
            }
        };
        (v, t)
    }

    /// Purpose: This function gives the plan of the model the program runs.
    fn plan(&self) -> &InferPlan {
        self.infer.as_ref().expect("checked by infer_op")
    }

    /// Purpose: This function returns a zeroed global for the model's state,
    /// making it the first time through [`Cx::state`].
    fn infer_global(&self, name: &str, slot: Slot) -> PointerValue<'ctx> {
        self.state(name, State::aligned(slot)).as_pointer_value()
    }

    fn infer_mutex(&self) -> PointerValue<'ctx> {
        self.infer_global("jade.infer.mutex", Slot::Bytes(64))
    }

    fn infer_ready(&self) -> PointerValue<'ctx> {
        self.infer_global("jade.infer.ready", Slot::Word(0))
    }

    /// Purpose: This function returns one of the per-region arrays: `base`, the
    /// address of each region's byte 0; `map` and `maplen`, the page-aligned
    /// mapping; `delta`, how far `base` is past `map`; `buf`, its Metal buffer.
    fn infer_regions(&self, what: &str) -> PointerValue<'ctx> {
        let n = self.plan().regions.len().max(1) as u32;
        let slot = match what {
            "maplen" | "delta" => Slot::Words(n),
            _ => Slot::Ptrs(n),
        };
        self.infer_global(&format!("jade.infer.{what}"), slot)
    }

    /// Purpose: This function declares an external function.
    fn ext(&self, name: &str, ty: FunctionType<'ctx>) -> FunctionValue<'ctx> {
        self.module
            .get_function(name)
            .unwrap_or_else(|| self.module.add_function(name, ty, Some(Linkage::External)))
    }

    /// Purpose: This function declares the C and Objective-C runtime functions
    /// inference calls that the backend's table does not.
    fn objc_fn(&self, name: &str) -> FunctionValue<'ctx> {
        let (i32t, i64t, ptr) = (self.i32t(), self.i64t(), self.ptrt());
        let void = self.ctx.void_type();
        let ty = match name {
            "objc_msgSend" => void.fn_type(&[], false),
            "sel_registerName" | "objc_getClass" => ptr.fn_type(&[ptr.into()], false),
            "objc_autoreleasePoolPush" | "MTLCreateSystemDefaultDevice" => ptr.fn_type(&[], false),
            "objc_autoreleasePoolPop" => void.fn_type(&[ptr.into()], false),
            // Both take a mode after the flags, which macOS declares variadic.
            "open" | "shm_open" => i32t.fn_type(&[ptr.into(), i32t.into()], true),
            "fstat" => i32t.fn_type(&[i32t.into(), ptr.into()], false),
            "geteuid" => i32t.fn_type(&[], false),
            "mmap" => ptr.fn_type(
                &[ptr.into(), i64t.into(), i32t.into(), i32t.into(), i32t.into(), i64t.into()],
                false,
            ),
            "getpagesize" => i32t.fn_type(&[], false),
            other => panic!("inference declares no function named {other}"),
        };
        self.ext(name, ty)
    }

    /// Purpose: This function returns `jade.infer.init()`, which sets up the
    /// model's lock before the program runs.
    pub fn infer_init_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.init", None, &[], |f| {
            f.call(f.cx.libc("pthread_mutex_init"), &[f.cx.infer_mutex().into(), f.null().into()]);
            f.ret_void();
        })
    }

    /// Purpose: This function returns an entry point that holds the model's
    /// lock around a body.
    ///
    /// Inputs:
    /// - `name`: the entry point, `jade.infer.<name>`.
    /// - `ret`, `params`: its signature.
    /// - `body`: builds the body, given the parameters; it returns the value.
    ///
    /// Outputs: the function. The body must not leave without returning, and
    /// may raise: the error waits in the slot while the lock is given back.
    fn infer_locked_fn(
        &self,
        name: &str,
        ret: Option<BasicTypeEnum<'ctx>>,
        params: &[BasicTypeEnum<'ctx>],
        body: impl FnOnce(&F<'_, 'ctx>, &[BasicValueEnum<'ctx>]),
    ) -> FunctionValue<'ctx> {
        let inner_name = format!("jade.infer.{name}.locked");
        let inner = self.support(&inner_name, ret, params, |f| {
            let ps: Vec<BasicValueEnum> = (0..params.len() as u32).map(|i| f.param(i)).collect();
            body(f, &ps);
        });
        self.support(&format!("jade.infer.{name}"), ret, params, |f| {
            f.call(f.cx.libc("pthread_mutex_lock"), &[f.cx.infer_mutex().into()]);
            let ps: Vec<BasicValueEnum> = (0..params.len() as u32).map(|i| f.param(i)).collect();
            let r = f.call(inner, &ps);
            f.call(f.cx.libc("pthread_mutex_unlock"), &[f.cx.infer_mutex().into()]);
            match r {
                Some(v) => f.ret(v),
                None => f.ret_void(),
            }
        })
    }
}
