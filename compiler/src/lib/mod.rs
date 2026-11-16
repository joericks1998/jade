//! The std library written in Jade: each package's source, embedded in the
//! compiler, and what a build needs to know about it.
//!
//! A package listed here is a `.jde` file beside this one, compiled with any
//! program that uses it the way `infer/prelude.jde` is. `use std::path` loads
//! `path.jde` into the program, in a namespace of its own whose id is reserved
//! for the package, so `path.join(a, b)` calls the module's `join` and no name
//! in it can meet one of the program's. The import pass in `mir/imports.rs`
//! does the loading; the checker types the module's functions by reading their
//! bodies, as it does any imported file's.
//!
//! What Jade cannot do on its own, such as reading a clock or calling an LLVM
//! intrinsic, a library module asks of `__sys`, the hook package: see
//! `SysFn` in `mir/mod.rs`. Only library modules can name it.
//!
//! `README.md` beside this file says how to add a package.

use std::sync::OnceLock;

use crate::frontend::ast::Stmt;

/// The first namespace id the library's packages are mangled with, as
/// `name$<id>`. Far above any id the import pass hands out, and below the
/// inference prelude's, so none of them meet.
const LIB_BASE: u32 = u32::MAX - 1024;

/// Purpose: This struct describes one std package written in Jade.
pub struct Library {
    /// The name a program writes after `std::`, which is also the name `use`
    /// binds: `path`.
    pub name: &'static str,
    /// The module's source.
    pub source: &'static str,
    /// Functions a call with more arguments than two folds left into, the
    /// way a variadic function would: `join(a, b, c)` is `join(join(a, b), c)`.
    pub folds: &'static [&'static str],
}

/// Every std package written in Jade. A package's index here gives its
/// namespace id, so a new one goes at the end.
static LIBRARIES: &[Library] = &[
    Library { name: "path", source: include_str!("path.jde"), folds: &["join"] },
    Library { name: "time", source: include_str!("time.jde"), folds: &[] },
    Library { name: "math", source: include_str!("math.jde"), folds: &[] },
    Library { name: "env", source: include_str!("env.jde"), folds: &[] },
    Library { name: "fs", source: include_str!("fs.jde"), folds: &[] },
    Library { name: "sh", source: include_str!("sh.jde"), folds: &[] },
    Library { name: "random", source: include_str!("random.jde"), folds: &[] },
    Library { name: "http", source: include_str!("http.jde"), folds: &[] },
    Library { name: "uhttp", source: include_str!("uhttp.jde"), folds: &[] },
];

/// The hook package's name. Library modules write `use std::__sys`; a program
/// that names it is told the name is undefined.
pub const SYS: &str = "__sys";

impl Library {
    /// Purpose: This function returns the namespace id the package's names
    /// are mangled with.
    pub fn id(&self) -> u32 {
        let index = LIBRARIES.iter().position(|l| std::ptr::eq(l, self)).unwrap_or(0);
        LIB_BASE + index as u32
    }

    /// Purpose: This function returns the path a `use` names the package by,
    /// as the parser spells it: `std/path`.
    pub fn import_name(&self) -> String {
        format!("std/{}", self.name)
    }

    /// Purpose: This function lists the functions a program can call, in the
    /// order the source defines them: every top-level `fn` whose name does
    /// not start with `_`. A name starting with `_` is the module's own.
    ///
    /// Outputs: the names, read from the source the first time they are asked
    /// for. A source that does not parse has none; building with it reports why.
    pub fn public_fns(&self) -> &'static [String] {
        static PUBLIC: OnceLock<Vec<Vec<String>>> = OnceLock::new();
        let all = PUBLIC.get_or_init(|| LIBRARIES.iter().map(read_public_fns).collect());
        let index = (self.id() - LIB_BASE) as usize;
        all.get(index).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Purpose: This function says whether a program may call a function.
    pub fn has_fn(&self, name: &str) -> bool {
        self.public_fns().iter().any(|f| f == name)
    }

