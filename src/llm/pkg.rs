use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::{NativeFnId, VmValue}},
    frontend::error::{Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

// `llm.set_max_tokens` is state-mutating so it cannot be a pure BuiltinFn.
// We expose a sentinel BuiltinFn that is never called directly — the VM's
// NativeFn(NativeFnId::LlmSetMaxTokens) variant is what actually runs.
// The Package::vm_dict_value() override below injects the right VmValue.

fn unreachable_set_max_tokens(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.set_max_tokens is handled by NativeFnId dispatch in the VM")
}

fn unreachable_count_tokens(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.count_tokens is handled by NativeFnId dispatch in the VM")
}

fn unreachable_total_tokens(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.total_tokens is handled by NativeFnId dispatch in the VM")
}

fn unreachable_keep_anchors(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.keep_anchors is handled by NativeFnId dispatch in the VM")
}

fn unreachable_model(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.model is handled by NativeFnId dispatch in the VM")
}

fn unreachable_profile(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.profile is handled by NativeFnId dispatch in the VM")
}

fn unreachable_health(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.health is handled by NativeFnId dispatch in the VM")
}

fn unreachable_find_tool_call(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.find_tool_call is handled by NativeFnId dispatch in the VM")
}

fn unreachable_find_tool_calls(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.find_tool_calls is handled by NativeFnId dispatch in the VM")
}

fn unreachable_tool_grammar(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.tool_grammar is handled by NativeFnId dispatch in the VM")
}

static LLM_FNS: [BuiltinFn; 10] = [
    BuiltinFn { name: "set_max_tokens", vm_impl: unreachable_set_max_tokens },
    BuiltinFn { name: "count_tokens",   vm_impl: unreachable_count_tokens },
    BuiltinFn { name: "total_tokens",   vm_impl: unreachable_total_tokens },
    BuiltinFn { name: "keep_anchors",   vm_impl: unreachable_keep_anchors },
    BuiltinFn { name: "model",          vm_impl: unreachable_model },
    BuiltinFn { name: "profile",        vm_impl: unreachable_profile },
    BuiltinFn { name: "health",         vm_impl: unreachable_health },
    BuiltinFn { name: "find_tool_call", vm_impl: unreachable_find_tool_call },
    BuiltinFn { name: "find_tool_calls", vm_impl: unreachable_find_tool_calls },
    BuiltinFn { name: "tool_grammar",   vm_impl: unreachable_tool_grammar },
];

fn register_llm_types(ctx: &mut TypeContext) {
    ctx.define("llm".to_string(), JadeType::Unknown);
}

pub static LLM_PKG: Package = Package {
    import_name: "llm",
    global_name: "llm",
    fns: &LLM_FNS,
    register_types: register_llm_types,
};

/// Override: inject the real NativeFn value for set_max_tokens.
pub fn llm_vm_dict_value() -> VmValue {
    let mut map = std::collections::HashMap::new();
    map.insert("set_max_tokens".to_string(), VmValue::NativeFn(NativeFnId::LlmSetMaxTokens));
    map.insert("count_tokens".to_string(),   VmValue::NativeFn(NativeFnId::LlmCountTokens));
    map.insert("total_tokens".to_string(),   VmValue::NativeFn(NativeFnId::LlmTotalTokens));
    map.insert("keep_anchors".to_string(),   VmValue::NativeFn(NativeFnId::LlmKeepAnchors));
    map.insert("model".to_string(),          VmValue::NativeFn(NativeFnId::LlmModel));
    map.insert("profile".to_string(),        VmValue::NativeFn(NativeFnId::LlmProfile));
    map.insert("health".to_string(),         VmValue::NativeFn(NativeFnId::LlmHealth));
    map.insert("find_tool_call".to_string(), VmValue::NativeFn(NativeFnId::LlmFindToolCall));
    map.insert("find_tool_calls".to_string(), VmValue::NativeFn(NativeFnId::LlmFindToolCalls));
    map.insert("tool_grammar".to_string(),   VmValue::NativeFn(NativeFnId::LlmToolGrammar));
    VmValue::Dict(map)
}

// Verify that the Span zero is acceptable for built-in errors.
const _: () = {
    let _s = Span { line: 0, col: 0 };
};
