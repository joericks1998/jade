/// Arg kind: how to coerce a JadeLang argument to an LLVM value for a C call.
#[derive(Copy, Clone)]
pub enum Arg {
    Ptr, // emit_expr then as_pointer
    I64, // emit_expr then value_to_i64
}

/// Return kind: what the C function returns and how to surface it.
#[derive(Copy, Clone)]
pub enum Ret {
    Ptr,      // returns ptr
    I64,      // returns i64
    Bool,     // returns i32, convert via NE 0 → i1
    Void,     // returns void, yield i64(0) sentinel
    I64Typed, // returns i64, then reinterpret via i64_to_value(ret_ty)
}

pub struct Sig {
    pub c_name: &'static str,
    pub args: &'static [Arg],
    pub ret: Ret,
    pub uses_dicts: bool,
}

// ── Module call table ─────────────────────────────────────────────────────────

/// Look up a stdlib MODULE call: module.method(args...).
/// Returns None for unknown modules/methods (falls through to next dispatch).
///
/// Special cases NOT in this table (handled inline in emit_call):
///   - json.stringify: complex inline codegen
///   - llm.set_max_tokens: no-op stub
///   - math.*: custom float promotion logic
pub fn module_sig(module: &str, method: &str) -> Option<Sig> {
    match (module, method) {
        ("json", "parse") => Some(Sig {
            c_name: "jrt_json_parse",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: true,
        }),
        ("fs", "exists") => Some(Sig {
            c_name: "jrt_fs_exists",
            args: &[Arg::Ptr],
            ret: Ret::Bool,
            uses_dicts: false,
        }),
        ("fs", "read") => Some(Sig {
            c_name: "jrt_fs_read",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("sh", "exec") => Some(Sig {
            c_name: "jrt_sh_exec",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("http", "get") => Some(Sig {
            c_name: "jrt_http_get",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: true,
        }),
        ("llm", "count_tokens") => Some(Sig {
            c_name: "jrt_count_tokens",
            args: &[Arg::Ptr],
            ret: Ret::I64,
            uses_dicts: false,
        }),
        ("time", "local") => Some(Sig {
            c_name: "jrt_time_local",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        _ => None,
    }
}

// ── Primitive method tables ───────────────────────────────────────────────────

/// Look up a STRING PRIMITIVE method call.
/// The sig includes the receiver as args[0] (always Ptr).
pub fn str_method_sig(method: &str) -> Option<Sig> {
    match method {
        "contains" => Some(Sig {
            c_name: "jrt_str_contains",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Bool,
            uses_dicts: false,
        }),
        "split" => Some(Sig {
            c_name: "jrt_str_split",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        "trim" => Some(Sig {
            c_name: "jrt_str_trim",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        "replace" => Some(Sig {
            c_name: "jrt_str_replace",
            args: &[Arg::Ptr, Arg::Ptr, Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        _ => None,
    }
}

/// Look up an ARRAY PRIMITIVE method call.
/// The sig includes the receiver as args[0] (always Ptr).
pub fn array_method_sig(method: &str) -> Option<Sig> {
    match method {
        "push" => Some(Sig {
            c_name: "jrt_array_push",
            args: &[Arg::Ptr, Arg::I64],
            ret: Ret::Void,
            uses_dicts: false,
        }),
        "pop" => Some(Sig {
            c_name: "jrt_array_pop",
            args: &[Arg::Ptr],
            ret: Ret::I64Typed,
            uses_dicts: false,
        }),
        _ => None,
    }
}

/// True if `name` is any known builtin method (used to gate primitive-method dispatch).
pub fn is_builtin_method(name: &str) -> bool {
    matches!(
        name,
        "contains" | "split" | "trim" | "replace"
            | "push" | "pop" | "sort"
            | "has" | "get"
            | "len"
    )
}
