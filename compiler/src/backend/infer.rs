//! The backend half of inference: what `src/infer/prelude.jde` asks of
//! `__infer`, written into the program from the model's plan.
//!
//! Five jobs, each a support function:
//!
//! - *The tables.* The tokenizer's pieces, merges and added tokens, as string
//!   constants, and the Unicode classes its splitter asks about.
//! - *Mapping.* `map` opens each region of the model's memory by the name
//!   dovata's record gave at build time, a part file by its path or a shared
//!   memory object with `shm_open`, and maps it. No message goes to dovata:
//!   the names are constants of the program.
//! - *Metal.* `setup` builds a Metal device, compiles `kernels.metal`, makes a
//!   pipeline per kernel and a buffer over each region without copying it. Metal
//!   is an Objective-C API, so every call is `objc_msgSend` with the selector's
//!   own signature; on Apple Silicon a struct of more than 16 bytes, such as
//!   `MTLSize`, goes by pointer.
//! - *A step.* `step` fills the step's inputs, walks the op table and encodes one
//!   dispatch per op into one command buffer, then waits for it.
//! - *Generation.* `generate` runs the prompt's tokens through as many steps as
//!   they take, then picks the likeliest next token, greedily, until a stop
//!   token or the limit.
//!
//! One lock covers the model's memory and the GPU, so two tasks dereferencing
//! prompts take turns: the KV cache holds one conversation at a time. Every
//! entry point takes the lock, calls an inner function that may raise, and
//! gives the lock back.
//!
//! macOS on Apple Silicon only, for now; `aot::compile` refuses a program that
//! runs a model anywhere else before this file is reached.

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};

use super::layout::{ARR_DATA, LEN};
use super::{Cx, F};
use crate::infer::plan::{Addr, InferPlan, KERNELS, MUL_MM, OP_WORDS, threads};
use crate::mir::{InferFn, Ty};

/// The Metal kernels, compiled when the program first runs a prompt.
const KERNELS_SOURCE: &str = include_str!("../infer/kernels.metal");

/// How many tokens a reply runs to unless `set_max_tokens` says otherwise.
const DEFAULT_MAX_TOKENS: i64 = 256;

/// Where macOS's `struct stat` keeps a file's mode, its owner, its modification
/// time, seconds then nanoseconds, and its size. Inference compiles for Apple
/// Silicon only.
const STAT_MODE: i64 = 4;
const STAT_UID: i64 = 16;
const STAT_MTIME_SEC: i64 = 48;
const STAT_MTIME_NSEC: i64 = 56;
const STAT_SIZE: i64 = 96;

/// macOS's open flags: read-only, read-write, write-only, create, append and
/// close-on-exec.
const O_RDONLY: i64 = 0;
const O_RDWR: i64 = 2;
const O_WRONLY: i64 = 1;
const O_CREAT: i64 = 0x200;
const O_APPEND: i64 = 0x8;
const O_CLOEXEC: i64 = 0x0100_0000;

