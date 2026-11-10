//! Inference: compiling a prompt dereference into a program that runs a model.
//!
//! A program that writes `?p` runs a language model on the GPU, over memory the
//! dovata process holds and hands out. Nothing of it is a runtime library. At
//! build time the compiler asks dovata for the model's plan, which says where
//! every tensor sits, which ops make one forward step, and how the model
//! tokenizes text; it writes GPU kernels for those ops and a table of them into
//! the program. `src/infer/README.md` says how the pieces fit.
//!
//! - `rewrite.rs` turns each `?p` into a call into `prelude.jde`, the Jade
//!   source that tokenizes text and decodes the reply, before the program is
//!   checked.
//! - `plan.rs` reads the plan dovata gives and builds what the backend emits:
//!   the op table, the tokenizer's tables and the inputs each step fills.
//! - `dovata.rs` asks the dovata process for a model's plan.
//! - `kernels.metal` holds the Metal kernels, one per op.
//! - `unicode.rs` holds the character classes the tokenizer splits text by.
//!
//! The backend half, which maps the memory, drives Metal and runs the
//! generation loop, is `src/backend/infer.rs`.

pub mod dovata;
pub mod plan;
pub mod rewrite;
pub mod unicode;

use crate::builtins::Package;
use crate::compiler::tir::JadeType;
use crate::compiler::type_infer::TypeContext;

/// Purpose: This function registers the internal package's name.
///
/// Inputs:
/// - `ctx`: the per-file checker's context.
///
/// Outputs: none.
fn register_infer_pkg_types(ctx: &mut TypeContext) {
    ctx.define("__infer".to_string(), JadeType::Unknown);
}

/// The prelude's own package: what `prelude.jde` asks of the backend. It is
/// not documented for programs; its name keeps it out of their way.
pub static INFER_PKG: Package = Package {
    import_name: "std/__infer",
    global_name: "__infer",
    fns: &[
        "data",
        "config",
        "char_class",
        "taint",
        "text",
        "ready",
        "dovata_map",
        "map_region",
        "setup",
        "generate",
    ],
    register_types: register_infer_pkg_types,
};
