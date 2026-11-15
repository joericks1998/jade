//! The static types of calls into Rust packages.
//!
//! A Rust package's built library describes every function it exports, in Jade's
//! own type spelling (`rustpkg::meta`), so each signature is complete: the checker
//! unifies each argument with its parameter and the call has the declared result.
//! Nothing is left to inference, which is what the C path needed when a library
//! carried no types at all.
//!
//! A record names a struct as `crate::Name`; [`parse_type_in`] turns that into the
//! class the program knows it as (`rustpkg::class_name`).

use std::collections::HashMap;

use super::ty::{SizedNum, Ty};

/// Purpose: This struct holds what a Rust function takes and returns, and the
/// symbol a call goes to.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeSig {
    /// How a program writes the call, `geo.midpoint`, for messages.
    pub display: String,
    /// The `extern "C"` wrapper `#[jade::export]` wrote; its companion that frees
    /// the result is the same name with `__free` added.
    pub symbol: String,
    /// The parameter types, in order.
    pub params: Vec<Ty>,
    /// The result type: `nil` for a function returning `()`.
    pub ret: Ty,
}

/// Purpose: This function reads one type a record names.
///
/// Inputs:
/// - `s`: the text: `int`, `float`, `bool`, `char`, `str`, `bytes`, `nil`, a sized
///   number type such as `uint32`, `[T]` for an array, `T?` for an optional, or a
///   class name.
/// - `classes`: each name a class may be written by, `geo::Point` in a record or
///   `Point$rgeo` in a class's own field, and the class it is.
///
/// Outputs: the type, or a message naming what was not understood.
pub fn parse_type_in(s: &str, classes: &HashMap<String, String>) -> Result<Ty, String> {
    let parse_type = |t: &str| parse_type_in(t, classes);
    let s = s.trim();
    if let Some(class) = classes.get(s) {
        return Ok(Ty::Struct(class.clone()));
    }
    if let Some(inner) = s.strip_suffix('?') {
        return Ok(Ty::opt(parse_type(inner)?));
    }
    if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return Ok(Ty::Array(Box::new(parse_type(inner)?)));
    }
    if let Some(n) = SizedNum::from_name(s) {
        return Ok(Ty::Sized(n));
    }
    Ok(match s {
        "int" | "long" => Ty::Int,
        "float" | "double" => Ty::Float,
        "bool" => Ty::Bool,
        "char" => Ty::Char,
        "str" => Ty::Str,
        "bytes" => Ty::Bytes,
        "nil" => Ty::Nil,
        other => {
            return Err(format!(
                "'{other}' is not a type a Rust package can export; it takes numbers, bool, \
                 char, str, bytes, arrays, optionals and #[jade::export] structs"
            ));
        }
    })
}

/// Purpose: This function makes a function's signature from its record.
///
/// Inputs:
/// - `display`: how the function is written in a program, `geo.midpoint`.
/// - `f`: the record.
/// - `classes`: as for [`parse_type_in`].
///
/// Outputs: the signature, or a message naming the function and the type.
pub fn rust_sig(
    display: &str,
    f: &crate::rustpkg::Function,
    classes: &HashMap<String, String>,
) -> Result<NativeSig, String> {
    let wrap = |e: String| format!("the Rust function {display}: {e}");
    let params = f
        .params
        .iter()
        .map(|p| parse_type_in(&p.ty, classes))
        .collect::<Result<Vec<_>, _>>()
        .map_err(wrap)?;
    let ret = parse_type_in(&f.ret, classes).map_err(wrap)?;
    Ok(NativeSig { display: display.to_string(), symbol: f.symbol.clone(), params, ret })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_types_read_as_the_checkers_own() {
        let classes: HashMap<String, String> =
            [("geo::Point".to_string(), "Point$rgeo".to_string())].into_iter().collect();
        let t = |s: &str| parse_type_in(s, &classes).unwrap();
        assert_eq!(t("[str]"), Ty::Array(Box::new(Ty::Str)));
        assert_eq!(t("int?"), Ty::Opt(Box::new(Ty::Int)));
        assert_eq!(t("[ulong]"), Ty::Array(Box::new(Ty::Sized(SizedNum::ULong))));
        assert_eq!(t("geo::Point"), Ty::Struct("Point$rgeo".into()));
        assert_eq!(
            t("[geo::Point]?"),
            Ty::Opt(Box::new(Ty::Array(Box::new(Ty::Struct("Point$rgeo".into())))))
        );
        assert!(parse_type_in("other::Thing", &classes).is_err());
        assert!(parse_type_in("handle<Thing>", &classes).is_err());
    }
}
