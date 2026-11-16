//! Codegen: MIR → LLVM IR → object files, which `src/link/` joins with the C
//! library into a binary.
//!
//! This directory writes LLVM IR. LLVM optimizes it and turns it into machine
//! code, written as an object file per module.
//!
//! Every value has a type the compiler knows, so an int is an `i64` in a
//! register, a float an `f64`, and a string, array, dict or struct a pointer to
//! a reference-counted object whose layout codegen decides. Nothing in a
//! compiled program reads a type tag, and nothing links a runtime library.
//!
//! ## Support code
//!
//! What v1 called the runtime is emitted here, into each program, as internal
//! LLVM functions named `jade.*`. A function is generated the first time
//! something asks for it ([`Cx::support`]), so a program that never splits a
//! string carries no split. Code that depends on a type, such as releasing an
//! array or printing a dict, is generated once per type it is used with. The
//! C library is the only dependency: `malloc`, `write`, `snprintf` and a few
//! more, which macOS provides as libSystem and Linux as libc.
//!
//! ## Object layouts
//!
//! Every heap object starts with an `i64` reference count. A count below zero
//! marks an immortal object, such as a string literal, which is never retained,
//! released or freed.
//!
//! ```text
//! str     { i64 rc, i64 len, i64 trust, u8 data[len], u8 0 }
//! bytes   the same as str
//! array   { i64 rc, i64 len, i64 cap, ptr data }          data: cap elements
//! dict    { i64 rc, i64 len, i64 cap, ptr keys, ptr vals, ptr index, i64 icap }
//! struct  { i64 rc, i64 type_id, fields... }
//! T?      a null pointer for nil when T is a pointer, else { i1 present, T }
//! ```
//!
//! ## Files
//!
//! `fb.rs` is the function-building layer the rest is written in. `layout.rs`
//! maps types to LLVM types, `func.rs` lowers MIR functions, `rc.rs` emits
//! retain and release, and `errors.rs` raised errors. `start.rs` is the
//! start-up code a Linux program begins at. The rest is split by what it
//! serves:
//!
//! - `support/`: the support code for values, one file per kind: strings,
//!   bytes, arrays and dicts, comparison, printing, sized numbers, `toDict`
//!   and tasks.
//! - `sys/`: the `__sys` hooks the std library written in Jade calls.
//! - `native.rs`: calls into Rust packages.
//! - `infer.rs`: the model's tables and Metal kernels, for `?p`.
//!
//! ## The library cache
//!
//! A binary build compiles each std library instance whose types are built in
//! once per machine and keeps its object (`libcache/`); `libcache/plan.rs`
//! decides which, and `shared.rs` makes the program define, once, the state
//! and the class-dependent functions those objects use. `--emit ir`, and a
//! build given no cache root, put everything in one module.

mod errors;
mod fb;
mod func;
mod infer;
mod layout;
mod libcache;
mod native;
mod rc;
mod shared;
mod start;
mod support;
mod sys;

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FloatType, IntType, PointerType,
};
use inkwell::values::{FunctionValue, GlobalValue, PointerValue};
use inkwell::{AddressSpace, OptimizationLevel};

pub use fb::F;
pub use libcache::{LibObject, Report, default_root as default_lib_cache};

use crate::mir;

