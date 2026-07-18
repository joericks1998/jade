// Chunk→LLVM path (replacing the TProgram re-lowering in `expr.rs`), built as
// self-contained bricks. `cfg` reconstructs basic blocks from the bytecode;
// `lower` translates each opcode. `compile()` tries this path first and falls
// back to `expr.rs` for any opcode it can't yet lower.
pub mod cfg;
pub mod lower;
pub mod expr;
pub mod imports;
pub mod stmt;
pub mod stdlib;
pub mod types;

use std::collections::HashMap;
use std::path::Path;

use inkwell::{
    builder::Builder,
    context::Context,
    module::Module,
    targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine},
    types::{BasicTypeEnum, StructType},
    values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue},
    AddressSpace, OptimizationLevel,
};

use jade::compiler::tir::{JadeType, TProgram};

/// Per-scope map of variable name → (stack slot pointer, static type).
type ScopeMap<'ctx> = HashMap<String, (PointerValue<'ctx>, JadeType)>;

/// All state threaded through expression and statement codegen.
pub struct CodegenCtx<'ctx> {
    pub context: &'ctx Context,
    pub module: Module<'ctx>,
    pub builder: Builder<'ctx>,

    /// Lexical scope stack.  Index 0 is the outermost function scope.
    scopes: Vec<ScopeMap<'ctx>>,

    /// Declared Jade functions: name → (llvm fn, param jade-types, ret jade-type).
    pub fn_info: HashMap<String, (FunctionValue<'ctx>, Vec<JadeType>, JadeType)>,

    /// Parameter names for each declared function: mangled-name → ordered param names.
    /// Populated by declare_fns; used to resolve keyword arguments at call sites.
    pub fn_param_names: HashMap<String, Vec<String>>,

    /// Default value expressions per parameter: mangled-name → ordered
    /// `Option<default>` (required params are `None`). Populated by declare_fns;
    /// used to fill omitted arguments at call sites. Defaults are always literals
    /// (enforced by the frontend), so they can be re-emitted in any caller context.
    pub fn_param_defaults: HashMap<String, Vec<Option<jade::compiler::tir::TExpr>>>,

    /// Return type of the function currently being emitted.
    pub current_ret_ty: Option<JadeType>,

    // ── libc / runtime helpers ─────────────────────────────────────────────
    /// printf(const char *fmt, ...) -> i32
    pub printf_fn: FunctionValue<'ctx>,
    /// exit(i32) -> void
    pub exit_fn: FunctionValue<'ctx>,
    /// malloc(i64) -> ptr
    pub malloc_fn: FunctionValue<'ctx>,
    /// strlen(ptr) -> i64
    pub strlen_fn: FunctionValue<'ctx>,
    /// sprintf(ptr buf, ptr fmt, ...) -> i32  — variadic (used for non-fstring contexts)
    pub sprintf_fn: FunctionValue<'ctx>,
    /// snprintf(ptr buf, i64 max, ptr fmt, ...) -> i32  — variadic (fstring safe writes)
    pub snprintf_fn: FunctionValue<'ctx>,

    // ── Async runtime extern symbols ──────────────────────────────────────
    /// jade_spawn(fn_ptr: ptr, args: ptr, n: i32) -> ptr
    pub jade_spawn_fn: FunctionValue<'ctx>,
    /// jade_await(future: ptr) -> i64
    pub jade_await_fn: FunctionValue<'ctx>,
    /// jade_join(futures: ptr, n: i32, results_out: ptr) -> void
    pub jade_join_fn: FunctionValue<'ctx>,

    // ── jade_rt dict runtime ───────────────────────────────────────────────
    /// jade_dict_create() -> ptr
    pub jade_dict_create_fn: FunctionValue<'ctx>,
    /// jade_dict_set(ptr dict, ptr key, i64 value) -> void
    pub jade_dict_set_fn: FunctionValue<'ctx>,
    /// jade_dict_get(ptr dict, ptr key) -> i64
    pub jade_dict_get_fn: FunctionValue<'ctx>,
    /// jade_dict_has(ptr dict, ptr key) -> i32
    pub jade_dict_has_fn: FunctionValue<'ctx>,
    /// jade_dict_len(ptr dict) -> i64
    pub jade_dict_len_fn: FunctionValue<'ctx>,

    // ── jade_rt exception runtime ──────────────────────────────────────────
    /// setjmp(ptr jmpbuf) -> i32  [returns_twice]
    pub setjmp_fn: FunctionValue<'ctx>,
    /// jade_exc_push_frame(ptr jmpbuf) -> void
    pub jade_exc_push_frame_fn: FunctionValue<'ctx>,
    /// jade_exc_pop() -> void
    pub jade_exc_pop_fn: FunctionValue<'ctx>,
    /// jade_exc_throw(i64 value) -> void  [noreturn]
    pub jade_exc_throw_fn: FunctionValue<'ctx>,
    /// jade_exc_throw_typed(i64 value, ptr type_name) -> void  [noreturn]
    pub jade_exc_throw_typed_fn: FunctionValue<'ctx>,
    /// jade_exc_value() -> i64
    pub jade_exc_value_fn: FunctionValue<'ctx>,
    /// jade_exc_type() -> ptr  (thrown struct type name, or null)
    pub jade_exc_type_fn: FunctionValue<'ctx>,

    // ── jade_rt inference runtime ──────────────────────────────────────────
    /// jrt_prompt(ptr prompt, ptr model) -> ptr
    pub jrt_prompt_fn: FunctionValue<'ctx>,
    /// jrt_prompt_typed(ptr prompt, ptr model, ptr type_name, i32 max_retries) -> ptr
    pub jrt_prompt_typed_fn: FunctionValue<'ctx>,
    /// jrt_prompt_grammar(ptr prompt, ptr model, ptr grammar) -> ptr
    pub jrt_prompt_grammar_fn: FunctionValue<'ctx>,
    /// jrt_prompt_grammar_ex(ptr prompt, ptr model, ptr pattern, ptr anchor_or_null, ptr stop_or_null) -> ptr
    pub jrt_prompt_grammar_ex_fn: FunctionValue<'ctx>,
    /// jrt_prompt_stream_ex(ptr prompt, ptr model, ptr pattern_or_null, ptr anchor_or_null, ptr stop_or_null, i32 start_muted) -> ptr
    pub jrt_prompt_stream_ex_fn: FunctionValue<'ctx>,
    /// jrt_get_model() -> ptr
    pub jrt_get_model_fn: FunctionValue<'ctx>,

    /// jrt_str_new(i64 len, i8 trust) -> ptr (data pointer past header byte)
    pub jrt_str_new_fn: FunctionValue<'ctx>,
    /// jrt_trust_of(ptr) -> i8
    pub jrt_trust_of_fn: FunctionValue<'ctx>,

    // ── Grammar heap struct layout ─────────────────────────────────────────
    /// `%jade.grammar = type { ptr pattern, ptr anchor, ptr stop_anchor }`
    pub jade_grammar_ty: inkwell::types::StructType<'ctx>,

    // ── libc helpers for typed prompt parsing ──────────────────────────────
    /// atoll(ptr) -> i64
    pub atoll_fn: FunctionValue<'ctx>,
    /// strtod(ptr, ptr) -> double
    pub strtod_fn: FunctionValue<'ctx>,
    /// strcmp(ptr, ptr) -> i32
    pub strcmp_fn: FunctionValue<'ctx>,
    /// free(ptr) -> void
    pub free_fn: FunctionValue<'ctx>,
    /// strstr(haystack: ptr, needle: ptr) -> ptr  — for `x in str`
    pub strstr_fn: FunctionValue<'ctx>,

    // ── Async / dict / exception / prompt codegen state ────────────────────
    /// When `Some(ty)`, `TStmt::Return` in an async body or closure function
    /// converts its value via `value_to_i64(val, ty)` before emitting `ret i64`.
    pub async_body_ret_ty: Option<JadeType>,
    /// Set to `true` when any async construct (spawn/await/join) is emitted.
    pub uses_runtime: bool,
    pub uses_async: bool,
    /// Set to `true` when any dict runtime call is emitted.
    pub uses_dicts: bool,
    /// Set to `true` when any try/catch or raise is emitted.
    pub uses_exceptions: bool,
    /// Set to `true` when any prompt deref is emitted.
    pub uses_prompts: bool,

    // ── Heap type layouts ──────────────────────────────────────────────────
    /// `%jade.array = type { ptr data, i64 len, i64 cap }`
    pub array_ty: StructType<'ctx>,
    /// `%jade.fn = type { ptr fn_ptr, ptr env_ptr, ptr name_ptr }` — fat pointer for first-class functions.
    pub jade_fn_ty: StructType<'ctx>,

    /// Counter for unique closure body function names (`closure_0`, `closure_1`, …).
    pub closure_counter: usize,

    /// Struct name → field names in definition order.
    /// Populated by the struct pre-pass before codegen begins.
    pub struct_field_order: HashMap<String, Vec<String>>,

    /// Struct name → field name → JadeType, learned from StructLiteral expressions.
    /// Filled by the StructLiteral pre-pass; used to reinterpret i64 slots correctly.
    pub struct_field_types: HashMap<String, HashMap<String, jade::compiler::tir::JadeType>>,

    /// Module-level (top-level) `let` bindings stored as LLVM globals so that
    /// struct methods compiled in separate functions can load them without
    /// crossing stack-frame boundaries.
    pub module_globals: HashMap<String, (inkwell::values::GlobalValue<'ctx>, JadeType)>,

    /// Nesting depth of function bodies currently being compiled.  0 = module
    /// (top-level code in `main`); incremented on entry to every `emit_fn_body`.
    pub fn_depth: usize,

    /// Native (C-ABI) package id → the `@native_pkg$<id>` global holding its
    /// `dlopen` handle (loaded once in `main`'s prologue). A `__native$<id>$<fn>`
    /// reference loads the handle from here and dispatches via `jrt_native_call`.
    pub native_pkgs: HashMap<u32, inkwell::values::GlobalValue<'ctx>>,
}

impl<'ctx> CodegenCtx<'ctx> {
    pub fn new(context: &'ctx Context, module: Module<'ctx>, builder: Builder<'ctx>) -> Self {
        let ptr_ty = context.ptr_type(AddressSpace::default());
        let i32_ty = context.i32_type();
        let i64_ty = context.i64_type();

        // printf(const char *, ...) -> i32
        let printf_ty = i32_ty.fn_type(&[ptr_ty.into()], true);
        let printf_fn = module.add_function("printf", printf_ty, None);

        // exit(i32) -> void
        let exit_ty = context.void_type().fn_type(&[i32_ty.into()], false);
        let exit_fn = module.add_function("exit", exit_ty, None);

        // malloc(i64) -> ptr
        let malloc_ty = ptr_ty.fn_type(&[i64_ty.into()], false);
        let malloc_fn = module.add_function("malloc", malloc_ty, None);

        // strlen(ptr) -> i64   (C returns size_t; treat as i64 on 64-bit)
        let strlen_ty = i64_ty.fn_type(&[ptr_ty.into()], false);
        let strlen_fn = module.add_function("strlen", strlen_ty, None);

        // sprintf(ptr, ptr, ...) -> i32
        let sprintf_ty = i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], true);
        let sprintf_fn = module.add_function("sprintf", sprintf_ty, None);

        // snprintf(ptr buf, i64 max, ptr fmt, ...) -> i32
        let snprintf_ty = i32_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], true);
        let snprintf_fn = module.add_function("snprintf", snprintf_ty, None);

        // %jade.array = type { ptr, i64, i64 }
        let array_ty = context.opaque_struct_type("jade.array");
        array_ty.set_body(&[ptr_ty.into(), i64_ty.into(), i64_ty.into()], false);

        // jade_spawn(fn_ptr: ptr, args: ptr, n: i32) -> ptr
        let jade_spawn_ty = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), i32_ty.into()], false);
        let jade_spawn_fn = module.add_function("jade_spawn", jade_spawn_ty, None);

        // jade_await(future: ptr) -> i64
        let jade_await_ty = i64_ty.fn_type(&[ptr_ty.into()], false);
        let jade_await_fn = module.add_function("jade_await", jade_await_ty, None);

        // jade_join(futures: ptr, n: i32, results_out: ptr) -> void
        let jade_join_ty = context.void_type().fn_type(&[ptr_ty.into(), i32_ty.into(), ptr_ty.into()], false);
        let jade_join_fn = module.add_function("jade_join", jade_join_ty, None);

        // jade_dict_create() -> ptr
        let jade_dict_create_ty = ptr_ty.fn_type(&[], false);
        let jade_dict_create_fn = module.add_function("jade_dict_create", jade_dict_create_ty, None);

        // jade_dict_set(ptr, ptr, i64) -> void
        let jade_dict_set_ty = context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into(), i64_ty.into()], false);
        let jade_dict_set_fn = module.add_function("jade_dict_set", jade_dict_set_ty, None);

        // jade_dict_get(ptr, ptr) -> i64
        let jade_dict_get_ty = i64_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let jade_dict_get_fn = module.add_function("jade_dict_get", jade_dict_get_ty, None);

        // jade_dict_has(ptr, ptr) -> i32
        let jade_dict_has_ty = i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let jade_dict_has_fn = module.add_function("jade_dict_has", jade_dict_has_ty, None);

        // jade_dict_len(ptr) -> i64
        let jade_dict_len_ty = i64_ty.fn_type(&[ptr_ty.into()], false);
        let jade_dict_len_fn = module.add_function("jade_dict_len", jade_dict_len_ty, None);

        // setjmp(ptr) -> i32  [returns_twice — must not be optimised away]
        let setjmp_ty = i32_ty.fn_type(&[ptr_ty.into()], false);
        let setjmp_fn = module.add_function("setjmp", setjmp_ty, None);
        {
            use inkwell::attributes::AttributeLoc;
            let id = inkwell::attributes::Attribute::get_named_enum_kind_id("returns_twice");
            let attr = context.create_enum_attribute(id, 0);
            setjmp_fn.add_attribute(AttributeLoc::Function, attr);
        }

        // jade_exc_push_frame(ptr) -> void
        let jade_exc_push_frame_ty = context.void_type().fn_type(&[ptr_ty.into()], false);
        let jade_exc_push_frame_fn = module.add_function("jade_exc_push_frame", jade_exc_push_frame_ty, None);

        // jade_exc_pop() -> void
        let jade_exc_pop_ty = context.void_type().fn_type(&[], false);
        let jade_exc_pop_fn = module.add_function("jade_exc_pop", jade_exc_pop_ty, None);

        // jade_exc_throw(i64) -> void  [noreturn]
        let jade_exc_throw_ty = context.void_type().fn_type(&[i64_ty.into()], false);
        let jade_exc_throw_fn = module.add_function("jade_exc_throw", jade_exc_throw_ty, None);
        {
            use inkwell::attributes::AttributeLoc;
            let id = inkwell::attributes::Attribute::get_named_enum_kind_id("noreturn");
            let attr = context.create_enum_attribute(id, 0);
            jade_exc_throw_fn.add_attribute(AttributeLoc::Function, attr);
        }

        // jade_exc_throw_typed(i64, ptr) -> void  [noreturn]
        let jade_exc_throw_typed_ty = context.void_type().fn_type(&[i64_ty.into(), ptr_ty.into()], false);
        let jade_exc_throw_typed_fn = module.add_function("jade_exc_throw_typed", jade_exc_throw_typed_ty, None);
        {
            use inkwell::attributes::AttributeLoc;
            let id = inkwell::attributes::Attribute::get_named_enum_kind_id("noreturn");
            let attr = context.create_enum_attribute(id, 0);
            jade_exc_throw_typed_fn.add_attribute(AttributeLoc::Function, attr);
        }

        // jade_exc_value() -> i64
        let jade_exc_value_ty = i64_ty.fn_type(&[], false);
        let jade_exc_value_fn = module.add_function("jade_exc_value", jade_exc_value_ty, None);

        // jade_exc_type() -> ptr
        let jade_exc_type_ty = ptr_ty.fn_type(&[], false);
        let jade_exc_type_fn = module.add_function("jade_exc_type", jade_exc_type_ty, None);

        // jrt_prompt(ptr prompt, ptr model) -> ptr
        let jrt_prompt_ty = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let jrt_prompt_fn = module.add_function("jrt_prompt", jrt_prompt_ty, None);

        // jrt_prompt_typed(ptr prompt, ptr model, ptr type_name, i32 max_retries) -> ptr
        let jrt_prompt_typed_ty = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), i32_ty.into()], false);
        let jrt_prompt_typed_fn = module.add_function("jrt_prompt_typed", jrt_prompt_typed_ty, None);

        // jrt_prompt_grammar(ptr prompt, ptr model, ptr grammar) -> ptr
        let jrt_prompt_grammar_ty = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let jrt_prompt_grammar_fn = module.add_function("jrt_prompt_grammar", jrt_prompt_grammar_ty, None);

        // jrt_prompt_grammar_ex(ptr prompt, ptr model, ptr pattern, ptr anchor, ptr stop) -> ptr
        let jrt_prompt_grammar_ex_ty = ptr_ty.fn_type(
            &[ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), ptr_ty.into()],
            false,
        );
        let jrt_prompt_grammar_ex_fn = module.add_function("jrt_prompt_grammar_ex", jrt_prompt_grammar_ex_ty, None);

        // jrt_prompt_stream_ex(ptr, ptr, ptr, ptr, ptr, i32) -> ptr
        let jrt_prompt_stream_ex_ty = ptr_ty.fn_type(
            &[ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), ptr_ty.into(), i32_ty.into()],
            false,
        );
        let jrt_prompt_stream_ex_fn = module.add_function("jrt_prompt_stream_ex", jrt_prompt_stream_ex_ty, None);

        // jrt_get_model() -> ptr
        let jrt_get_model_ty = ptr_ty.fn_type(&[], false);
        let jrt_get_model_fn = module.add_function("jrt_get_model", jrt_get_model_ty, None);

        // jrt_str_new(i64 len, i8 trust) -> ptr
        let i8_ty = context.i8_type();
        let jrt_str_new_ty = ptr_ty.fn_type(&[i64_ty.into(), i8_ty.into()], false);
        let jrt_str_new_fn = module.add_function("jrt_str_new", jrt_str_new_ty, None);

        // jrt_trust_of(ptr) -> i8
        let jrt_trust_of_ty = i8_ty.fn_type(&[ptr_ty.into()], false);
        let jrt_trust_of_fn = module.add_function("jrt_trust_of", jrt_trust_of_ty, None);

        // %jade.grammar = type { ptr pattern, ptr anchor, ptr stop_anchor }
        let jade_grammar_ty = context.opaque_struct_type("jade.grammar");
        jade_grammar_ty.set_body(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);

        // atoll(ptr) -> i64
        let atoll_ty = i64_ty.fn_type(&[ptr_ty.into()], false);
        let atoll_fn = module.add_function("atoll", atoll_ty, None);

        // strtod(ptr endptr_or_null) -> double
        let f64_ty = context.f64_type();
        let strtod_ty = f64_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let strtod_fn = module.add_function("strtod", strtod_ty, None);

        // strcmp(ptr, ptr) -> i32
        let strcmp_ty = i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let strcmp_fn = module.add_function("strcmp", strcmp_ty, None);

        // free(ptr) -> void
        let free_ty = context.void_type().fn_type(&[ptr_ty.into()], false);
        let free_fn = module.add_function("free", free_ty, None);

        // strstr(const char *haystack, const char *needle) -> char*
        let strstr_ty = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
        let strstr_fn = module.add_function("strstr", strstr_ty, None);

        // %jade.fn = type { ptr fn_ptr, ptr env_ptr, ptr name_ptr }
        let jade_fn_ty = context.opaque_struct_type("jade.fn");
        jade_fn_ty.set_body(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);

        CodegenCtx {
            context,
            module,
            builder,
            scopes: vec![HashMap::new()],
            fn_info: HashMap::new(),
            fn_param_names: HashMap::new(),
            fn_param_defaults: HashMap::new(),
            current_ret_ty: None,
            printf_fn,
            exit_fn,
            malloc_fn,
            strlen_fn,
            sprintf_fn,
            snprintf_fn,
            jade_spawn_fn,
            jade_await_fn,
            jade_join_fn,
            jade_dict_create_fn,
            jade_dict_set_fn,
            jade_dict_get_fn,
            jade_dict_has_fn,
            jade_dict_len_fn,
            setjmp_fn,
            jade_exc_push_frame_fn,
            jade_exc_pop_fn,
            jade_exc_throw_fn,
            jade_exc_throw_typed_fn,
            jade_exc_value_fn,
            jade_exc_type_fn,
            jrt_prompt_fn,
            jrt_prompt_typed_fn,
            jrt_prompt_grammar_fn,
            jrt_prompt_grammar_ex_fn,
            jrt_prompt_stream_ex_fn,
            jrt_get_model_fn,
            jrt_str_new_fn,
            jrt_trust_of_fn,
            jade_grammar_ty,
            atoll_fn,
            strtod_fn,
            strcmp_fn,
            free_fn,
            strstr_fn,
            async_body_ret_ty: None,
            uses_runtime: false,
            uses_async: false,
            uses_dicts: false,
            uses_exceptions: false,
            uses_prompts: false,
            array_ty,
            jade_fn_ty,
            closure_counter: 0,
            struct_field_order: HashMap::new(),
            struct_field_types: HashMap::new(),
            module_globals: HashMap::new(),
            fn_depth: 0,
            native_pkgs: HashMap::new(),
        }
    }

    pub fn push_scope(&mut self) { self.scopes.push(HashMap::new()); }
    pub fn pop_scope(&mut self)  { self.scopes.pop(); }

    /// Lazily declare an external C function by looking it up in the module first.
    /// Replaces all the get_jrt_*() methods — driven by the stdlib::Sig table.
    pub fn extern_fn(&self, sig: &stdlib::Sig) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function(sig.c_name) { return f; }
        let ptr = self.context.ptr_type(AddressSpace::default());
        let i64t = self.context.i64_type();
        let params: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = sig.args.iter().map(|a| match a {
            stdlib::Arg::Ptr => ptr.into(),
            stdlib::Arg::I64 | stdlib::Arg::I64Raw => i64t.into(),
        }).collect();
        let fn_ty = match sig.ret {
            stdlib::Ret::Ptr       => ptr.fn_type(&params, false),
            stdlib::Ret::I64
            | stdlib::Ret::I64Typed => i64t.fn_type(&params, false),
            stdlib::Ret::Bool      => self.context.i32_type().fn_type(&params, false),
            stdlib::Ret::Void      => self.context.void_type().fn_type(&params, false),
        };
        self.module.add_function(sig.c_name, fn_ty, None)
    }

    /// Lazily declare jrt_readline(ptr) -> ptr (used by emit_input, not in the main table).
    pub fn get_jrt_readline(&self) -> FunctionValue<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        let ty = ptr.fn_type(&[ptr.into()], false);
        self.module.get_function("jrt_readline")
            .unwrap_or_else(|| self.module.add_function("jrt_readline", ty, None))
    }

    /// Lazily declare jrt_json_esc_str(ptr) -> ptr (used by emit_json_stringify).
    pub fn get_jrt_json_esc_str(&self) -> FunctionValue<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        let ty = ptr.fn_type(&[ptr.into()], false);
        self.module.get_function("jrt_json_esc_str")
            .unwrap_or_else(|| self.module.add_function("jrt_json_esc_str", ty, None))
    }

    /// Lazily declare jrt_native_load(ptr path) -> ptr handle.
    pub fn get_jrt_native_load(&self) -> FunctionValue<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        let ty = ptr.fn_type(&[ptr.into()], false);
        self.module.get_function("jrt_native_load")
            .unwrap_or_else(|| self.module.add_function("jrt_native_load", ty, None))
    }

    /// Lazily declare jrt_native_call(ptr handle, ptr name, ptr argv, i64 argc) -> i64.
    pub fn get_jrt_native_call(&self) -> FunctionValue<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        let i64t = self.context.i64_type();
        let ty = i64t.fn_type(&[ptr.into(), ptr.into(), ptr.into(), i64t.into()], false);
        self.module.get_function("jrt_native_call")
            .unwrap_or_else(|| self.module.add_function("jrt_native_call", ty, None))
    }

    /// Lazily declare jrt_json_arr_dicts(i64) -> ptr (used by emit_json_stringify).
    pub fn get_jrt_json_arr_dicts(&self) -> FunctionValue<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        let i64t = self.context.i64_type();
        let ty = ptr.fn_type(&[i64t.into()], false);
        self.module.get_function("jrt_json_arr_dicts")
            .unwrap_or_else(|| self.module.add_function("jrt_json_arr_dicts", ty, None))
    }

    /// Lazily declare jrt_llm_keep_anchors(i32) -> void (llm.keep_anchors).
    pub fn get_jrt_llm_keep_anchors(&self) -> FunctionValue<'ctx> {
        let i32t = self.context.i32_type();
        let ty = self.context.void_type().fn_type(&[i32t.into()], false);
        self.module.get_function("jrt_llm_keep_anchors")
            .unwrap_or_else(|| self.module.add_function("jrt_llm_keep_anchors", ty, None))
    }

    /// Lazily declare jrt_tool_grammar() -> ptr (llm.tool_grammar).
    pub fn get_jrt_tool_grammar(&self) -> FunctionValue<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        let ty = ptr.fn_type(&[], false);
        self.module.get_function("jrt_tool_grammar")
            .unwrap_or_else(|| self.module.add_function("jrt_tool_grammar", ty, None))
    }

    /// Lazily declare jrt_llm_find_tool_call(ptr) -> i64 (a tagged dict/nil).
    pub fn get_jrt_llm_find_tool_call(&self) -> FunctionValue<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        let i64t = self.context.i64_type();
        let ty = i64t.fn_type(&[ptr.into()], false);
        self.module.get_function("jrt_llm_find_tool_call")
            .unwrap_or_else(|| self.module.add_function("jrt_llm_find_tool_call", ty, None))
    }

    /// Lazily declare jrt_llm_find_tool_calls(ptr) -> i64 (a tagged array).
    pub fn get_jrt_llm_find_tool_calls(&self) -> FunctionValue<'ctx> {
        let ptr = self.context.ptr_type(AddressSpace::default());
        let i64t = self.context.i64_type();
        let ty = i64t.fn_type(&[ptr.into()], false);
        self.module.get_function("jrt_llm_find_tool_calls")
            .unwrap_or_else(|| self.module.add_function("jrt_llm_find_tool_calls", ty, None))
    }

    /// Lazily declare jrt_llm_profile() -> i64 (a tagged dict/nil).
    pub fn get_jrt_llm_profile(&self) -> FunctionValue<'ctx> {
        let i64t = self.context.i64_type();
        let ty = i64t.fn_type(&[], false);
        self.module.get_function("jrt_llm_profile")
            .unwrap_or_else(|| self.module.add_function("jrt_llm_profile", ty, None))
    }

    /// Lazily declare jrt_llm_health() -> i64 (a tagged dict/nil).
    pub fn get_jrt_llm_health(&self) -> FunctionValue<'ctx> {
        let i64t = self.context.i64_type();
        let ty = i64t.fn_type(&[], false);
        self.module.get_function("jrt_llm_health")
            .unwrap_or_else(|| self.module.add_function("jrt_llm_health", ty, None))
    }

    pub fn define(&mut self, name: String, ptr: PointerValue<'ctx>, ty: JadeType) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, (ptr, ty));
        }
    }

    pub fn lookup(&self, name: &str) -> Option<(PointerValue<'ctx>, JadeType)> {
        for scope in self.scopes.iter().rev() {
            if let Some(entry) = scope.get(name) {
                return Some(entry.clone());
            }
        }
        None
    }

    /// Resolve a variable's storage pointer + type, consulting module-level
    /// globals (top-level `let`, emitted as LLVM globals) before lexical scopes.
    /// A global's `as_pointer_value()` is the pointer to its slot — load-/store-
    /// compatible with a local alloca — so callers treat both uniformly.
    ///
    /// Mutating statements at module top level (`FieldAssign` / `IndexAssign`)
    /// need this: their target may be a global, which `lookup` (scopes only)
    /// never sees. The `Assign` arm inlines the same precedence.
    pub fn lookup_or_global(&self, name: &str) -> Option<(PointerValue<'ctx>, JadeType)> {
        if let Some((global, ty)) = self.module_globals.get(name) {
            return Some((global.as_pointer_value(), ty.clone()));
        }
        self.lookup(name)
    }

    /// Alloca placed in the **entry block** of the current function.
    pub fn build_entry_alloca(
        &self,
        ty: BasicTypeEnum<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, String> {
        let cur_bb = self.builder.get_insert_block()
            .ok_or("build_entry_alloca: not inside a function")?;
        let fn_val = cur_bb.get_parent()
            .ok_or("build_entry_alloca: block has no parent")?;
        let entry_bb = fn_val.get_first_basic_block()
            .ok_or("build_entry_alloca: function has no blocks")?;

        // Insert just before the first non-alloca instruction.
        let insert_before = {
            let mut cursor = entry_bb.get_first_instruction();
            while let Some(instr) = cursor {
                use inkwell::values::InstructionOpcode::Alloca;
                if instr.get_opcode() != Alloca { break; }
                cursor = instr.get_next_instruction();
            }
            cursor
        };

        if let Some(before) = insert_before {
            self.builder.position_before(&before);
        } else {
            self.builder.position_at_end(entry_bb);
        }

        let ptr = self.builder.build_alloca(ty, name).map_err(|e| e.to_string())?;
        self.builder.position_at_end(cur_bb);
        Ok(ptr)
    }

    /// True if the current block already has a terminator.
    pub fn is_terminated(&self) -> bool {
        self.builder.get_insert_block()
            .map(|bb| bb.get_terminator().is_some())
            .unwrap_or(true)
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    /// Call `fn_val` with `args` and extract the return value as `BasicValueEnum`.
    pub fn call_rv(
        &self,
        fn_val: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        use inkwell::values::{AnyValue, AnyValueEnum::*};
        let cs = self.builder.build_call(fn_val, args, name).map_err(|e| e.to_string())?;
        Ok(match cs.as_any_value_enum() {
            IntValue(v)     => v.into(),
            FloatValue(v)   => v.into(),
            PointerValue(v) => v.into(),
            StructValue(v)  => v.into(),
            ArrayValue(v)   => v.into(),
            VectorValue(v)  => v.into(),
            _ => self.context.i64_type().const_int(0, false).into(),
        })
    }

    /// Call `fn_val` discarding the return value.
    pub fn call_void(
        &self,
        fn_val: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<(), String> {
        self.builder.build_call(fn_val, args, "").map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Allocate `size` bytes on the heap and return the pointer.
    pub fn malloc_ptr(
        &self,
        size: inkwell::values::IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, String> {
        let malloc_fn = self.malloc_fn;
        self.call_rv(malloc_fn, &[size.into()], name)
            .map(|v| v.into_pointer_value())
    }

    /// Reinterpret the bits of `fval` as i64 (for storing floats in i64 slots).
    pub fn float_to_i64_bits(
        &self,
        fval: inkwell::values::FloatValue<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let slot = self.builder.build_alloca(self.context.f64_type(), "f2i_slot")
            .map_err(|e| e.to_string())?;
        self.builder.build_store(slot, fval).map_err(|e| e.to_string())?;
        self.builder.build_load(self.context.i64_type(), slot, "f2i_bits")
            .map_err(|e| e.to_string())
            .map(|v| v.into_int_value())
    }

    /// GEP wrapper that handles inkwell 0.8's unsafe `build_gep`.
    pub fn gep(
        &self,
        pointee_ty: impl inkwell::types::BasicType<'ctx>,
        ptr: inkwell::values::PointerValue<'ctx>,
        indices: &[inkwell::values::IntValue<'ctx>],
        name: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        unsafe { self.builder.build_gep(pointee_ty, ptr, indices, name) }
            .map_err(|e| e.to_string())
    }

}

// ── Public entry point ────────────────────────────────────────────────────────

/// Compile a type-inferred program to a native binary, or (when `emit_ir`)
/// return the LLVM IR as a string instead of writing/linking an object.
///
/// Returns `Ok(Some(ir))` for `emit_ir`, `Ok(None)` once a binary has been
/// written to `output_path`. The build daemon frames the returned IR back to
/// the client rather than printing it to the daemon's own stdout.
/// Try the Chunk→LLVM path for the whole program. On success, emits
/// `jade_toplevel() -> i64` (plus its `fn_defs`) into `module` and returns it,
/// for `main` to call after its prologue. Returns `Err` on any opcode the new
/// backend can't lower yet, so the caller falls back to the legacy `expr.rs`
/// lowering.
///
/// A **probe** runs the identical lowering into a throwaway module first: if it
/// fails partway (an unsupported opcode after some functions/globals were
/// already emitted), only the throwaway module is polluted — the real module is
/// touched only once the whole program is known to lower cleanly.
fn try_chunk_toplevel<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    program: &TProgram,
) -> Result<inkwell::values::FunctionValue<'ctx>, String> {
    let cp = jade::compiler::emit::emit(program.clone()).map_err(|e| e.to_string())?;
    let has_methods = !cp.extend_methods.is_empty();
    {
        let probe_ctx = Context::create();
        let probe_mod = probe_ctx.create_module("probe");
        lower::lower_program(&probe_ctx, &probe_mod, &cp.top, cp.top_n_slots, &cp.struct_defs, has_methods)?;
    }
    lower::lower_program(context, module, &cp.top, cp.top_n_slots, &cp.struct_defs, has_methods)
}

pub fn compile(program: TProgram, source_path: Option<&Path>, output_path: &Path, emit_ir: bool) -> Result<Option<String>, String> {
    // ── Import resolution + module namespacing ────────────────────────────
    // Inline every imported `.jde` file into one stream, mangling each imported
    // module's globals into its own `name$<id>` namespace so distinct modules
    // never collide — matching the bytecode VM (the source of truth), which keeps
    // imports namespaced. `main` keeps bare names. See `imports.rs`.
    let (mut program, native_pkgs) = if let Some(src) = source_path {
        let (stmts, native_pkgs) = imports::resolve_and_namespace(program.stmts, src)?;
        (jade::compiler::tir::TProgram { stmts }, native_pkgs)
    } else {
        (program, Vec::new())
    };

    // ── Inline defaults into cross-file struct literals ───────────────────
    // The per-file inference pass can't see imported StructDefs, so literals
    // like `messages.Session { system: ..., tools: ... }` leave default-bearing
    // fields (e.g. `let _history = []`) missing.  Without this pass the emitter
    // would zero-initialise those slots — turning `[]` into a NULL pointer.
    jade::compiler::type_infer::fill_struct_literal_defaults(&mut program)
        .map_err(|e| e.to_string())?;

    let context = Context::create();
    let module = context.create_module("jade_program");
    let builder = context.create_builder();

    let mut ctx = CodegenCtx::new(&context, module, builder);

    // ── Native package handle globals ─────────────────────────────────────
    // One `@native_pkg$<id>: ptr = null` per dlopen'd native library, filled in
    // main's prologue below. A `__native$<id>$<fn>` reference loads its handle here.
    {
        let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
        for (pkgid, _) in &native_pkgs {
            let g = ctx.module.add_global(ptr_ty, None, &format!("native_pkg${pkgid}"));
            g.set_initializer(&ptr_ty.const_null());
            ctx.native_pkgs.insert(*pkgid, g);
        }
    }

    // ── Struct pre-pass: record field names and infer field types ─────────
    stmt::collect_struct_defs(&program.stmts, &mut ctx.struct_field_order);
    stmt::collect_struct_literal_types(&program.stmts, &mut ctx.struct_field_types);

    // ── Pass 1: forward-declare all top-level functions ───────────────────
    stmt::declare_fns(&mut ctx, &program.stmts)?;

    // ── Pass 2: emit main() entry point ───────────────────────────────────
    // main(i32 argc, ptr argv): the args are forwarded to the runtime via
    // jrt_set_args so env.args() can read them. A bare main() would leave
    // env.args() empty.
    let i32_ty = ctx.context.i32_type();
    let ptr_ty = ctx.context.ptr_type(AddressSpace::default());
    let main_fn = ctx.module.add_function(
        "main", i32_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false), None);
    let entry_bb = ctx.context.append_basic_block(main_fn, "entry");
    ctx.builder.position_at_end(entry_bb);
    ctx.current_ret_ty = Some(JadeType::Int);

    // Record argv for jrt_env_args before any user code runs.
    {
        let argc = main_fn.get_nth_param(0).unwrap().into_int_value();
        let argv = main_fn.get_nth_param(1).unwrap().into_pointer_value();
        let set_args = if let Some(f) = ctx.module.get_function("jrt_set_args") { f } else {
            let void_ty = ctx.context.void_type();
            ctx.module.add_function(
                "jrt_set_args", void_ty.fn_type(&[i32_ty.into(), ptr_ty.into()], false), None)
        };
        ctx.builder.build_call(set_args, &[argc.into(), argv.into()], "")
            .map_err(|e| e.to_string())?;
    }

    // Load every native package once, before any user code runs. The path is a
    // build-time-resolved absolute path embedded as a plain C string (dlopen'd at
    // runtime — see imports.rs / runtime_lib/native.c).
    if !native_pkgs.is_empty() {
        ctx.uses_exceptions = true; // jrt_native_load can raise on a failed dlopen
        let load_fn = ctx.get_jrt_native_load();
        for (pkgid, path) in &native_pkgs {
            let path_ptr = ctx
                .builder
                .build_global_string_ptr(path, "native_pkg_path")
                .map_err(|e| e.to_string())?
                .as_pointer_value();
            let handle = ctx
                .call_rv(load_fn, &[path_ptr.into()], "native_load")?
                .into_pointer_value();
            let g = ctx.native_pkgs[pkgid];
            ctx.builder
                .build_store(g.as_pointer_value(), handle)
                .map_err(|e| e.to_string())?;
        }
    }

    // ── Body: try the Chunk→LLVM path, fall back to the TProgram lowering ──
    // The migration (Track A) is replacing `expr.rs`/`stmt.rs`'s re-lowering of
    // the typed AST with a mechanical translation of the bytecode `Chunk` (the
    // same one the VM runs). It grows one opcode-family at a time; any opcode it
    // can't yet handle makes `try_chunk_toplevel` return `Err`, and we fall back
    // to the legacy path for that whole program. So difftest gates each new
    // family the moment it lands — no lowering ships unverified.
    match try_chunk_toplevel(&context, &ctx.module, &program) {
        Ok(top_fn) => {
            ctx.builder
                .build_call(top_fn, &[], "")
                .map_err(|e| e.to_string())?;
        }
        Err(chunk_err) => {
            // The Chunk backend can't yet lower some opcode in this program; fall
            // back to the legacy TProgram lowering. Set JADE_CHUNK_DEBUG to see
            // which opcode forced the fallback (used to measure migration coverage).
            if std::env::var_os("JADE_CHUNK_DEBUG").is_some() {
                eprintln!("CHUNK FALLBACK: {chunk_err}");
            }
            stmt::emit_stmts(&mut ctx, &program.stmts)?;
        }
    }

    if !ctx.is_terminated() {
        ctx.builder
            .build_return(Some(&i32_ty.const_int(0, false)))
            .map_err(|e| e.to_string())?;
    }

    ctx.module.verify().map_err(|e| e.to_string())?;

    if emit_ir {
        return Ok(Some(ctx.module.print_to_string().to_string()));
    }

    // ── AOT compilation ───────────────────────────────────────────────────
    Target::initialize_all(&InitializationConfig::default());
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

    let obj_path = output_path.with_extension("o");
    machine
        .write_to_file(&ctx.module, FileType::Object, &obj_path)
        .map_err(|e| e.to_string())?;

    let mut cc = std::process::Command::new("cc");
    cc.arg(&obj_path).arg("-o").arg(output_path);
    if cfg!(target_os = "macos") {
        if let Ok(ver) = std::process::Command::new("sw_vers").args(["-productVersion"]).output() {
            if let Ok(s) = std::str::from_utf8(&ver.stdout) {
                let short = s.trim().splitn(3, '.').take(2).collect::<Vec<_>>().join(".");
                cc.arg(format!("-mmacosx-version-min={short}"));
            }
        }
    }
    // Always link the runtime archive. Previously this was gated on a hand-kept
    // set of feature flags (uses_runtime/uses_async/uses_dicts/uses_exceptions/
    // uses_prompts), but that enumeration was incomplete: programs that only call
    // string methods (jrt_str_trim/split/replace/contains) or array methods
    // (jrt_array_push/pop) set none of those flags, so `-lJadeRuntime` was never
    // passed and the link failed with undefined `_jrt_*` symbols. The runtime is
    // a static archive, so unreferenced members aren't pulled into trivial
    // binaries — always linking is harmless and removes the whole bug class.
    let _ = (ctx.uses_runtime, ctx.uses_async, ctx.uses_dicts, ctx.uses_exceptions, ctx.uses_prompts);
    // The C runtime archive dir: a caller-set `JADE_RT_LIB` (installed daemon)
    // wins, else this crate's own build.rs-baked OUT_DIR (dev builds — codegen
    // now owns the C runtime, so it needs no daemon to hand it the path). Same
    // for the shared Rust runtime staticlib below.
    let rt_lib = std::env::var("JADE_RT_LIB").unwrap_or_else(|_| env!("JADE_RT_LIB_DIR").to_string());
    cc.arg(format!("-L{rt_lib}"));
    cc.arg("-lJadeRuntime");
    // The shared Rust runtime staticlib supplies symbols moved out of the C
    // runtime (float boxing, ipow, …). It must come *after* -lJadeRuntime, whose
    // members reference these symbols (static-archive left-to-right resolution).
    let rust_rt = std::env::var("JADE_RUST_RT").unwrap_or_else(|_| env!("JADE_RUST_RT_DIR").to_string());
    cc.arg(format!("-L{rust_rt}"));
    cc.arg("-ljade_runtime");
    #[cfg(target_os = "linux")]
    {
        cc.arg("-lpthread");
        // dlopen/dlsym for native (C-ABI) packages (libdl). macOS has these in
        // libSystem, so no flag is needed there.
        cc.arg("-ldl");
        // libm for the runtime's float math (jrt_pow_any/jrt_mod_any call
        // pow/fmod). macOS folds libm into libSystem, so no flag is needed
        // there; on Linux it's a separate archive and must come *after*
        // -lJadeRuntime, which references these symbols.
        cc.arg("-lm");
    }
    let status = cc.status().map_err(|e| format!("linker not found: {e}"))?;

    std::fs::remove_file(&obj_path).ok();
    if !status.success() { return Err("linking failed".into()); }

    Ok(None)
}
