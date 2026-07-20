use crate::{
    compiler::vm::VmValue,
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::BuiltinFn;

const ZERO: Span = Span { line: 0, col: 0 };

fn grammar_new(args: &[VmValue]) -> Result<VmValue> {
    let pattern = match args.get(0) {
        Some(VmValue::Str(s)) => s.clone(),
        Some(other) => return Err(JadeError::TypeMismatch {
            expected: "str".to_string(),
            got: format!("{:?}", other),
            span: ZERO,
        }),
        None => return Err(JadeError::ArityMismatch { expected: 1, got: 0, span: ZERO }),
    };
    let anchor = match args.get(1) {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Str(s)) => Some(s.clone()),
        Some(other) => return Err(JadeError::TypeMismatch {
            expected: "str".to_string(),
            got: format!("{:?}", other),
            span: ZERO,
        }),
    };
    let stop_anchor = match args.get(2) {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Str(s)) => Some(s.clone()),
        Some(other) => return Err(JadeError::TypeMismatch {
            expected: "str".to_string(),
            got: format!("{:?}", other),
            span: ZERO,
        }),
    };
    Ok(VmValue::Grammar(std::sync::Arc::new(
        jade_runtime::grammarf::GrammarObj::new(pattern, anchor, stop_anchor),
    )))
}

pub static GRAMMAR_NEW: BuiltinFn = BuiltinFn { name: "new", vm_impl: grammar_new };

#[cfg(test)]
mod tests;