/// Purpose: This struct holds the module being built and everything its
/// functions share.
pub struct Cx<'ctx> {
    pub ctx: &'ctx Context,
    pub module: Module<'ctx>,
    /// The program's struct types, in type-id order.
    pub structs: Vec<mir::StructDef>,
    struct_ids: HashMap<String, u32>,
    struct_tys: RefCell<HashMap<String, inkwell::types::StructType<'ctx>>>,
    /// Support functions emitted so far, by name.
    support: RefCell<HashMap<String, FunctionValue<'ctx>>>,
    /// String literal objects emitted so far, by content.
    lits: RefCell<HashMap<String, PointerValue<'ctx>>>,
    /// Each class's destructor chain, as the functions to call.
    pub drops: HashMap<String, Vec<FunctionValue<'ctx>>>,
    /// The model the program runs, when it dereferences a prompt.
    pub infer: Option<crate::infer::plan::InferPlan>,
    /// Which kind of module this is: the program's, or a cached library
    /// instance's; see `shared.rs`.
    role: shared::Role,
    /// In a library module, each shared name it declared, for the program's
    /// module to define.
    refs: RefCell<BTreeMap<String, shared::Shared>>,
    /// In a library module, whether anything asked for what only the program
    /// knows, such as a class's layout.
    dependent: Cell<bool>,
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function makes an empty module for a program.
    ///
    /// Inputs:
    /// - `ctx`: the LLVM context.
    /// - `prog`: the program, for its struct types.
    /// - `role`: the program built whole, or the program's module beside
    ///   library objects, with what they need of it.
    ///
    /// Outputs: the module context.
    fn new(ctx: &'ctx Context, prog: &mir::Program, role: shared::Role) -> Self {
        let struct_ids =
            prog.structs.iter().enumerate().map(|(i, s)| (s.name.clone(), i as u32)).collect();
        Cx {
            ctx,
            module: ctx.create_module("jade_program"),
            structs: prog.structs.clone(),
            struct_ids,
            struct_tys: RefCell::new(HashMap::new()),
            support: RefCell::new(HashMap::new()),
            lits: RefCell::new(HashMap::new()),
            drops: HashMap::new(),
            infer: prog.infer.clone(),
            role,
            refs: RefCell::new(BTreeMap::new()),
            dependent: Cell::new(false),
        }
    }

    /// Purpose: This function makes an empty module for one library instance.
    /// It knows none of the program's classes, so nothing it builds can
    /// depend on them without being caught.
    ///
    /// Inputs:
    /// - `ctx`: the LLVM context, the module's alone.
    /// - `name`: the module's name.
    ///
    /// Outputs: the module context.
    fn library(ctx: &'ctx Context, name: &str) -> Self {
        Cx {
            ctx,
            module: ctx.create_module(name),
            structs: Vec::new(),
            struct_ids: HashMap::new(),
            struct_tys: RefCell::new(HashMap::new()),
            support: RefCell::new(HashMap::new()),
            lits: RefCell::new(HashMap::new()),
            drops: HashMap::new(),
            infer: None,
            role: shared::Role::Library,
            refs: RefCell::new(BTreeMap::new()),
            dependent: Cell::new(false),
        }
    }

    pub fn i64t(&self) -> IntType<'ctx> {
        self.ctx.i64_type()
    }

    pub fn i32t(&self) -> IntType<'ctx> {
        self.ctx.i32_type()
    }

    pub fn i8t(&self) -> IntType<'ctx> {
        self.ctx.i8_type()
    }

    pub fn i16t(&self) -> IntType<'ctx> {
        self.ctx.i16_type()
    }

    pub fn f32t(&self) -> inkwell::types::FloatType<'ctx> {
        self.ctx.f32_type()
    }

    pub fn i1t(&self) -> IntType<'ctx> {
        self.ctx.bool_type()
    }

    pub fn f64t(&self) -> FloatType<'ctx> {
        self.ctx.f64_type()
    }

    pub fn ptrt(&self) -> PointerType<'ctx> {
        self.ctx.ptr_type(AddressSpace::default())
    }

    /// Purpose: This function returns a support function, building it the
    /// first time anything asks for it.
    ///
    /// Inputs:
    /// - `name`: the function's name, which also identifies it.
    /// - `ret`: its return type, or `None` for void.
    /// - `params`: its parameter types.
    /// - `build`: builds its body.
    ///
    /// Outputs: the function. It is internal, so the optimizer may inline it
    /// and drops it when nothing calls it.
    pub fn support(
        &self,
        name: &str,
        ret: Option<BasicTypeEnum<'ctx>>,
        params: &[BasicTypeEnum<'ctx>],
        build: impl FnOnce(&F<'_, 'ctx>),
    ) -> FunctionValue<'ctx> {
        if let Some(f) = self.support.borrow().get(name) {
            return *f;
        }
        self.define_fn(name, ret, params, Linkage::Internal, build)
    }

    /// Purpose: This function adds a support function with a linkage and
    /// builds its body. It is recorded before its body is built, so a body
    /// that asks for itself gets itself.
    ///
    /// Inputs:
    /// - `name`, `ret`, `params`, `build`: as for [`Cx::support`].
    /// - `linkage`: internal for a private copy, external for a function a
    ///   library object calls.
    ///
    /// Outputs: the function.
    fn define_fn(
        &self,
        name: &str,
        ret: Option<BasicTypeEnum<'ctx>>,
        params: &[BasicTypeEnum<'ctx>],
        linkage: Linkage,
        build: impl FnOnce(&F<'_, 'ctx>),
    ) -> FunctionValue<'ctx> {
        let ps: Vec<BasicMetadataTypeEnum> = params.iter().map(|p| (*p).into()).collect();
        let fn_ty = match ret {
            Some(r) => r.fn_type(&ps, false),
            None => self.ctx.void_type().fn_type(&ps, false),
        };
        let f = self.module.add_function(name, fn_ty, Some(linkage));
        self.support.borrow_mut().insert(name.to_string(), f);
        let fb = F::new(self, f);
        build(&fb);
        f
    }

    /// Purpose: This function declares a C library function.
    ///
    /// Inputs:
    /// - `name`: the function.
    ///
    /// Outputs: its declaration, with the signature the backend uses.
    pub fn libc(&self, name: &str) -> FunctionValue<'ctx> {
        let sym = libc_symbol(name);
        if let Some(f) = self.module.get_function(sym) {
            return f;
        }
        let (i64t, i32t, ptr, f64t) = (self.i64t(), self.i32t(), self.ptrt(), self.f64t());
        let void = self.ctx.void_type();
        let ty = match name {
            "malloc" => ptr.fn_type(&[i64t.into()], false),
            "realloc" => ptr.fn_type(&[ptr.into(), i64t.into()], false),
            "free" => void.fn_type(&[ptr.into()], false),
            "memcmp" => i32t.fn_type(&[ptr.into(), ptr.into(), i64t.into()], false),
            "write" => i64t.fn_type(&[i32t.into(), ptr.into(), i64t.into()], false),
            "read" => i64t.fn_type(&[i32t.into(), ptr.into(), i64t.into()], false),
            "exit" => void.fn_type(&[i32t.into()], false),
            "isatty" => i32t.fn_type(&[i32t.into()], false),
            "snprintf" => i32t.fn_type(&[ptr.into(), i64t.into(), ptr.into()], true),
            "strtod" => f64t.fn_type(&[ptr.into(), ptr.into()], false),
            "strtof" => self.f32t().fn_type(&[ptr.into(), ptr.into()], false),
            "strtoll" => i64t.fn_type(&[ptr.into(), ptr.into(), i32t.into()], false),
            "qsort" => void.fn_type(&[ptr.into(), i64t.into(), i64t.into(), ptr.into()], false),
            "pthread_attr_init" | "pthread_attr_destroy" => i32t.fn_type(&[ptr.into()], false),
            "pthread_attr_setstacksize" => i32t.fn_type(&[ptr.into(), i64t.into()], false),
            "pthread_create" => {
                i32t.fn_type(&[ptr.into(), ptr.into(), ptr.into(), ptr.into()], false)
            }
            "pthread_join" | "pthread_mutex_init" | "pthread_cond_init" | "pthread_cond_wait" => {
                i32t.fn_type(&[ptr.into(), ptr.into()], false)
            }
            "pthread_mutex_lock"
            | "pthread_mutex_unlock"
            | "pthread_cond_broadcast"
            | "pthread_detach" => i32t.fn_type(&[ptr.into()], false),
            "nanosleep" => i32t.fn_type(&[ptr.into(), ptr.into()], false),
            "clock_gettime" => i32t.fn_type(&[i32t.into(), ptr.into()], false),
            "tan" | "asin" | "acos" | "atan" => f64t.fn_type(&[f64t.into()], false),
            "atan2" | "hypot" => f64t.fn_type(&[f64t.into(), f64t.into()], false),
            "fopen" => ptr.fn_type(&[ptr.into(), ptr.into()], false),
            "fread" | "fwrite" => {
                i64t.fn_type(&[ptr.into(), i64t.into(), i64t.into(), ptr.into()], false)
            }
            "fclose" | "ferror" | "unlink" | "rmdir" | "closedir" => {
                i32t.fn_type(&[ptr.into()], false)
            }
            "stat" | "rename" => i32t.fn_type(&[ptr.into(), ptr.into()], false),
            "mkdir" => i32t.fn_type(&[ptr.into(), i32t.into()], false),
            "opendir" | "readdir" => ptr.fn_type(&[ptr.into()], false),
            "strlen" => i64t.fn_type(&[ptr.into()], false),
            "strerror" => ptr.fn_type(&[i32t.into()], false),
            "errno" => ptr.fn_type(&[], false),
            "getenv" => ptr.fn_type(&[ptr.into()], false),
            "setenv" => i32t.fn_type(&[ptr.into(), ptr.into(), i32t.into()], false),
            "getcwd" => ptr.fn_type(&[ptr.into(), i64t.into()], false),
            "getpid" => i32t.fn_type(&[], false),
            "strdup" => ptr.fn_type(&[ptr.into()], false),
            "dlopen" => ptr.fn_type(&[ptr.into(), i32t.into()], false),
            "dlsym" | "realpath" => ptr.fn_type(&[ptr.into(), ptr.into()], false),
            "dlerror" => ptr.fn_type(&[], false),
            "dladdr" | "strcmp" => i32t.fn_type(&[ptr.into(), ptr.into()], false),
            "unsetenv" => i32t.fn_type(&[ptr.into()], false),
            "tzset" => void.fn_type(&[], false),
            "localtime_r" => ptr.fn_type(&[ptr.into(), ptr.into()], false),
            "strftime" => i64t.fn_type(&[ptr.into(), i64t.into(), ptr.into(), ptr.into()], false),
            "pipe" | "posix_spawn_file_actions_init" | "posix_spawn_file_actions_destroy" => {
                i32t.fn_type(&[ptr.into()], false)
            }
            "posix_spawn_file_actions_adddup2" => {
                i32t.fn_type(&[ptr.into(), i32t.into(), i32t.into()], false)
            }
            "posix_spawn_file_actions_addopen" => i32t
                .fn_type(&[ptr.into(), i32t.into(), ptr.into(), i32t.into(), i32t.into()], false),
            "signal" => ptr.fn_type(&[i32t.into(), ptr.into()], false),
            "kill" => i32t.fn_type(&[i32t.into(), i32t.into()], false),
            "posix_spawn" | "posix_spawnp" => i32t.fn_type(
                &[ptr.into(), ptr.into(), ptr.into(), ptr.into(), ptr.into(), ptr.into()],
                false,
            ),
            "fcntl" => i32t.fn_type(&[i32t.into(), i32t.into()], true),
            "close" => i32t.fn_type(&[i32t.into()], false),
            "poll" => i32t.fn_type(&[ptr.into(), i64t.into(), i32t.into()], false),
            "waitpid" => i32t.fn_type(&[i32t.into(), ptr.into(), i32t.into()], false),
            other => panic!("the static backend declares no C function named {other}"),
        };
        self.module.add_function(sym, ty, Some(Linkage::External))
    }

    /// Purpose: This function returns an immortal string object for a literal.
    ///
    /// Inputs:
    /// - `s`: the text.
    ///
    /// Outputs: a pointer to a constant `{ -1, len, bytes, 0 }` object.
    pub fn lit(&self, s: &str) -> PointerValue<'ctx> {
        if let Some(p) = self.lits.borrow().get(s) {
            return *p;
        }
        let bytes = s.as_bytes();
        let arr_ty = self.i8t().array_type(bytes.len() as u32 + 1);
        let obj_ty = self.ctx.struct_type(
            &[self.i64t().into(), self.i64t().into(), self.i64t().into(), arr_ty.into()],
            false,
        );
        let mut data: Vec<inkwell::values::IntValue> =
            bytes.iter().map(|b| self.i8t().const_int(*b as u64, false)).collect();
        data.push(self.i8t().const_zero());
        let init = obj_ty.const_named_struct(&[
            self.i64t().const_int(u64::MAX, true).into(),
            self.i64t().const_int(bytes.len() as u64, false).into(),
            self.i64t().const_zero().into(),
            self.i8t().const_array(&data).into(),
        ]);
        let g = self.module.add_global(obj_ty, None, "jade.lit");
        g.set_initializer(&init);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.set_alignment(8);
        let p = g.as_pointer_value();
        self.lits.borrow_mut().insert(s.to_string(), p);
        p
    }

    /// Purpose: This function returns a C string constant, for `snprintf` formats.
    ///
    /// Inputs:
    /// - `s`: the text.
    ///
    /// Outputs: a pointer to its NUL-terminated bytes.
    pub fn cstr(&self, s: &str) -> PointerValue<'ctx> {
        let p = self.lit(s);
        // The bytes of a literal object start at `STR_DATA` and end with a NUL.
        // SAFETY: a constant offset into a constant global this module owns.
        let off = self.i64t().const_int(layout::STR_DATA as u64, false);
        unsafe { p.const_in_bounds_gep(self.i8t(), &[off]) }
    }

    /// Purpose: This function returns a struct's type id.
    ///
    /// Inputs:
    /// - `name`: the struct.
    ///
    /// Outputs: its index in the program's struct list.
    pub fn struct_id(&self, name: &str) -> u32 {
        self.mark_program_dependent();
        self.struct_ids.get(name).copied().unwrap_or(u32::MAX)
    }
}