    /// Purpose: This function parses the module and runs the per-file pass
    /// over it, in the mode that lets it name `__sys`.
    ///
    /// Outputs: its statements, not yet namespaced, or why it does not
    /// compile, which is a compiler bug.
    pub fn statements(&self) -> Result<Vec<Stmt>, String> {
        let fail = |e: crate::frontend::error::JadeError| {
            format!("internal compiler error in std::{}: {e}", self.name)
        };
        let tokens = crate::frontend::lexer::tokenize(self.source).map_err(fail)?;
        let ast = crate::frontend::parser::parse(tokens).map_err(fail)?;
        let tree = crate::typechecker::resolve::resolve_library(ast).map_err(fail)?;
        Ok(tree.stmts)
    }
}

/// Purpose: This function reads a module's public function names.
fn read_public_fns(lib: &Library) -> Vec<String> {
    let Ok(tokens) = crate::frontend::lexer::tokenize(lib.source) else { return Vec::new() };
    let Ok(ast) = crate::frontend::parser::parse(tokens) else { return Vec::new() };
    ast.stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::FnDef { name, .. } | Stmt::AsyncFnDef { name, .. } if !name.starts_with('_') => {
                Some(name.clone())
            }
            _ => None,
        })
        .collect()
}

/// Purpose: This function finds a package by the path a `use` names.
///
/// Inputs:
/// - `import_name`: the path, such as `std/path`.
///
/// Outputs: the package, or `None` when it is not written in Jade.
pub fn find(import_name: &str) -> Option<&'static Library> {
    let name = import_name.strip_prefix("std/")?;
    by_name(name)
}

/// Purpose: This function finds a package by the name `use` binds.
pub fn by_name(name: &str) -> Option<&'static Library> {
    LIBRARIES.iter().find(|l| l.name == name)
}

/// Purpose: This function finds the package a namespace id belongs to.
pub fn by_id(id: u32) -> Option<&'static Library> {
    let index = id.checked_sub(LIB_BASE)? as usize;
    LIBRARIES.get(index)
}

/// Purpose: This function says which package a mangled name belongs to.
///
/// Inputs:
/// - `mangled`: a name as the import pass mangles it, such as `join$4294966271`.
///
/// Outputs: the package and the name as its source writes it, or `None` for
/// a name that is not the library's.
pub fn owner(mangled: &str) -> Option<(&'static Library, &str)> {
    let (base, id) = mangled.split_once('$')?;
    let lib = by_id(id.parse().ok()?)?;
    Some((lib, base))
}

/// Purpose: This function names a library function the way a program calls
/// it, for messages.
///
/// Inputs:
/// - `mangled`: the function's mangled name.
///
/// Outputs: `path.join`, or `None` for a name that is not the library's.
pub fn display(mangled: &str) -> Option<String> {
    owner(mangled).map(|(lib, base)| format!("{}.{base}", lib.name))
}

/// Purpose: This function says whether a name belongs to library code.
pub fn is_library_name(mangled: &str) -> bool {
    owner(mangled).is_some()
}

/// Purpose: This function says whether a mangled name is a library function
/// that folds a long call into nested two-argument ones.
pub fn folds(mangled: &str) -> bool {
    owner(mangled).is_some_and(|(lib, base)| lib.folds.contains(&base))
}

/// Purpose: This function removes a second copy of a library module from a
/// merged program, keeping the first. A module loaded once for the program
/// and once for the inference prelude must define its names once.
///
/// Inputs:
/// - `stmts`: the merged program.
///
/// Outputs: the program, each library definition once.
pub fn dedupe(stmts: Vec<Stmt>) -> Vec<Stmt> {
    let mut seen = std::collections::HashSet::new();
    stmts
        .into_iter()
        .filter(|s| {
            let key = match s {
                Stmt::FnDef { name, .. }
                | Stmt::AsyncFnDef { name, .. }
                | Stmt::StructDef { name, .. }
                | Stmt::Let { name, .. } => name.clone(),
                // A class's methods, by the class and the methods' names.
                Stmt::ExtendBlock { type_name, methods, .. } => {
                    let names: Vec<&str> = methods
                        .iter()
                        .filter_map(|m| match m {
                            Stmt::FnDef { name, .. } | Stmt::AsyncFnDef { name, .. } => {
                                Some(name.as_str())
                            }
                            _ => None,
                        })
                        .collect();
                    format!("{type_name}::{}", names.join(","))
                }
                _ => return true,
            };
            let name = key.split("::").next().unwrap_or_default();
            !is_library_name(name) || seen.insert(key)
        })
        .collect()
}

#[cfg(test)]
mod tests;
