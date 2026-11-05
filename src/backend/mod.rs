//! Jade 2.0's static backend: MIR → LLVM → a binary that links only libc.
//!
//! Every value has a type the compiler knows, so an int is an `i64` in a
//! register, a float an `f64`, and a string, array, dict or struct a pointer to
//! a reference-counted object whose layout this backend decides. Nothing in a
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
//! str     { i64 rc, i64 len, u8 data[len], u8 0 }
//! array   { i64 rc, i64 len, i64 cap, ptr data }          data: cap elements
//! dict    { i64 rc, i64 len, i64 cap, ptr keys, ptr vals, ptr index, i64 icap }
//! struct  { i64 rc, i64 type_id, fields... }
//! T?      a null pointer for nil when T is a pointer, else { i1 present, T }
//! ```
//!
//! ## Files
//!
//! `fb.rs` is the function-building layer the rest is written in. `layout.rs`
//! maps types to LLVM types, `func.rs` lowers MIR functions, and `rc.rs`
//! emits retain and release. The support code is split by what it serves:
//! `text.rs` (strings), `format.rs` (printing and conversions),
//! `collections.rs` (arrays and dicts), `unicode.rs` (case tables) and
//! `errors.rs` (runtime errors).

mod collections;
mod compare;
mod errors;
mod fb;
mod format;
mod func;
mod layout;
mod rc;
mod tasks;
mod text;
mod unicode;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;

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

use crate::mir;