/// Purpose: This function returns the symbol a C library function has on
/// this target. Intel macOS names the 64-bit-inode versions of the directory
/// and `stat` calls with a suffix, and each C library reaches `errno` through a
/// function of its own.
///
/// Inputs:
/// - `name`: the function as C source names it, or `errno`.
///
/// Outputs: the symbol to declare.
fn libc_symbol(name: &str) -> &str {
    let intel_mac = cfg!(all(target_os = "macos", target_arch = "x86_64"));
    match name {
        "errno" if cfg!(target_os = "macos") => "__error",
        "errno" => "__errno_location",
        "stat" if intel_mac => "stat$INODE64",
        "opendir" if intel_mac => "opendir$INODE64",
        "readdir" if intel_mac => "readdir$INODE64",
        other => other,
    }
}

/// Purpose: This function builds a program's LLVM module, whole: every
/// function in it, the library's included, as `--emit ir` shows it.
///
/// Inputs:
/// - `ctx`: the LLVM context.
/// - `prog`: the MIR program.
///
/// Outputs: the verified module, or LLVM's complaint about it.
pub fn build_module<'ctx>(ctx: &'ctx Context, prog: &mir::Program) -> Result<Module<'ctx>, String> {
    build_program_module(ctx, prog, &libcache::plan::LibPlan::whole(prog), shared::Role::Whole)
}

