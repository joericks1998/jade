#[cfg(test)]
mod tests;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext}, vm::{VmValue, value_to_display},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::BuiltinFn;

// ── write ─────────────────────────────────────────────────────────────────────

fn native_write(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: Span { line: 0, col: 0 } });
    }
    crate::stdio::write_str_flush(&value_to_display(&args[0]));
    Ok(VmValue::Nil)
}

pub const WRITE: BuiltinFn = BuiltinFn { name: "write", vm_impl: native_write };

// ── len ───────────────────────────────────────────────────────────────────────

fn native_len(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: Span { line: 0, col: 0 } });
    }
    let n = match &args[0] {
        VmValue::Str(s)   => s.chars().count() as i64,
        VmValue::Array(a) => a.lock().len() as i64,
        VmValue::Dict(d)  => d.len() as i64,
        _ => return Err(JadeError::TypeError { message: "len".to_string(), span: Span { line: 0, col: 0 } }),
    };
    Ok(VmValue::Int(n))
}

pub const LEN: BuiltinFn = BuiltinFn { name: "len", vm_impl: native_len };

// ── input ─────────────────────────────────────────────────────────────────────

fn native_input(args: &[VmValue]) -> Result<VmValue> {
    if args.len() > 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: Span { line: 0, col: 0 } });
    }
    if let Some(prompt) = args.first() {
        match prompt {
            VmValue::Str(s) => crate::stdio::write_str_flush(s),
            _ => return Err(JadeError::TypeError { message: "input".to_string(), span: Span { line: 0, col: 0 } }),
        }
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    Ok(VmValue::Str(line.trim_end_matches('\n').trim_end_matches('\r').to_string().into()))
}

pub const INPUT: BuiltinFn = BuiltinFn { name: "input", vm_impl: native_input };

// ── Type registration ─────────────────────────────────────────────────────────

pub fn register_types(ctx: &mut TypeContext) {
    ctx.define("print".to_string(), JadeType::Fn {
        params: vec![JadeType::Unknown],
        ret: Box::new(JadeType::Nil),
    });
    ctx.define("len".to_string(), JadeType::Fn {
        params: vec![JadeType::Unknown],
        ret: Box::new(JadeType::Int),
    });
    ctx.define("join".to_string(), JadeType::Fn {
        params: vec![JadeType::Unknown],
        ret: Box::new(JadeType::Array(Box::new(JadeType::Unknown))),
    });
    ctx.define("input".to_string(), JadeType::Fn {
        params: vec![JadeType::Unknown],
        ret: Box::new(JadeType::Str),
    });
    ctx.define("write".to_string(), JadeType::Fn {
        params: vec![JadeType::Unknown],
        ret: Box::new(JadeType::Nil),
    });
    ctx.define("stream".to_string(), JadeType::Fn {
        params: vec![JadeType::Unknown],
        ret: Box::new(JadeType::Str),
    });
    ctx.define("route".to_string(), JadeType::Fn {
        params: vec![JadeType::Unknown, JadeType::Unknown],
        ret: Box::new(JadeType::Unknown),
    });
    // Primitive type constructors
    for name in &["int", "float", "bool", "str", "func"] {
        ctx.define(name.to_string(), JadeType::Fn {
            params: vec![JadeType::Unknown],
            ret: Box::new(JadeType::Unknown),
        });
    }
    // nil / None / null literals — all three spellings are valid
    ctx.define("nil".to_string(),  JadeType::Nil);
    ctx.define("None".to_string(), JadeType::Nil);
    ctx.define("null".to_string(), JadeType::Nil);
    // LLM session globals
    ctx.define("__tokens__".to_string(), JadeType::Int);
    ctx.define("__model__".to_string(), JadeType::Str);
    ctx.define("__max_retries__".to_string(), JadeType::Int);
    ctx.define("__retry_log__".to_string(), JadeType::Array(Box::new(JadeType::Unknown)));
    // Grammar global: Grammar.new(pattern) -> Grammar
    ctx.define("Grammar".to_string(), JadeType::Unknown);
}