/// Purpose: This enum lists what a static build produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emit {
    /// A native executable.
    Binary,
    /// The LLVM IR, before optimization, as text.
    Ir,
}

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
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function makes an empty module for a program.
    ///
    /// Inputs:
    /// - `ctx`: the LLVM context.
    /// - `prog`: the program, for its struct types.
    ///
    /// Outputs: the module context.
    fn new(ctx: &'ctx Context, prog: &mir::Program) -> Self {
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
        let ps: Vec<BasicMetadataTypeEnum> = params.iter().map(|p| (*p).into()).collect();
        let fn_ty = match ret {
            Some(r) => r.fn_type(&ps, false),
            None => self.ctx.void_type().fn_type(&ps, false),
        };
        let f = self.module.add_function(name, fn_ty, Some(Linkage::Internal));
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
        if let Some(f) = self.module.get_function(name) {
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
            "snprintf" => i32t.fn_type(&[ptr.into(), i64t.into(), ptr.into()], true),
            "strtod" => f64t.fn_type(&[ptr.into(), ptr.into()], false),
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
            other => panic!("the static backend declares no C function named {other}"),
        };
        self.module.add_function(name, ty, Some(Linkage::External))
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
        let obj_ty =
            self.ctx.struct_type(&[self.i64t().into(), self.i64t().into(), arr_ty.into()], false);
        let mut data: Vec<inkwell::values::IntValue> =
            bytes.iter().map(|b| self.i8t().const_int(*b as u64, false)).collect();
        data.push(self.i8t().const_zero());
        let init = obj_ty.const_named_struct(&[
            self.i64t().const_int(u64::MAX, true).into(),
            self.i64t().const_int(bytes.len() as u64, false).into(),
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
        // The bytes of a literal object start 16 bytes in and end with a NUL.
        // SAFETY: a constant offset into a constant global this module owns.
        unsafe { p.const_in_bounds_gep(self.i8t(), &[self.i64t().const_int(16, false)]) }
    }

    /// Purpose: This function returns a struct's type id.
    ///
    /// Inputs:
    /// - `name`: the struct.
    ///
    /// Outputs: its index in the program's struct list.
    pub fn struct_id(&self, name: &str) -> u32 {
        self.struct_ids.get(name).copied().unwrap_or(u32::MAX)
    }
}

/// Purpose: This function builds a program's LLVM module.
///
/// Inputs:
/// - `ctx`: the LLVM context.
/// - `prog`: the MIR program.
///
/// Outputs: the verified module, or LLVM's complaint about it.
pub fn build_module<'ctx>(ctx: &'ctx Context, prog: &mir::Program) -> Result<Module<'ctx>, String> {
    let mut cx = Cx::new(ctx, prog);
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
    let funcs: Vec<FunctionValue> = prog
        .funcs
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let params: Vec<BasicMetadataTypeEnum> =
                f.locals[..f.params as usize].iter().map(|l| cx.llty(&l.ty).into()).collect();
            let ty = cx.llty(&f.ret).fn_type(&params, false);
            cx.module.add_function(&format!("jade.fn{i}.{}", f.name), ty, Some(Linkage::Internal))
        })
        .collect();
    cx.drops = prog
        .drops
        .iter()
        .map(|(class, ids)| (class.clone(), ids.iter().map(|id| funcs[id.0 as usize]).collect()))
        .collect();
    let env = func::Env { prog, funcs: &funcs, globals: &globals };
    for (i, f) in prog.funcs.iter().enumerate() {
        func::lower(&cx, &env, f, funcs[i]);
    }
    emit_c_main(&cx, funcs[prog.main.0 as usize]);
    cx.module.verify().map_err(|e| e.to_string())?;
    Ok(cx.module)
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
    let f = F::new(cx, main);
    // The task lock is set up before any thread runs, when the program has tasks.
    if cx.module.get_function("jade.task.submit").is_some() {
        f.call(cx.task_init_fn(), &[]);
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

/// Purpose: This function compiles a MIR program to a binary, or to IR text.
///
/// Inputs:
/// - `prog`: the program.
/// - `out`: where the binary goes.
/// - `emit`: what to produce.
///
/// Outputs: the IR text for `Emit::Ir`, `None` after writing a binary, or the
/// error.
pub fn compile(prog: &mir::Program, out: &Path, emit: Emit) -> Result<Option<String>, String> {
    let ctx = Context::create();
    let module = build_module(&ctx, prog)?;
    if emit == Emit::Ir {
        return Ok(Some(module.print_to_string().to_string()));
    }
    Target::initialize_native(&InitializationConfig::default())?;
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).map_err(|e| e.to_string())?;
    let machine = target
        .create_target_machine(
            &triple,
            "generic",
            "",
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or("failed to create LLVM target machine")?;
    module.set_triple(&triple);
    module.set_data_layout(&machine.get_target_data().get_data_layout());
    module
        .run_passes("default<O2>", &machine, PassBuilderOptions::create())
        .map_err(|e| e.to_string())?;
    let obj = out.with_extension("o");
    machine.write_to_file(&module, FileType::Object, &obj).map_err(|e| e.to_string())?;
    let linked = link(&obj, out);
    std::fs::remove_file(&obj).ok();
    linked?;
    Ok(None)
}

/// Purpose: This function links an object file against the C library alone.
///
/// Inputs:
/// - `obj`: the object file.
/// - `out`: the binary to write.
///
/// Outputs: `Ok(())`, or the linker's own message.
fn link(obj: &Path, out: &Path) -> Result<(), String> {
    let mut cc = std::process::Command::new("cc");
    cc.arg(obj).arg("-o").arg(out);
    if cfg!(target_os = "macos")
        && let Ok(ver) = std::process::Command::new("sw_vers").args(["-productVersion"]).output()
        && let Ok(s) = std::str::from_utf8(&ver.stdout)
    {
        let short = s.trim().splitn(3, '.').take(2).collect::<Vec<_>>().join(".");
        cc.arg(format!("-mmacosx-version-min={short}"));
    }
    if cfg!(target_os = "linux") {
        cc.arg("-lpthread");
    }
    let result = cc.output().map_err(|e| format!("linker not found: {e}"))?;
    if !result.status.success() {
        return Err(format!("linking failed\n{}", String::from_utf8_lossy(&result.stderr).trim()));
    }
    Ok(())
}