/// The size of the record each kernel takes as buffer 5: five tensors of twelve
/// words, eight floats and eight ints.
const ARGS_BYTES: i64 = 5 * 96 + 32 + 64;

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
        let t = match m {
            InferFn::Data | InferFn::Taint | InferFn::Text => Ty::Str,
            InferFn::Config | InferFn::CharClass | InferFn::MaxTokens | InferFn::SetMaxTokens => {
                Ty::Int
            }
            InferFn::Ready => Ty::Bool,
            InferFn::Map | InferFn::Setup => Ty::Nil,
            InferFn::Generate => Ty::Array(Box::new(Ty::Int)),
        };
        // The reply's length is the one setting a program without a model has.
        let limit = self.infer_global("jade.infer.max_tokens", self.i64t().into());
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
        // `aot::compile` reads the plan before any program that runs a model is
        // built, so this is a compiler bug, said as an error rather than a crash.
        if self.infer.is_none() {
            f.raise(
                "internal compiler error: a program that runs a model was built without its plan",
            );
            return (super::layout::zero(self.llty(&t)), t);
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
            InferFn::Generate => {
                let (ptr, i64t) = (self.ptrt().into(), self.i64t().into());
                let fun = self.infer_locked_fn("generate", Some(ptr), &[ptr, i64t], |f, ps| {
                    let r = f.call_ptr(f.cx.infer_generate_fn(), ps);
                    f.ret(r);
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

    /// Purpose: This function returns a zeroed internal global for the model's
    /// state, making it the first time.
    fn infer_global(&self, name: &str, ty: BasicTypeEnum<'ctx>) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g.as_pointer_value();
        }
        let g = self.module.add_global(ty, None, name);
        g.set_initializer(&super::layout::zero(ty));
        g.set_linkage(Linkage::Internal);
        g.set_alignment(16);
        g.as_pointer_value()
    }

    fn infer_mutex(&self) -> PointerValue<'ctx> {
        self.infer_global("jade.infer.mutex", self.i8t().array_type(64).into())
    }

    fn infer_ready(&self) -> PointerValue<'ctx> {
        self.infer_global("jade.infer.ready", self.i64t().into())
    }

    /// Purpose: This function returns one of the per-region arrays: `base`, the
    /// address of each region's byte 0; `map` and `maplen`, the page-aligned
    /// mapping; `delta`, how far `base` is past `map`; `buf`, its Metal buffer.
    fn infer_regions(&self, what: &str) -> PointerValue<'ctx> {
        let n = self.plan().regions.len().max(1) as u32;
        let elem: BasicTypeEnum = match what {
            "maplen" | "delta" => self.i64t().into(),
            _ => self.ptrt().into(),
        };
        self.infer_global(&format!("jade.infer.{what}"), elem.array_type(n).into())
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

    // ── the tables ───────────────────────────────────────────────────────

    /// Purpose: This function returns an immortal string object for a large
    /// constant, built in one piece rather than byte by byte.
    fn big_lit(&self, name: &str, s: &str) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g.as_pointer_value();
        }
        let bytes = self.ctx.const_string(s.as_bytes(), true);
        let ty = self.ctx.struct_type(
            &[self.i64t().into(), self.i64t().into(), self.i64t().into(), bytes.get_type().into()],
            false,
        );
        let init = ty.const_named_struct(&[
            self.i64t().const_int(u64::MAX, true).into(),
            self.i64t().const_int(s.len() as u64, false).into(),
            self.i64t().const_zero().into(),
            bytes.into(),
        ]);
        let g = self.module.add_global(ty, None, name);
        g.set_initializer(&init);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.set_alignment(8);
        g.as_pointer_value()
    }

    /// Purpose: This function returns `jade.infer.data(k) -> str`: the
    /// tokenizer's pieces, merges or added tokens.
    fn infer_data_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.data", Some(self.ptrt().into()), &[self.i64t().into()], |f| {
            let k = f.param_int(0);
            let p = f.cx.plan();
            let tables = [
                f.cx.big_lit("jade.infer.pieces", &p.pieces),
                f.cx.big_lit("jade.infer.merges", &p.merges),
                f.cx.big_lit("jade.infer.added", &p.added),
            ];
            for (i, t) in tables.iter().enumerate() {
                f.if_(f.eq(k, f.i64(i as i64)), |f| f.ret(*t));
            }
            f.ret(f.cx.lit(""));
        })
    }

    /// Purpose: This function returns a constant table of inclusive ranges.
    fn range_table(&self, name: &str, ranges: &[(u32, u32)]) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g.as_pointer_value();
        }
        let vals: Vec<_> = ranges
            .iter()
            .flat_map(|(a, b)| [*a, *b])
            .map(|x| self.i32t().const_int(x as u64, false))
            .collect();
        let arr = self.i32t().const_array(&vals);
        let g = self.module.add_global(arr.get_type(), None, name);
        g.set_initializer(&arr);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.as_pointer_value()
    }

    /// Purpose: This function returns `jade.infer.in_ranges(table, n, c) ->
    /// bool`: a binary search of a range table.
    fn in_ranges_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t, i32t) = (self.ptrt(), self.i64t(), self.i32t());
        self.support(
            "jade.infer.in_ranges",
            Some(self.i1t().into()),
            &[ptr.into(), i64t.into(), i32t.into()],
            |f| {
                let (t, n) = (f.param_ptr(0), f.param_int(1));
                let c = f.param_int(2);
                let lo = f.var(f.i64(0));
                let hi = f.var(n);
                f.while_(
                    |f| f.slt(f.load_i64(lo), f.load_i64(hi)),
                    |f, _| {
                        let (l, h) = (f.load_i64(lo), f.load_i64(hi));
                        let mid = f.sdiv(f.add(l, h), f.i64(2));
                        let a = f.load_i32(f.elem(i32t, t, f.mul(mid, f.i64(2))));
                        let b = f.load_i32(f.elem(i32t, t, f.add(f.mul(mid, f.i64(2)), f.i64(1))));
                        f.if_else(
                            f.ult(c, a),
                            |f| f.store(hi, mid),
                            |f| {
                                f.if_else(
                                    f.icmp(inkwell::IntPredicate::UGT, c, b),
                                    |f| f.store(lo, f.add(mid, f.i64(1))),
                                    |f| f.ret(f.bool(true)),
                                );
                            },
                        );
                    },
                );
                f.ret(f.bool(false));
            },
        )
    }

    /// Purpose: This function returns `jade.infer.char_class(c) -> int`: 1 for a
    /// letter, 2 a number, 3 white space, else 0.
    fn infer_char_class_fn(&self) -> FunctionValue<'ctx> {
        use crate::infer::unicode::{LETTERS, NUMBERS, SPACES};
        self.support(
            "jade.infer.char_class",
            Some(self.i64t().into()),
            &[self.i32t().into()],
            |f| {
                // A tainted char carries its taint in bit 31.
                let c = f.and(f.param_int(0), f.cx.i32t().const_int(0x1F_FFFF, false));
                let tables = [
                    (1, f.cx.range_table("jade.infer.letters", LETTERS), LETTERS.len()),
                    (2, f.cx.range_table("jade.infer.numbers", NUMBERS), NUMBERS.len()),
                    (3, f.cx.range_table("jade.infer.spaces", SPACES), SPACES.len()),
                ];
                for (class, t, n) in tables {
                    let hit = f.call_int(
                        f.cx.in_ranges_fn(),
                        &[t.into(), f.i64(n as i64).into(), c.into()],
                    );
                    f.if_(hit, |f| f.ret(f.i64(class)));
                }
                f.ret(f.i64(0));
            },
        )
    }

    /// Purpose: This function returns `jade.infer.taint(s) -> str`: a copy of
    /// the text marked as coming from outside the program.
    fn infer_taint_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.taint", Some(self.ptrt().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let len = f.str_len(s);
            let t = f.str_new(len);
            f.memcpy(f.str_data(t), f.str_data(s), len);
            f.set_trust(t, f.i64(1));
            f.ret(t);
        })
    }

    // ── mapping the model's memory ───────────────────────────────────────

    /// Purpose: This function returns the body of `__infer.map()`, called with
    /// the lock held: it opens and maps every region of the model's memory, a
    /// region already mapped staying as it is, then registers the program with
    /// dovata, so `dovata status` lists it.
    fn infer_map_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.map.body", None, &[], |f| {
            let cx = f.cx;
            let (i64t, i8t, ptr) = (cx.i64t(), cx.i8t(), cx.ptrt());
            let plan = cx.plan();
            let model = &plan.model;
            // `struct stat` is 144 bytes on macOS; eight-byte words keep its fields aligned.
            let st = f.alloca(i64t.array_type(32));
            let page = f.sext(f.call_int(cx.objc_fn("getpagesize"), &[]), i64t);
            for (id, r) in plan.regions.iter().enumerate() {
                let base = f.elem(ptr, cx.infer_regions("base"), f.i64(id as i64));
                f.if_(f.is_null(f.load_ptr(base)), |f| {
                    let name = cx.cstr(&r.name);
                    let fd = if r.shared {
                        let flags = if r.writable { O_RDWR } else { O_RDONLY };
                        f.call_int(
                            cx.objc_fn("shm_open"),
                            &[name.into(), f.i32(flags).into(), f.i32(0).into()],
                        )
                    } else {
                        f.call_int(
                            cx.objc_fn("open"),
                            &[name.into(), f.i32(O_RDONLY | O_CLOEXEC).into(), f.i32(0).into()],
                        )
                    };
                    f.fail_if(
                        f.slt(fd, f.i32(0)),
                        &if r.shared {
                            format!(
                                "dovata does not hold the memory this program was built for \
                                 ({}): add the model {model} to dovata with the options the \
                                 program was built against, or build the program again",
                                r.name
                            )
                        } else {
                            format!("cannot open the model file {}", r.name)
                        },
                    );
                    let stat_ok = f.call_int(cx.objc_fn("fstat"), &[fd.into(), st.into()]);
                    let size = f.load_i64(f.at_c(st, STAT_SIZE));
                    let bad = if r.shared {
                        // Names are shared by every user of the machine, so another
                        // user could have made one before dovata did: the object must
                        // be this user's, and open to no one else.
                        let me = f.call_int(cx.objc_fn("geteuid"), &[]);
                        let owner = f.load_i32(f.at_c(st, STAT_UID));
                        let others = f.and(f.load_i32(f.at_c(st, STAT_MODE)), f.i32(0o077));
                        let theirs = f.or(f.ne(owner, me), f.ne(others, f.i32(0)));
                        let short = f.slt(size, f.i64(r.offset + r.bytes));
                        f.or(f.ne(stat_ok, f.i32(0)), f.or(short, theirs))
                    } else {
                        let sec = f.load_i64(f.at_c(st, STAT_MTIME_SEC));
                        let nsec = f.load_i64(f.at_c(st, STAT_MTIME_NSEC));
                        let same = f.and(
                            f.eq(size, f.i64(r.file_bytes)),
                            f.and(f.eq(sec, f.i64(r.modified_sec)), f.eq(nsec, f.i64(r.modified_nsec))),
                        );
                        f.or(f.ne(stat_ok, f.i32(0)), f.not(same))
                    };
                    f.if_(bad, |f| {
                        f.call(cx.libc("close"), &[fd.into()]);
                        f.fail(&if r.shared {
                            format!(
                                "dovata's memory for {model} ({}) is not the size this program \
                                 was built for, or does not belong to this user alone; add the \
                                 model to dovata again and build the program again",
                                r.name
                            )
                        } else {
                            format!(
                                "{} changed on disk after dovata loaded it; remove the model with \
                                 `dovata remove {model}`, add it again, and build the program again",
                                r.name
                            )
                        });
                    });
                    // A mapping starts on a page; the region's base is as far past
                    // that as its offset is past a page.
                    let offset = f.i64(r.offset);
                    let start = f.sub(offset, f.srem(offset, page));
                    let delta = f.sub(offset, start);
                    let len = f.add(delta, f.i64(r.bytes));
                    let len = f.mul(f.sdiv(f.add(len, f.sub(page, f.i64(1))), page), page);
                    let prot = if r.writable { 3 } else { 1 };
                    let p = f.call_ptr(
                        cx.objc_fn("mmap"),
                        &[
                            f.null().into(),
                            len.into(),
                            f.i32(prot).into(),
                            f.i32(1).into(),
                            fd.into(),
                            start.into(),
                        ],
                    );
                    // The mapping keeps the file or object; the descriptor is not needed.
                    f.call(cx.libc("close"), &[fd.into()]);
                    let failed =
                        f.eq(f.b.build_ptr_to_int(p, i64t, "").expect("positioned"), f.i64(-1));
                    f.fail_if(failed, &format!("could not map the model's memory ({})", r.name));
                    f.store(f.elem(ptr, cx.infer_regions("map"), f.i64(id as i64)), p);
                    f.store(f.elem(i64t, cx.infer_regions("maplen"), f.i64(id as i64)), len);
                    f.store(f.elem(i64t, cx.infer_regions("delta"), f.i64(id as i64)), delta);
                    f.store(base, f.at(p, delta));
                });
            }
            // An empty file named for this process, in the directory dovata
            // lists a model's programs from. A program that cannot make it
            // still runs.
            let path = f.alloca(i8t.array_type(1024));
            let pid = f.call_int(cx.libc("getpid"), &[]);
            let n = f.call_int(
                cx.libc("snprintf"),
                &[path.into(), f.i64(1024).into(), cx.cstr("%s/%d").into(), cx.cstr(&plan.clients).into(), pid.into()],
            );
            f.if_(f.and(f.sgt(n, f.i32(0)), f.slt(n, f.i32(1024))), |f| {
                let flags = O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC;
                let fd = f.call_int(
                    cx.objc_fn("open"),
                    &[path.into(), f.i32(flags).into(), f.i32(0o600).into()],
                );
                f.if_(f.sge(fd, f.i32(0)), |f| {
                    f.call(cx.libc("close"), &[fd.into()]);
                });
            });
            f.ret_void();
        })
    }

    // ── Metal ────────────────────────────────────────────────────────────

    /// Purpose: This function returns a selector, registered once and kept.
    fn sel(&self, f: &F<'_, 'ctx>, name: &str) -> PointerValue<'ctx> {
        let g = self.infer_global(&format!("jade.infer.sel.{name}"), self.ptrt().into());
        let v = f.var(f.load_ptr(g));
        f.if_(f.is_null(f.load_ptr(v)), |f| {
            let s = f.call_ptr(self.objc_fn("sel_registerName"), &[self.cstr(name).into()]);
            f.store(g, s);
            f.store(v, s);
        });
        f.load_ptr(v)
    }

    /// Purpose: This function sends an Objective-C message.
    ///
    /// Inputs:
    /// - `f`: the function being written.
    /// - `ret`: the method's result type, or `None` for void.
    /// - `recv`: the receiver.
    /// - `sel`: the selector's name.
    /// - `args`: the arguments after the receiver and selector.
    ///
    /// Outputs: the result, if the method has one.
    fn send(
        &self,
        f: &F<'_, 'ctx>,
        ret: Option<BasicTypeEnum<'ctx>>,
        recv: PointerValue<'ctx>,
        sel: &str,
        args: &[BasicValueEnum<'ctx>],
    ) -> Option<BasicValueEnum<'ctx>> {
        let s = self.sel(f, sel);
        let mut ps: Vec<BasicMetadataTypeEnum> = vec![self.ptrt().into(), self.ptrt().into()];
        ps.extend(args.iter().map(|a| BasicMetadataTypeEnum::from(a.get_type())));
        let ty = match ret {
            Some(r) => r.fn_type(&ps, false),
            None => self.ctx.void_type().fn_type(&ps, false),
        };
        let mut av: Vec<BasicMetadataValueEnum> = vec![recv.into(), s.into()];
        av.extend(args.iter().map(|a| BasicMetadataValueEnum::from(*a)));
        let target = self.objc_fn("objc_msgSend").as_global_value().as_pointer_value();
        let cs = f.b.build_indirect_call(ty, target, &av, "").expect("positioned");
        match cs.try_as_basic_value() {
            inkwell::values::ValueKind::Basic(v) => Some(v),
            inkwell::values::ValueKind::Instruction(_) => None,
        }
    }

    /// Purpose: This function sends a message whose result is an object.
    fn send_ptr(
        &self,
        f: &F<'_, 'ctx>,
        recv: PointerValue<'ctx>,
        sel: &str,
        args: &[BasicValueEnum<'ctx>],
    ) -> PointerValue<'ctx> {
        self.send(f, Some(self.ptrt().into()), recv, sel, args)
            .expect("an object result")
            .into_pointer_value()
    }

    /// Purpose: This function makes an `NSString` from C text.
    fn nsstring(&self, f: &F<'_, 'ctx>, text: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let class = f.call_ptr(self.objc_fn("objc_getClass"), &[self.cstr("NSString").into()]);
        self.send_ptr(f, class, "stringWithUTF8String:", &[text.into()])
    }

    /// Purpose: This function fails with a message and an `NSError`'s
    /// description.
    fn fail_ns(&self, f: &F<'_, 'ctx>, what: &str, err: PointerValue<'ctx>) {
        let text = f.var(self.cstr("no reason given"));
        f.if_(f.not(f.is_null(err)), |f| {
            let d = self.send_ptr(f, err, "localizedDescription", &[]);
            let c = self.send_ptr(f, d, "UTF8String", &[]);
            f.store(text, c);
        });
        let c = f.load_ptr(text);
        let n = f.call_int(self.libc("strlen"), &[c.into()]);
        let reason = f.call_ptr(self.str_from_fn(), &[c.into(), n.into()]);
        let msg = f.str_cat(&[self.lit(what), reason]);
        f.release(&Ty::Str, reason.into());
        f.fail_str(msg);
    }

    fn infer_obj(&self, name: &str) -> PointerValue<'ctx> {
        self.infer_global(&format!("jade.infer.{name}"), self.ptrt().into())
    }

    fn infer_pipelines(&self) -> PointerValue<'ctx> {
        self.infer_global(
            "jade.infer.pipelines",
            self.ptrt().array_type(KERNELS.len() as u32).into(),
        )
    }

    /// Purpose: This function returns the body of `__infer.setup()`, called
    /// with the lock held inside an autorelease pool: the device, the queue,
    /// the kernels, and a buffer over each region.
    fn infer_setup_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.setup.body", None, &[], |f| {
            let cx = f.cx;
            let ptr = cx.ptrt();
            let i64t = cx.i64t();
            f.if_(f.ne(f.load_i64(cx.infer_ready()), f.i64(0)), |f| f.ret_void());
            let dev = f.call_ptr(cx.objc_fn("MTLCreateSystemDefaultDevice"), &[]);
            f.fail_if(f.is_null(dev), "this machine has no Metal device to run the model on");
            let queue = cx.send_ptr(f, dev, "newCommandQueue", &[]);
            let err = f.var(f.null());
            let src = cx.nsstring(f, cx.cstr(KERNELS_SOURCE));
            let lib = cx.send_ptr(
                f,
                dev,
                "newLibraryWithSource:options:error:",
                &[src.into(), f.null().into(), err.into()],
            );
            f.if_(f.is_null(lib), |f| {
                let e = f.load_ptr(err);
                cx.fail_ns(f, "the GPU kernels did not compile: ", e);
            });
            let pipes = cx.infer_pipelines();
            for (k, name) in KERNELS.iter().enumerate() {
                let n = cx.nsstring(f, cx.cstr(name));
                let func = cx.send_ptr(f, lib, "newFunctionWithName:", &[n.into()]);
                f.fail_if(f.is_null(func), "a GPU kernel is missing from the compiled library");
                let p = cx.send_ptr(
                    f,
                    dev,
                    "newComputePipelineStateWithFunction:error:",
                    &[func.into(), err.into()],
                );
                f.if_(f.is_null(p), |f| {
                    let e = f.load_ptr(err);
                    cx.fail_ns(f, "a GPU kernel could not be prepared: ", e);
                });
                f.store(f.elem(ptr, pipes, f.i64(k as i64)), p);
            }
            let n = cx.plan().regions.len() as i64;
            f.for_range(f.i64(0), f.i64(n), |f, r, _| {
                let map = f.load_ptr(f.elem(ptr, cx.infer_regions("map"), r));
                f.fail_if(f.is_null(map), "a region of the model was not mapped before setup");
                let len = f.load_i64(f.elem(i64t, cx.infer_regions("maplen"), r));
                let buf = cx.send_ptr(
                    f,
                    dev,
                    "newBufferWithBytesNoCopy:length:options:deallocator:",
                    &[map.into(), len.into(), f.i64(0).into(), f.null().into()],
                );
                f.fail_if(f.is_null(buf), "Metal would not take the model's memory as a buffer");
                f.store(f.elem(ptr, cx.infer_regions("buf"), r), buf);
            });
            f.store(cx.infer_obj("device"), dev);
            f.store(cx.infer_obj("queue"), queue);
            f.store(cx.infer_ready(), f.i64(1));
            f.ret_void();
        })
    }

    // ── a step ───────────────────────────────────────────────────────────

    /// Purpose: This function returns the address of a tensor on the host.
    fn host_addr(&self, f: &F<'_, 'ctx>, a: Addr) -> PointerValue<'ctx> {
        let base = f.load_ptr(f.elem(self.ptrt(), self.infer_regions("base"), f.i64(a.region)));
        f.at(base, f.i64(a.offset))
    }

    /// Purpose: This function returns the op table, a constant array of
    /// [`OP_WORDS`] words per op.
    fn op_table(&self) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global("jade.infer.ops") {
            return g.as_pointer_value();
        }
        let vals: Vec<_> = self
            .plan()
            .ops
            .iter()
            .flat_map(|r| r.iter())
            .map(|w| self.i64t().const_int(*w as u64, true))
            .collect();
        let arr = self.i64t().const_array(&vals);
        let g = self.module.add_global(arr.get_type(), None, "jade.infer.ops");
        g.set_initializer(&arr);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.as_pointer_value()
    }

    /// Purpose: This function returns `jade.infer.step(tokens, n, pos)`: run
    /// one forward step over `n` tokens, whose ids are `tokens`, an array of
    /// ints, at positions `pos` onward.
    fn infer_step_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support("jade.infer.step", None, &[ptr.into(), i64t.into(), i64t.into()], |f| {
            let cx = f.cx;
            let p = cx.plan().clone();
            let (i32t, i16t) = (cx.i32t(), cx.ctx.i16_type());
            let (tokens, n, pos) = (f.param_ptr(0), f.param_int(1), f.param_int(2));
            // The inputs: ids, positions, the cache cell each token writes, and
            // a mask that lets each token see itself and what came before it.
            let (tp, pp, kp, vp) = (
                cx.host_addr(f, p.tokens),
                cx.host_addr(f, p.positions),
                cx.host_addr(f, p.k_cells),
                cx.host_addr(f, p.v_cells),
            );
            let mp = cx.host_addr(f, p.mask);
            f.for_range(f.i64(0), n, |f, k, _| {
                let id = f.load_i64(f.elem(i64t, tokens, k));
                f.store(f.elem(i32t, tp, k), f.trunc(id, i32t));
                let at = f.add(pos, k);
                f.store(f.elem(i32t, pp, k), f.trunc(at, i32t));
                f.store(f.elem(i64t, kp, k), at);
                f.store(f.elem(i64t, vp, k), at);
                let row = f.at(mp, f.mul(k, f.i64(p.n_ctx * 2)));
                f.for_range(f.i64(0), f.i64(p.n_ctx), |f, j, _| {
                    let seen = f.sle(j, at);
                    // f16 zero, or f16 minus infinity.
                    let bits = f.select_int(seen, i16t.const_zero(), i16t.const_int(0xFC00, false));
                    f.store(f.elem(i16t, row, j), bits);
                });
            });
            let oi = cx.host_addr(f, p.out_ids);
            f.store(oi, f.trunc(f.sub(n, f.i64(1)), i32t));

            // One command buffer: every op, in order.
            let queue = f.load_ptr(cx.infer_obj("queue"));
            let cb = cx.send_ptr(f, queue, "commandBuffer", &[]);
            let enc = cx.send_ptr(f, cb, "computeCommandEncoder", &[]);
            let args = f.alloca(cx.i8t().array_type(ARGS_BYTES as u32));
            let groups = f.alloca(i64t.array_type(3));
            let width = f.alloca(i64t.array_type(3));
            f.store(width, f.i64(64));
            f.store(f.at_c(width, 8), f.i64(1));
            f.store(f.at_c(width, 16), f.i64(1));
            let table = cx.op_table();
            let pipes = cx.infer_pipelines();
            let bufs = cx.infer_regions("buf");
            let deltas = cx.infer_regions("delta");
            f.for_range(f.i64(0), f.i64(p.ops.len() as i64), |f, op, _| {
                let rec = f.elem(i64t, table, f.mul(op, f.i64(OP_WORDS as i64)));
                let word = |f: &F<'_, 'ctx>, i: i64| f.load_i64(f.elem(i64t, rec, f.i64(i)));
                // Each tensor's shape, with the real token count where the plan
                // has the step's.
                for slot in 0..5i64 {
                    let b = 1 + slot * 12;
                    let axis = word(f, b + 11);
                    for d in 0..4i64 {
                        let ne = word(f, b + 2 + d);
                        let ne = f.select_int(f.eq(axis, f.i64(d)), n, ne);
                        f.store(f.at_c(args, slot * 96 + d * 8), ne);
                        f.store(f.at_c(args, slot * 96 + 32 + d * 8), word(f, b + 6 + d));
                    }
                    f.store(f.at_c(args, slot * 96 + 64), word(f, b + 10));
                }
                for k in 0..4i64 {
                    f.store(f.at_c(args, 480 + k * 8), word(f, 61 + k));
                }
                for k in 0..8i64 {
                    f.store(f.at_c(args, 512 + k * 8), word(f, 65 + k));
                }
                // The cache cells that hold tokens once this step writes its own.
                f.store(f.at_c(args, 512 + 6 * 8), f.add(pos, n));
                let ne = |f: &F<'_, 'ctx>, slot: i64, d: i64| {
                    f.load_i64(f.at_c(args, slot * 96 + d * 8))
                };
                let all = |f: &F<'_, 'ctx>, slot: i64| {
                    let a = f.mul(ne(f, slot, 0), ne(f, slot, 1));
                    f.mul(a, f.mul(ne(f, slot, 2), ne(f, slot, 3)))
                };
                let rule = word(f, 73);
                let count = f.var(all(f, 4));
                f.if_(f.eq(rule, f.i64(threads::DST_ROWS)), |f| {
                    f.store(count, f.mul(ne(f, 4, 1), f.mul(ne(f, 4, 2), ne(f, 4, 3))));
                });
                f.if_(f.eq(rule, f.i64(threads::SRC1)), |f| f.store(count, all(f, 1)));
                f.if_(f.eq(rule, f.i64(threads::ATTENTION)), |f| {
                    f.store(count, f.mul(ne(f, 0, 1), ne(f, 0, 2)));
                });
                // A mul_mat over more than one token runs as tiles.
                let tiled = f.and(f.eq(rule, f.i64(threads::MAT)), f.sgt(ne(f, 4, 1), f.i64(1)));
                f.if_(tiled, |f| {
                    let tile = f.i64(threads::TILE);
                    let rows = f.sdiv(f.add(ne(f, 4, 0), f.i64(threads::TILE - 1)), tile);
                    let cols = f.sdiv(f.add(ne(f, 4, 1), f.i64(threads::TILE - 1)), tile);
                    f.store(count, f.mul(f.mul(rows, cols), f.mul(ne(f, 4, 2), ne(f, 4, 3))));
                });
                let count = f.load_i64(count);
                f.store(f.at_c(args, 512 + 7 * 8), count);
                let per_item = f.select_int(tiled, f.i64(threads::TILE_THREADS), word(f, 74));
                let threads = f.mul(count, per_item);

                let kernel = f.select_int(tiled, f.i64(MUL_MM), word(f, 0));
                let pipe = f.load_ptr(f.elem(cx.ptrt(), pipes, kernel));
                cx.send(f, None, enc, "setComputePipelineState:", &[pipe.into()]);
                for slot in 0..5i64 {
                    let b = 1 + slot * 12;
                    let region = word(f, b);
                    f.if_(f.sge(region, f.i64(0)), |f| {
                        let buf = f.load_ptr(f.elem(cx.ptrt(), bufs, region));
                        let delta = f.load_i64(f.elem(i64t, deltas, region));
                        let offset = f.add(delta, word(f, b + 1));
                        cx.send(
                            f,
                            None,
                            enc,
                            "setBuffer:offset:atIndex:",
                            &[buf.into(), offset.into(), f.i64(slot).into()],
                        );
                    });
                }
                cx.send(
                    f,
                    None,
                    enc,
                    "setBytes:length:atIndex:",
                    &[args.into(), f.i64(ARGS_BYTES).into(), f.i64(5).into()],
                );
                f.if_(f.sgt(threads, f.i64(0)), |f| {
                    f.store(groups, f.sdiv(f.add(threads, f.i64(63)), f.i64(64)));
                    f.store(f.at_c(groups, 8), f.i64(1));
                    f.store(f.at_c(groups, 16), f.i64(1));
                    // MTLSize is 24 bytes, so Apple Silicon passes it by pointer.
                    cx.send(
                        f,
                        None,
                        enc,
                        "dispatchThreadgroups:threadsPerThreadgroup:",
                        &[groups.into(), width.into()],
                    );
                });
            });
            cx.send(f, None, enc, "endEncoding", &[]);
            cx.send(f, None, cb, "commit", &[]);
            cx.send(f, None, cb, "waitUntilCompleted", &[]);
            let status = cx
                .send(f, Some(i64t.into()), cb, "status", &[])
                .expect("a status")
                .into_int_value();
            // MTLCommandBufferStatusCompleted.
            f.if_(f.ne(status, f.i64(4)), |f| {
                let e = cx.send_ptr(f, cb, "error", &[]);
                cx.fail_ns(f, "the GPU could not run the model's step: ", e);
            });
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.infer.argmax() -> int`: the token
    /// the last step's logits favor.
    fn infer_argmax_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.argmax", Some(self.i64t().into()), &[], |f| {
            let cx = f.cx;
            let p = cx.plan().clone();
            let f32t = cx.ctx.f32_type();
            let logits = cx.host_addr(f, p.logits);
            let best = f.var(f32t.const_float(f64::NEG_INFINITY));
            let at = f.var(f.i64(0));
            f.for_range(f.i64(0), f.i64(p.n_vocab), |f, i, _| {
                let x = f.load(f32t, f.elem(f32t, logits, i)).into_float_value();
                let b = f.load(f32t, best).into_float_value();
                f.if_(f.fcmp(inkwell::FloatPredicate::OGT, x, b), |f| {
                    f.store(best, x);
                    f.store(at, i);
                });
            });
            f.ret(f.load_i64(at));
        })
    }

    /// Purpose: This function returns the body of `__infer.generate(ids,
    /// max)`, called with the lock held inside an autorelease pool.
    fn infer_generate_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        self.support("jade.infer.generate.body", Some(ptr.into()), &[ptr.into(), i64t.into()], |f| {
            let cx = f.cx;
            let p = cx.plan().clone();
            let (ids, max) = (f.param_ptr(0), f.param_int(1));
            let out = f.array_new(&Ty::Int, f.i64(16));
            let n = f.load_i64(f.at_c(ids, LEN));
            f.if_(f.eq(n, f.i64(0)), |f| f.ret(out));
            f.if_(f.eq(f.load_i64(cx.infer_ready()), f.i64(0)), |f| {
                f.release(&Ty::Array(Box::new(Ty::Int)), out.into());
                f.fail("the model was not set up before a prompt ran");
            });
            f.if_(f.sge(n, f.i64(p.n_ctx)), |f| {
                f.release(&Ty::Array(Box::new(Ty::Int)), out.into());
                f.fail("the prompt is longer than the model's context; dovata plans the context's length");
            });
            let data = f.load_ptr(f.at_c(ids, ARR_DATA));
            let pos = f.var(f.i64(0));
            // The prompt, as many tokens a step as the plan allows.
            f.while_(
                |f| f.slt(f.load_i64(pos), n),
                |f, _| {
                    let at = f.load_i64(pos);
                    let left = f.sub(n, at);
                    let cnt = f.select_int(f.sgt(left, f.i64(p.n_tokens)), f.i64(p.n_tokens), left);
                    let from = f.elem(i64t, data, at);
                    f.call(cx.infer_step_fn(), &[from.into(), cnt.into(), at.into()]);
                    f.if_(f.err_pending(), |f| {
                        f.release(&Ty::Array(Box::new(Ty::Int)), out.into());
                        f.ret(f.null());
                    });
                    f.store(pos, f.add(at, cnt));
                },
            );
            // Then one token at a time, the likeliest each time.
            let next = f.var(f.call_int(cx.infer_argmax_fn(), &[]));
            let tok = f.alloca(i64t);
            f.while_(
                |f| {
                    let len = f.load_i64(f.at_c(out, LEN));
                    let room = f.and(f.slt(len, max), f.slt(f.load_i64(pos), f.i64(p.n_ctx)));
                    let nx = f.load_i64(next);
                    let mut stop = f.bool(false);
                    for s in &p.stops {
                        stop = f.or(stop, f.eq(nx, f.i64(*s)));
                    }
                    f.and(room, f.not(stop))
                },
                |f, _| {
                    let nx = f.load_i64(next);
                    f.array_push_owned(&Ty::Int, out, nx.into());
                    f.store(tok, nx);
                    let at = f.load_i64(pos);
                    f.call(cx.infer_step_fn(), &[tok.into(), f.i64(1).into(), at.into()]);
                    f.if_(f.err_pending(), |f| {
                        f.release(&Ty::Array(Box::new(Ty::Int)), out.into());
                        f.ret(f.null());
                    });
                    f.store(pos, f.add(at, f.i64(1)));
                    f.store(next, f.call_int(cx.infer_argmax_fn(), &[]));
                },
            );
            f.ret(out);
        })
    }
}
