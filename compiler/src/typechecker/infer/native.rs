//! The static types of calls into native packages.
//!
//! A native package is a shared library a program loads at run time, either a
//! Jade-ABI package that exports `jade_pkg_init` or the shim generated for a
//! plain C library. Its functions trade 16-byte `JadeVal`s, so nothing in the
//! library says what a function takes or returns. The types come from
//! `jade.toml` instead, as design decision 11 in `design/v2-static-values.md`
//! settles:
//!
//! ```toml
//! [lib.handlefix.symbols.open]
//! args = ["str"]
//! ret = "handle<Thing>"
//! ```
//!
//! A C dependency's `symbols` table already describes its C signature, and the
//! Jade signature is derived from it (`pkg::cshim::jade_signature`).
//!
//! A function with no declaration still checks: each argument has the type it
//! has at the call, and the result takes the type its use gives it. Where use
//! does not settle the result, the static backend names the declaration to
//! add.

use std::collections::HashMap;

use super::ty::{FnTy, Ty};

/// Purpose: This struct holds what a native function is declared to take and
/// return.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeSig {
    /// How a program writes the call, `handlefix.open`, for messages.
    pub display: String,
    /// The parameter types, or `None` when only the result is declared. A
    /// `Handle` with an empty name takes a handle of any type.
    pub params: Option<Vec<Ty>>,
    /// The result type, or `None` when it is left to inference.
    pub ret: Option<Ty>,
    /// Why the static backend cannot call it, for a C symbol whose shape it
    /// does not decode yet.
    pub refused: Option<String>,
}

/// Purpose: This function reads one type a `jade.toml` declaration names.
///
/// Inputs:
/// - `s`: the text: `int`, `float`, `bool`, `char`, `str`, `bytes`, `nil`,
///   `json`, `handle<T>`, `handle` for any handle, `[T]` for an array, `{T}`
///   for a dict, `T?` for an optional, or `fn(T, …) -> R` for a callback.
///
/// Outputs: the type, or a message naming what was not understood.
pub fn parse_type(s: &str) -> Result<Ty, String> {
    parse_type_in(s, &HashMap::new())
}

/// Purpose: This function reads one declared type, where a name may also be
/// a class the package's declarations define.
///
/// Inputs:
/// - `s`: the text, as for [`parse_type`], or a class name.
/// - `classes`: each class name the package defines, and the name its class
///   has in the program.
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
    if let Some(inner) = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
        return Ok(Ty::Map(Box::new(parse_type(inner)?)));
    }
    if let Some(rest) = s.strip_prefix("fn(") {
        let close = matching_paren(rest).ok_or_else(|| format!("'{s}' has no closing ')'"))?;
        let params = split_top(&rest[..close])
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .map(parse_type)
            .collect::<Result<Vec<_>, _>>()?;
        let after = rest[close + 1..].trim();
        let ret = match after.strip_prefix("->") {
            Some(r) => parse_type(r)?,
            None if after.is_empty() => Ty::Nil,
            None => return Err(format!("'{s}' has '{after}' after its parameters; write '-> T'")),
        };
        return Ok(Ty::Fn(Box::new(FnTy { params, ret, is_async: false })));
    }
    if s == "handle" {
        return Ok(Ty::Handle(String::new()));
    }
    if let Some(name) = s.strip_prefix("handle<").and_then(|r| r.strip_suffix('>')) {
        // The C type as the library spells it, which may be `struct _GIConv`.
        let name = name.trim();
        if name.is_empty() || name.contains(['<', '>']) {
            return Err(format!("'{s}' needs a C type name inside handle<…>"));
        }
        return Ok(Ty::Handle(name.to_string()));
    }
    Ok(match s {
        "int" => Ty::Int,
        "float" => Ty::Float,
        "bool" => Ty::Bool,
        "char" => Ty::Char,
        "str" => Ty::Str,
        "bytes" => Ty::Bytes,
        "nil" => Ty::Nil,
        "json" => Ty::Json,
        other => {
            return Err(format!(
                "'{other}' is not a type a native declaration can name; use int, float, bool, \
                 char, str, bytes, nil, json, handle<T>, [T], {{T}}, T? or fn(T) -> R"
            ));
        }
    })
}

/// Purpose: This function finds the `)` closing a list whose `(` was just
/// consumed.
fn matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// Purpose: This function splits a list on the commas not nested in brackets.
fn split_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            // `->` is not a closing bracket.
            '>' if i == 0 || !s[..i].ends_with('-') => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Purpose: This function reads a whole declaration.
///
/// Inputs:
/// - `display`: how the function is written in a program.
/// - `args`: the declared parameter types, if any.
/// - `ret`: the declared result type, if any.
///
/// Outputs: the signature, or a message naming the function and the type.
pub fn parse_sig(
    display: &str,
    args: Option<&[String]>,
    ret: Option<&str>,
) -> Result<NativeSig, String> {
    parse_sig_in(display, args, ret, &HashMap::new())
}

/// Purpose: This function reads a whole declaration whose types may name the
/// package's own classes.
///
/// Inputs:
/// - `display`, `args`, `ret`: as for [`parse_sig`].
/// - `classes`: as for [`parse_type_in`].
///
/// Outputs: the signature, or a message naming the function and the type.
pub fn parse_sig_in(
    display: &str,
    args: Option<&[String]>,
    ret: Option<&str>,
    classes: &HashMap<String, String>,
) -> Result<NativeSig, String> {
    let wrap = |e: String| format!("the declaration of {display} in jade.toml: {e}");
    let params = match args {
        Some(a) => Some(
            a.iter()
                .map(|t| parse_type_in(t, classes))
                .collect::<Result<Vec<_>, _>>()
                .map_err(wrap)?,
        ),
        None => None,
    };
    let ret = match ret {
        Some(r) => Some(parse_type_in(r, classes).map_err(wrap)?),
        None => None,
    };
    Ok(NativeSig { display: display.to_string(), params, ret, refused: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types_read_as_the_checkers_own() {
        assert_eq!(parse_type("handle<Thing>").unwrap(), Ty::Handle("Thing".into()));
        assert_eq!(parse_type("[int]").unwrap(), Ty::Array(Box::new(Ty::Int)));
        assert_eq!(parse_type("str?").unwrap(), Ty::Opt(Box::new(Ty::Str)));
        assert_eq!(
            parse_type("fn(int, str) -> int").unwrap(),
            Ty::Fn(Box::new(FnTy {
                params: vec![Ty::Int, Ty::Str],
                ret: Ty::Int,
                is_async: false
            }))
        );
        assert_eq!(
            parse_type("fn(fn(int) -> int)").unwrap(),
            Ty::Fn(Box::new(FnTy {
                params: vec![Ty::Fn(Box::new(FnTy {
                    params: vec![Ty::Int],
                    ret: Ty::Int,
                    is_async: false
                }))],
                ret: Ty::Nil,
                is_async: false
            }))
        );
        assert_eq!(
            parse_type("handle<struct _GIConv>").unwrap(),
            Ty::Handle("struct _GIConv".into())
        );
        assert!(parse_type("pointer").is_err());
    }
}