/// Purpose: This function builds the program's module: its own functions,
/// a private copy of each small library instance, and a declaration of each
/// library instance a cached object holds.
///
/// Inputs:
/// - `ctx`: the LLVM context.
/// - `prog`: the MIR program.
/// - `plan`: how each function is compiled.
/// - `role`: whole, or the program beside library objects, with what they
///   refer to.
///
/// Outputs: the verified module, or LLVM's complaint about it.
fn build_program_module<'ctx>(
    ctx: &'ctx Context,
    prog: &mir::Program,
    plan: &libcache::plan::LibPlan,
    role: shared::Role,
) -> Result<Module<'ctx>, String> {
    let mut cx = Cx::new(ctx, prog, role);
    let globals: Vec<GlobalValue> = prog
        .globals
        .iter()
        .map(|g| {
            let t = cx.llty(&g.ty);
            let gv = cx.module.add_global(t, None, &format!("jade.global.{}", g.name));
            gv.set_initializer(&layout::zero(t));
            gv.set_linkage(Linkage::Internal);
            gv
        })
        .collect();
    let funcs: Vec<Option<FunctionValue>> = prog
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let ty = cx.fn_type(f);
            match (plan.kind(i), plan.symbol(i)) {
                (libcache::plan::Kind::Object, Some(sym)) => Some(cx.declare_lib(sym, ty)),
                _ => Some(cx.module.add_function(
                    &format!("jade.fn{i}.{}", f.name),
                    ty,
                    Some(Linkage::Internal),
                )),
            }
        })
        .collect();
    cx.drops = prog
        .drops
        .iter()
        .map(|(class, ids)| {
            (class.clone(), ids.iter().filter_map(|id| funcs[id.0 as usize]).collect())
        })
        .collect();
    let env = func::Env { prog, funcs: &funcs, globals: &globals };
    for (i, f) in prog.funcs.iter().enumerate() {
        if plan.kind(i) == libcache::plan::Kind::Object {
            continue;
        }
        if let Some(fv) = funcs[i] {
            func::lower(&cx, &env, f, fv);
        }
    }
    cx.define_exports();
    let top = funcs
        .get(prog.main.0 as usize)
        .copied()
        .flatten()
        .ok_or("internal compiler error: the program has no top level")?;
    emit_c_main(&cx, top);
    cx.module.verify().map_err(|e| e.to_string())?;
    Ok(cx.module)
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the LLVM type of a MIR function.
    fn fn_type(&self, f: &mir::Func) -> inkwell::types::FunctionType<'ctx> {
        let params: Vec<BasicMetadataTypeEnum> =
            f.locals[..f.params as usize].iter().map(|l| self.llty(&l.ty).into()).collect();
        self.llty(&f.ret).fn_type(&params, false)
    }

    /// Purpose: This function declares a library instance a cached object
    /// defines: external, so the linker finds it, and hidden, so the call is
    /// direct and the symbol stays inside the program.
    fn declare_lib(
        &self,
        sym: &str,
        ty: inkwell::types::FunctionType<'ctx>,
    ) -> FunctionValue<'ctx> {
        let f = self.module.add_function(sym, ty, Some(Linkage::External));
        f.as_global_value().set_visibility(inkwell::GlobalVisibility::Hidden);
        f
    }
}

