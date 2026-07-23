//! The `use llm` package.
//!
//! Every function here is stateful — each one reads or writes `VmState` (the
//! token budget, the inference backend handle, the selected model profile), so
//! none can be a pure `BuiltinFn`. That makes this package entirely a
//! [`Package::natives`] table: name → the `NativeFnId` the VM dispatches on.
//!
//! It used to be spelled out three times over — a `unreachable!()` stub per
//! function, an entry in a `fns` table pointing at that stub, and the real id
//! in a `llm_vm_dict_value()` override the VM special-cased by name. The stubs
//! existed only to satisfy a field that could not describe what these functions
//! are, and a missed edit surfaced as a panic at call time rather than a
//! compile error.

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext},
    vm::NativeFnId,
};

use crate::builtins::Package;

/// The `llm.*` surface. Dispatch for each id lives in the VM's `call_native`.
static LLM_NATIVES: &[(&str, NativeFnId)] = &[
    ("set_max_tokens",  NativeFnId::LlmSetMaxTokens),
    ("count_tokens",    NativeFnId::LlmCountTokens),
    ("total_tokens",    NativeFnId::LlmTotalTokens),
    ("keep_anchors",    NativeFnId::LlmKeepAnchors),
    ("model",           NativeFnId::LlmModel),
    ("health",          NativeFnId::LlmHealth),
    ("tool_grammar",    NativeFnId::LlmToolGrammar),
];

fn register_llm_types(ctx: &mut TypeContext) {
    ctx.define("llm".to_string(), JadeType::Unknown);
}

pub static LLM_PKG: Package = Package {
    import_name: "llm",
    global_name: "llm",
    fns: &[],
    natives: LLM_NATIVES,
    register_types: register_llm_types,
};
