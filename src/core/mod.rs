//! The globals every program has without an import: `print`, `len`, `write`,
//! `input`, the task controls, the primitive conversions and `Grammar`.

use crate::compiler::{tir::JadeType, type_infer::TypeContext};

/// Every global a program can name without importing anything.
///
/// The compiled backend checks each global a program reads against what it
/// binds; these are the ones it binds without being told.
pub const GLOBAL_NAMES: &[&str] = &[
    "print",
    "len",
    "write",
    "input",
    "cancelled",
    "max_tasks",
    "set_max_tasks",
    "wait",
    "route",
    "int",
    "float",
    "bool",
    "str",
    "char",
    "func",
    "Grammar",
];

// ── Type registration ─────────────────────────────────────────────────────────

pub fn register_types(ctx: &mut TypeContext) {
    ctx.define(
        "print".to_string(),
        JadeType::Fn { params: vec![JadeType::Unknown], ret: Box::new(JadeType::Nil) },
    );
    ctx.define(
        "len".to_string(),
        JadeType::Fn { params: vec![JadeType::Unknown], ret: Box::new(JadeType::Int) },
    );
    ctx.define(
        "join".to_string(),
        JadeType::Fn {
            params: vec![JadeType::Unknown],
            ret: Box::new(JadeType::Array(Box::new(JadeType::Unknown))),
        },
    );
    ctx.define(
        "input".to_string(),
        JadeType::Fn { params: vec![JadeType::Unknown], ret: Box::new(JadeType::Str) },
    );
    ctx.define(
        "cancelled".to_string(),
        JadeType::Fn { params: vec![], ret: Box::new(JadeType::Bool) },
    );
    ctx.define(
        "max_tasks".to_string(),
        JadeType::Fn { params: vec![], ret: Box::new(JadeType::Int) },
    );
    ctx.define(
        "set_max_tasks".to_string(),
        JadeType::Fn { params: vec![JadeType::Int], ret: Box::new(JadeType::Int) },
    );
    ctx.define(
        "wait".to_string(),
        JadeType::Fn {
            params: vec![JadeType::Array(Box::new(JadeType::Unknown))],
            ret: Box::new(JadeType::Int),
        },
    );
    ctx.define(
        "write".to_string(),
        JadeType::Fn { params: vec![JadeType::Unknown], ret: Box::new(JadeType::Nil) },
    );
    ctx.define(
        "route".to_string(),
        JadeType::Fn {
            params: vec![JadeType::Unknown, JadeType::Unknown],
            ret: Box::new(JadeType::Unknown),
        },
    );
    // Primitive type constructors
    for name in &["int", "float", "bool", "str", "char", "func"] {
        ctx.define(
            name.to_string(),
            JadeType::Fn { params: vec![JadeType::Unknown], ret: Box::new(JadeType::Unknown) },
        );
    }
    // nil / None / null literals — all three spellings are valid
    ctx.define("nil".to_string(), JadeType::Nil);
    ctx.define("None".to_string(), JadeType::Nil);
    ctx.define("null".to_string(), JadeType::Nil);
    // Grammar global: Grammar.new(pattern) -> Grammar
    ctx.define("Grammar".to_string(), JadeType::Unknown);
}
