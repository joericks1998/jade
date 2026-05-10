use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::{NativeFnId, VmValue}},
    frontend::error::{Result, Span},
};

use super::{BuiltinFn, Package};

// `llm.set_max_tokens` is state-mutating so it cannot be a pure BuiltinFn.
// We expose a sentinel BuiltinFn that is never called directly — the VM's
// NativeFn(NativeFnId::LlmSetMaxTokens) variant is what actually runs.
// The Package::vm_dict_value() override below injects the right VmValue.

fn unreachable_set_max_tokens(_args: &[VmValue]) -> Result<VmValue> {
    unreachable!("llm.set_max_tokens is handled by NativeFnId dispatch in the VM")
}

static LLM_FNS: [BuiltinFn; 1] = [
    BuiltinFn { name: "set_max_tokens", vm_impl: unreachable_set_max_tokens },
];

fn register_llm_types(ctx: &mut TypeContext) {
    let mut methods = std::collections::HashMap::new();
    methods.insert("set_max_tokens".to_string(), JadeType::Fn {
        params: vec![JadeType::Int],
        ret: Box::new(JadeType::Nil),
    });
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
    map.insert(
        "set_max_tokens".to_string(),
        VmValue::NativeFn(NativeFnId::LlmSetMaxTokens),
    );
    VmValue::Dict(map)
}

// Verify that the Span zero is acceptable for built-in errors.
const _: () = {
    let _s = Span { line: 0, col: 0 };
};
