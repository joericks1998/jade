use std::sync::OnceLock;

use parking_lot::Mutex;
use rand::{rngs::StdRng, Rng, SeedableRng};

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

static RNG: OnceLock<Mutex<StdRng>> = OnceLock::new();

fn get_rng() -> &'static Mutex<StdRng> {
    RNG.get_or_init(|| Mutex::new(StdRng::from_entropy()))
}

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
    let n = get_rng().lock().gen_range(lo..=hi);
    Ok(VmValue::Int(n))
}

/// `random.float()` — random float in `[0.0, 1.0)`.
fn random_float(args: &[VmValue]) -> Result<VmValue> {
    if !args.is_empty() {
        return Err(JadeError::ArityMismatch { expected: 0, got: args.len(), span: ZERO });
    }
    let f: f64 = get_rng().lock().r#gen();
    Ok(VmValue::Float(f))
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
            let idx = get_rng().lock().gen_range(0..guard.len());
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
            let mut rng = get_rng().lock();
            let len = guard.len();
            // Fisher-Yates via rand's gen_range.
            for i in (1..len).rev() {
                let j = rng.gen_range(0..=i);
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
        VmValue::Int(i) => *i as u64,
        _ => return Err(JadeError::TypeError { message: "random.seed".to_string(), span: ZERO }),
    };
    *get_rng().lock() = StdRng::seed_from_u64(n);
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
