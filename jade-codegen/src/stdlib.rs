/// Arg kind: how to coerce a JadeLang argument to an LLVM value for a C call.
#[derive(Copy, Clone)]
pub enum Arg {
    Ptr,    // emit_expr then as_pointer
    I64,    // emit_expr then value_to_i64 (TAGGED value word — e.g. array.push)
    I64Raw, // emit_expr as a native (untagged) i64 — e.g. random.int bounds
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
///   - llm.set_max_tokens / keep_anchors / model / tool_grammar / profile /
///     health / find_tool_call / find_tool_calls: return Unknown-typed values
///     (tagged dicts/arrays, strings, or sticky-state nil), lowered inline
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
        // fs.read is lowered inline (optional `trust` arg, defaults to 0) — see emit_call.
        ("fs", "write") => Some(Sig {
            // (path, content) -> void. Matches the VM's `fs.write` returning Nil
            // (the Void sentinel is i64(0), discarded by callers). Not a code-
            // execution sink, so jrt_fs_write does not refuse tainted input —
            // mirroring the VM, which performs no trust check.
            c_name: "jrt_fs_write",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Void,
            uses_dicts: false,
        }),
        ("sh", "exec") => Some(Sig {
            c_name: "jrt_sh_exec",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("sh", "output") => Some(Sig {
            // (cmd) -> dict{stdout, stderr (strs), code (int)}. Captures both
            // streams separately and never errors on non-zero exit (unlike
            // sh.exec). stdout/stderr are TAINTED (shell output) per the ABI.
            c_name: "jrt_sh_output",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: true,
        }),
        ("env", "cwd") => Some(Sig {
            c_name: "jrt_env_cwd",
            args: &[],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("fs", "list_dir") => Some(Sig {
            // (path) -> array of file-name strs (no "."/"..", unordered,
            // includes hidden). File-derived → TAINTED, matching fs.read.
            c_name: "jrt_fs_list_dir",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("path", "basename") => Some(Sig {
            c_name: "jrt_path_basename",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("path", "ext") => Some(Sig {
            // (p) -> ".ext" str, or nil (NULL) when there's no extension.
            c_name: "jrt_path_ext",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("path", "join") => Some(Sig {
            // (a, b) -> joined path. The VM's path.join is variadic (>= 2);
            // emit_call left-folds N args through this 2-arg primitive, so this
            // Sig is the fold's building block (see the path.join special case).
            c_name: "jrt_path_join",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        // http.* is lowered inline (optional headers/body args) — see emit_call.
        ("array", "map") => Some(Sig {
            // (arr, fn) -> new array of fn(elem). fn is a jade_fn_t* fat pointer.
            c_name: "jrt_array_map",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("array", "filter") => Some(Sig {
            // (arr, fn) -> elements where fn(elem) is true.
            c_name: "jrt_array_filter",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("dict", "merge") => Some(Sig {
            // (d1, d2) -> new dict (d2 wins on conflict). Inputs unchanged.
            c_name: "jrt_dict_merge",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: true,
        }),
        ("random", "int") => Some(Sig {
            // (lo, hi) -> uniform int in [lo, hi]. Raises if lo > hi. Raw i64
            // bounds (not tagged value words).
            c_name: "jrt_random_int",
            args: &[Arg::I64Raw, Arg::I64Raw],
            ret: Ret::I64,
            uses_dicts: false,
        }),
        ("random", "seed") => Some(Sig {
            c_name: "jrt_random_seed",
            args: &[Arg::I64Raw],
            ret: Ret::Void,
            uses_dicts: false,
        }),
        ("random", "choice") => Some(Sig {
            // (arr) -> a random element (already tagged). Raises on empty array.
            c_name: "jrt_random_choice",
            args: &[Arg::Ptr],
            ret: Ret::I64Typed,
            uses_dicts: false,
        }),
        ("random", "shuffle") => Some(Sig {
            // (arr) -> Nil. Fisher-Yates in place.
            c_name: "jrt_random_shuffle",
            args: &[Arg::Ptr],
            ret: Ret::Void,
            uses_dicts: false,
        }),
        // random.float is lowered inline (it returns a double; Sig has no float ret).
        ("llm", "count_tokens") => Some(Sig {
            c_name: "jrt_count_tokens",
            args: &[Arg::Ptr],
            ret: Ret::I64,
            uses_dicts: false,
        }),
        ("llm", "total_tokens") => Some(Sig {
            // () -> cumulative session token count (a `stats_only` daemon
            // round-trip). Mirrors the VM's llm.total_tokens.
            c_name: "jrt_total_tokens",
            args: &[],
            ret: Ret::I64,
            uses_dicts: false,
        }),
        ("time", "local") => Some(Sig {
            c_name: "jrt_time_local",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("time", "now") => Some(Sig {
            // () -> i64 seconds since the epoch. Typed Int (pkg_call_return_type),
            // so the native i64 is interpreted directly — no tag round-trip.
            c_name: "jrt_time_now",
            args: &[],
            ret: Ret::I64,
            uses_dicts: false,
        }),
        ("time", "now_ms") => Some(Sig {
            c_name: "jrt_time_now_ms",
            args: &[],
            ret: Ret::I64,
            uses_dicts: false,
        }),
        // time.sleep is NOT here: its arg is a float (Sig only models Ptr/I64
        // args), so it's lowered inline in emit_call.
        ("sh", "run") => Some(Sig {
            // (cmd) -> exit code. Execution sink; jrt_sh_run refuses tainted.
            c_name: "jrt_sh_run",
            args: &[Arg::Ptr],
            ret: Ret::I64,
            uses_dicts: false,
        }),
        ("fs", "append") => Some(Sig {
            // (path, content) -> Nil. Like fs.write: not a sink, no taint check.
            c_name: "jrt_fs_append",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Void,
            uses_dicts: false,
        }),
        ("fs", "delete") => Some(Sig {
            c_name: "jrt_fs_delete",
            args: &[Arg::Ptr],
            ret: Ret::Void,
            uses_dicts: false,
        }),
        ("fs", "mkdir") => Some(Sig {
            // (path) -> Nil. Recursive (create_dir_all).
            c_name: "jrt_fs_mkdir",
            args: &[Arg::Ptr],
            ret: Ret::Void,
            uses_dicts: false,
        }),
        ("env", "set") => Some(Sig {
            c_name: "jrt_env_set",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Void,
            uses_dicts: false,
        }),
        ("env", "args") => Some(Sig {
            // () -> array of TRUSTED strs (argv). Unknown-typed (like fs.list_dir):
            // a raw array ptr is a valid Unknown payload.
            c_name: "jrt_env_args",
            args: &[],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        // env.get is NOT here: it returns str-or-nil and is Unknown-typed, so the
        // char*/NULL result must be tagged (str) or turned into a tagged nil —
        // lowered inline in emit_call.
        ("path", "dirname") => Some(Sig {
            c_name: "jrt_path_dirname",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("path", "stem") => Some(Sig {
            c_name: "jrt_path_stem",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("path", "abs") => Some(Sig {
            c_name: "jrt_path_abs",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        ("path", "is_abs") => Some(Sig {
            c_name: "jrt_path_is_abs",
            args: &[Arg::Ptr],
            ret: Ret::Bool,
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
        "upper" => Some(Sig {
            c_name: "jrt_str_upper",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        "lower" => Some(Sig {
            c_name: "jrt_str_lower",
            args: &[Arg::Ptr],
            ret: Ret::Ptr,
            uses_dicts: false,
        }),
        "starts_with" => Some(Sig {
            c_name: "jrt_str_starts_with",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Bool,
            uses_dicts: false,
        }),
        "ends_with" => Some(Sig {
            c_name: "jrt_str_ends_with",
            args: &[Arg::Ptr, Arg::Ptr],
            ret: Ret::Bool,
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
            | "upper" | "lower" | "starts_with" | "ends_with"
            | "push" | "pop" | "sort" | "reverse"
            | "has" | "get" | "keys" | "values"
            | "len"
    )
}
