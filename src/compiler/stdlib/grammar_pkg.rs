use crate::{
    compiler::vm::VmValue,
    frontend::error::{JadeError, Result, Span},
};

use super::BuiltinFn;

const ZERO: Span = Span { line: 0, col: 0 };

fn grammar_new(args: &[VmValue]) -> Result<VmValue> {
    match args.get(0) {
        Some(VmValue::Str(pattern)) => Ok(VmValue::Grammar(pattern.clone())),
        Some(other) => Err(JadeError::TypeMismatch {
            expected: "str".to_string(),
            got: format!("{:?}", other),
            span: ZERO,
        }),
        None => Err(JadeError::ArityMismatch { expected: 1, got: 0, span: ZERO }),
    }
}

pub static GRAMMAR_NEW: BuiltinFn = BuiltinFn { name: "new", vm_impl: grammar_new };