/// Purpose: This function emits the C `main`, which runs the program's top
/// level on a thread with a 256 MiB stack, as v1 did.
///
/// Inputs:
/// - `cx`: the module.
/// - `top`: the function holding the top-level statements.
///
/// Outputs: none.
fn emit_c_main<'ctx>(cx: &Cx<'ctx>, top: FunctionValue<'ctx>) {
    let ptr = cx.ptrt();
    let body = cx.support("jade.thread_body", Some(ptr.into()), &[ptr.into()], |f| {
        f.call(top, &[]);
        f.call(f.cx.report_uncaught_fn(), &[]);
        f.ret(f.null());
    });
    let main_ty = cx.i32t().fn_type(&[cx.i32t().into(), ptr.into()], false);
    let main = cx.module.add_function("main", main_ty, Some(Linkage::External));
    // Where the program begins before `main`, on a system whose linker would
    // otherwise want it from a development package; see `start.rs`.
    if let Some(start) = start::start_code() {
        cx.module.set_inline_assembly(start);
    }
    let f = F::new(cx, main);
    // `env.args()`'s hook reads the arguments from where `main` leaves them,
    // which exist when the program's code or a library object reads them.
    if cx.module.get_global("jade.argc").is_some() {
        let (argc, argv) = cx.args_globals();
        f.store(argc, f.sext(f.param_int(0), cx.i64t()));
        f.store(argv, f.param_ptr(1));
    }
    // The task lock is set up before any thread runs, when the program has tasks.
    if cx.module.get_function("jade.task.submit").is_some() || cx.exported("jade.task.mutex") {
        f.call(cx.task_init_fn(), &[]);
    }
    // So is the model's, when the program runs one.
    if cx.infer.is_some() {
        f.call(cx.infer_init_fn(), &[]);
    }
    // A pthread_attr_t and a pthread_t are opaque; 128 bytes holds either on
    // every Unix this targets.
    let attr = f.alloca(cx.i8t().array_type(128));
    let thread = f.alloca(cx.i8t().array_type(16));
    let ok_init = f.call_int(cx.libc("pthread_attr_init"), &[attr.into()]);
    let ok_size = f.call_int(
        cx.libc("pthread_attr_setstacksize"),
        &[attr.into(), f.i64(256 * 1024 * 1024).into()],
    );
    let both = f.or(ok_init, ok_size);
    let fallback = |f: &F<'_, 'ctx>| {
        f.call(top, &[]);
        f.call(f.cx.report_uncaught_fn(), &[]);
    };
    f.if_else(
        f.eq(both, f.i32(0)),
        |f| {
            let made = f.call_int(
                cx.libc("pthread_create"),
                &[
                    thread.into(),
                    attr.into(),
                    body.as_global_value().as_pointer_value().into(),
                    f.null().into(),
                ],
            );
            f.if_else(
                f.eq(made, f.i32(0)),
                |f| {
                    let t = f.load_ptr(thread);
                    f.call(cx.libc("pthread_join"), &[t.into(), f.null().into()]);
                },
                fallback,
            );
        },
        fallback,
    );
    f.ret(f.i32(0));
}

