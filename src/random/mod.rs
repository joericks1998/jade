use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext}, vm::VmValue,
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

#[cfg(test)]
mod tests;

const ZERO: Span = Span { line: 0, col: 0 };

// The RNG (xoshiro256**) lives in the shared crate (jade_runtime::randomf), so the
// VM and AOT share one implementation. Sequences are the same on both engines;
// they differ from the pre-unification StdRng — random is non-reproducible across
// versions by design (only distributions/invariants are guaranteed).

/// `random.int(min, max)` — random integer in `[min, max]` inclusive.
fn random_int(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 2 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let lo = match &args[0] {
        VmValue::Int(i) => *i,
        _ => return Err(JadeError::TypeError { message: "random.int".to_string(), span: ZERO }),
    };
    let hi = match &args[1] {
        VmValue::Int(i) => *i,
        _ => return Err(JadeError::TypeError { message: "random.int".to_string(), span: ZERO }),
    };
    if lo > hi {
        return Err(JadeError::IoError {
            message: format!("random.int: min ({}) > max ({})", lo, hi),
            span: ZERO,
        });
    }
    Ok(VmValue::Int(jade_runtime::randomf::draw(lo, hi)))
}

/// `random.float()` — random float in `[0.0, 1.0)`.
fn random_float(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    Ok(VmValue::Float(jade_runtime::randomf::float()))
}

/// `random.choice(arr)` — random element from an array. Raises on empty array.
fn random_choice(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    match &args[0] {
        VmValue::Array(arc) => {
            let guard = arc.lock();
            if guard.is_empty() {
                return Err(JadeError::IoError {
                    message: "random.choice: array is empty".to_string(),
                    span: ZERO,
                });
            }
            let idx = jade_runtime::randomf::draw(0, guard.len() as i64 - 1) as usize;
            Ok(guard[idx].clone())
        }
        _ => Err(JadeError::TypeError { message: "random.choice".to_string(), span: ZERO }),
    }
}

/// `random.shuffle(arr)` — shuffles array in-place, returns nil.
fn random_shuffle(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    match &args[0] {
        VmValue::Array(arc) => {
            let mut guard = arc.lock();
            let len = guard.len();
            // Fisher-Yates via the shared RNG (inclusive [0, i]).
            for i in (1..len).rev() {
                let j = jade_runtime::randomf::draw(0, i as i64) as usize;
                guard.swap(i, j);
            }
            Ok(VmValue::Nil)
        }
        _ => Err(JadeError::TypeError { message: "random.shuffle".to_string(), span: ZERO }),
    }
}

/// `random.seed(n)` — reseed the global RNG with an integer.
fn random_seed(args: &[VmValue]) -> Result<VmValue> {
    if args.len() != 1 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let n = match &args[0] {
        VmValue::Int(i) => *i,
        _ => return Err(JadeError::TypeError { message: "random.seed".to_string(), span: ZERO }),
    };
    jade_runtime::randomf::seed(n);
    Ok(VmValue::Nil)
}

static RANDOM_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "int",     vm_impl: random_int },
    BuiltinFn { name: "float",   vm_impl: random_float },
    BuiltinFn { name: "choice",  vm_impl: random_choice },
    BuiltinFn { name: "shuffle", vm_impl: random_shuffle },
    BuiltinFn { name: "seed",    vm_impl: random_seed },
];

fn register_random_pkg_types(ctx: &mut TypeContext) {
    ctx.define("random".to_string(), JadeType::Unknown);
}

pub static RANDOM_PKG: Package = Package {
    import_name: "std/random",
    global_name: "random",
    fns: RANDOM_PKG_FNS,
    register_types: register_random_pkg_types,
};