/// The optimization pipeline every module runs, the program's and each
/// library object's alike.
pub const PIPELINE: &str = "default<O2>";

/// The CPU and features objects are compiled for: any CPU of the target's
/// architecture, so a cached object suits every machine of it.
const CPU: &str = "generic";
const FEATURES: &str = "";

/// The code-generation options `target_machine` passes LLVM, as they enter
/// a cached object's key: change one there and change this with it.
const CODEGEN: &str = "opt=default reloc=pic code-model=default";

/// Purpose: This function compiles a MIR program into one module and
/// returns its LLVM IR, unoptimized, for `--emit ir`.
///
/// Inputs:
/// - `prog`: the program.
///
/// Outputs: the IR text, or LLVM's complaint about the module.
pub fn ir(prog: &mir::Program) -> Result<String, String> {
    let ctx = Context::create();
    let module = build_module(&ctx, prog)?;
    Ok(module.print_to_string().to_string())
}

/// Purpose: This struct holds the object files one build wrote, for the
/// linker.
#[derive(Debug)]
pub struct Objects {
    /// Every object to link, the program's first.
    pub files: Vec<PathBuf>,
    /// What the build did with the library cache.
    pub report: Report,
}

impl Objects {
    /// Purpose: This function deletes the program's object and every library
    /// object written beside the output, once they are linked. The cache's
    /// own objects stay.
    pub fn remove(&self) {
        if let Some(program) = self.files.first() {
            std::fs::remove_file(program).ok();
        }
        libcache::remove_scratch_objects(self.report.objects.iter());
    }
}

/// Purpose: This function compiles a MIR program into object files: the
/// program's own, and one per std library instance the cache holds.
///
/// Inputs:
/// - `prog`: the program.
/// - `out`: where the binary will go; objects that are not cached are written
///   beside it.
/// - `lib_cache`: where compiled library instances are kept,
///   `~/.jade/lib` for `jade build` (see [`default_lib_cache`]); `None`
///   compiles every function into the program's object.
///
/// Outputs: the objects, or the error.
pub fn objects(
    prog: &mir::Program,
    out: &Path,
    lib_cache: Option<&Path>,
) -> Result<Objects, String> {
    match lib_cache {
        Some(root) => libcache::objects(prog, out, root),
        None => whole(prog, out),
    }
}

/// Purpose: This function compiles a program into one object, as every
/// build did before the library cache.
///
/// Inputs:
/// - `prog`: the program.
/// - `out`: where the binary will go; the object is written beside it.
///
/// Outputs: the object, or the error.
fn whole(prog: &mir::Program, out: &Path) -> Result<Objects, String> {
    let ctx = Context::create();
    let module = build_module(&ctx, prog)?;
    let machine = target_machine()?;
    let obj = out.with_extension("o");
    write_object(&module, &machine, &obj)?;
    Ok(Objects { files: vec![obj], report: Report::default() })
}

/// Purpose: This function sets up LLVM's code generator for this machine,
/// once per process, so builds on several threads can share it.
///
/// Outputs: `Ok(())`, or why LLVM could not.
fn init_llvm() -> Result<(), String> {
    static INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    INIT.get_or_init(|| Target::initialize_native(&InitializationConfig::default())).clone()
}

/// Purpose: This function makes a target machine for this machine. Each
/// thread makes its own, since LLVM's is not shared between threads.
///
/// Outputs: the machine, or why LLVM could not make one.
fn target_machine() -> Result<TargetMachine, String> {
    init_llvm()?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    target
        .create_target_machine(
            &triple,
            CPU,
            FEATURES,
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or_else(|| "failed to create LLVM target machine".to_string())
}

/// Purpose: This function gives a module the machine's triple and data
/// layout, which its IR text then shows.
fn set_target(module: &Module<'_>, machine: &TargetMachine) {
    module.set_triple(&machine.get_triple());
    module.set_data_layout(&machine.get_target_data().get_data_layout());
}

/// Purpose: This function optimizes a module and writes it as an object.
///
/// Inputs:
/// - `module`: the module.
/// - `machine`: the target machine.
/// - `obj`: the object file to write.
///
/// Outputs: `Ok(())`, or LLVM's error.
fn write_object(module: &Module<'_>, machine: &TargetMachine, obj: &Path) -> Result<(), String> {
    optimize(module, machine)?;
    machine.write_to_file(module, FileType::Object, obj).map_err(|e| e.to_string())
}

/// Purpose: This function runs the optimization pipeline over a module.
fn optimize(module: &Module<'_>, machine: &TargetMachine) -> Result<(), String> {
    set_target(module, machine);
    module.run_passes(PIPELINE, machine, PassBuilderOptions::create()).map_err(|e| e.to_string())
}
