//! The checker's types, and the store that lets a type be worked out over time.
//!
//! A type is either concrete (`Int`, `Array(Str)`) or a *slot* (`Var`) whose type
//! is not known yet. Slots live in [`Store`]. Every variable, array element, dict
//! value and struct field is a slot, so a program can say what a value is in one
//! place and use it in another: `let xs = []` makes an array whose element slot
//! is empty, and a later `xs.push(1)` fills it with `Int`.
//!
//! Two types meet through [`Store::join`], which answers the one type both fit
//! in or an error. `Int` and `Int` join to `Int`. `T` and `Nil` join to `T?`.
//! Two struct types join to their nearest common parent. A slot joined with a
//! type takes that type, and a slot that already has one widens to the join of
//! the two. Joining never narrows a slot, which is what lets the checker repeat
//! its passes until nothing changes.

use std::collections::HashMap;
use std::fmt;

/// Purpose: This enum lists the types the checker gives a value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Char,
    Str,
    Bytes,
    Nil,
    /// A value that may be absent: `T` joined with `nil`. Never wraps `Nil`,
    /// another `Opt`, or `Json`, which has a null of its own.
    Opt(Box<Ty>),
    Array(Box<Ty>),
    /// A dict: string keys, one value type.
    Map(Box<Ty>),
    /// A struct, by its (possibly mangled) type name. A value of a parent type
    /// may hold any of its children; see [`Store::join`].
    Struct(String),
    /// A function value of a known shape: the join of two different functions,
    /// or a function stored in a container. Its members are the named
    /// functions that flow into it, tracked by the checker.
    Fn(Box<FnTy>),
    /// A named function or closure used as a value, not yet given argument
    /// types. Calling one through a variable checks it at that call's types,
    /// the way calling it by name does.
    Func(FuncRef),
    Future(Box<Ty>),
    Stream(Box<Ty>),
    Prompt,
    Grammar,
    /// A parsed JSON value. The one built-in value that carries its own kind.
    Json,
    /// The value a catch-all `catch e` binds.
    Error,
    /// An opaque pointer from a C library, named by its C type.
    Handle(String),
    /// A slot in the [`Store`].
    Var(u32),
}

/// Purpose: This struct holds the shape of a function value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FnTy {
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub is_async: bool,
}

/// Purpose: This enum names a function used as a value.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FuncRef {
    /// A top-level `fn` or `async fn`, by its global name.
    Named(String),
    /// A builtin such as `len` or `str`.
    Builtin(String),
}

impl Ty {
    /// Purpose: This function wraps a type as "maybe absent", keeping the
    /// invariant that an optional never wraps nil or another optional.
    ///
    /// Inputs:
    /// - `t`: the type to wrap.
    ///
    /// Outputs: `T?`, or `t` itself when it is already nil or optional.
    pub fn opt(t: Ty) -> Ty {
        match t {
            // JSON already has a null of its own.
            Ty::Nil | Ty::Opt(_) | Ty::Json | Ty::Error => t,
            other => Ty::Opt(Box::new(other)),
        }
    }
}

/// Purpose: This struct holds one slot: its union-find parent and its type.
#[derive(Debug, Clone)]
struct Slot {
    parent: u32,
    ty: Option<Ty>,
}

/// Purpose: This struct holds every slot in a program and the struct
/// hierarchy joins consult.
#[derive(Debug, Default)]
pub struct Store {
    slots: Vec<Slot>,
    /// Bumped whenever a slot gains or widens its type. The checker repeats
    /// its passes until a whole pass leaves it unchanged.
    pub version: u64,
    /// Each struct's ancestors, nearest first, not including itself.
    pub ancestors: HashMap<String, Vec<String>>,
    /// How many parameters each named function or closure takes, and whether
    /// it is async: what joining two of them into one `Fn` type needs.
    pub arities: HashMap<FuncRef, (usize, bool)>,
    /// Each `Fn` type made by joining function values, with the functions that
    /// flowed into it. Once its parameter types settle, the checker checks
    /// each member at them.
    pub fn_members: Vec<(Ty, Vec<FuncRef>)>,
}

/// Purpose: This struct holds why two types do not join.
#[derive(Debug, Clone)]
pub struct Mismatch {
    pub left: Ty,
    pub right: Ty,
    /// True when joining would make a type contain itself, such as an array
    /// pushed into itself.
    pub recursive: bool,
}

impl Store {
    /// Purpose: This function makes a new empty slot.
    ///
    /// Inputs: none.
    ///
    /// Outputs: a `Ty::Var` naming the slot.
    pub fn fresh(&mut self) -> Ty {
        let id = self.slots.len() as u32;
        self.slots.push(Slot { parent: id, ty: None });
        Ty::Var(id)
    }

    /// Purpose: This function makes a slot that already holds a type.
    ///
    /// Inputs:
    /// - `t`: the type the slot starts with.
    ///
    /// Outputs: a `Ty::Var` naming the slot.
    pub fn slot_of(&mut self, t: Ty) -> Ty {
        let v = self.fresh();
        if let Ty::Var(id) = v {
            self.slots[id as usize].ty = Some(t);
        }
        v
    }

    /// Purpose: This function finds the representative slot of a slot.
    ///
    /// Inputs:
    /// - `id`: any slot id.
    ///
    /// Outputs: the id of the slot that holds the type for its whole set.
    fn root(&mut self, id: u32) -> u32 {
        let mut r = id;
        while self.slots[r as usize].parent != r {
            r = self.slots[r as usize].parent;
        }
        let mut cur = id;
        while self.slots[cur as usize].parent != r {
            let next = self.slots[cur as usize].parent;
            self.slots[cur as usize].parent = r;
            cur = next;
        }
        r
    }

    /// Purpose: This function follows a slot to the type it holds, one level.
    ///
    /// Inputs:
    /// - `t`: any type.
    ///
    /// Outputs: the slot's type when `t` is a filled slot, the slot's
    /// representative when it is empty, or `t` unchanged otherwise.
    pub fn shallow(&mut self, t: &Ty) -> Ty {
        match t {
            Ty::Var(id) => {
                let r = self.root(*id);
                match &self.slots[r as usize].ty {
                    Some(inner) => {
                        let inner = inner.clone();
                        self.shallow(&inner)
                    }
                    None => Ty::Var(r),
                }
            }
            other => other.clone(),
        }
    }

    /// Purpose: This function replaces every filled slot in a type by its type,
    /// all the way down.
    ///
    /// Inputs:
    /// - `t`: any type.
    ///
    /// Outputs: the same type with only empty slots left as `Var`.
    pub fn resolve(&mut self, t: &Ty) -> Ty {
        match self.shallow(t) {
            Ty::Opt(a) => Ty::opt(self.resolve(&a)),
            Ty::Array(a) => Ty::Array(Box::new(self.resolve(&a))),
            Ty::Map(a) => Ty::Map(Box::new(self.resolve(&a))),
            Ty::Future(a) => Ty::Future(Box::new(self.resolve(&a))),
            Ty::Stream(a) => Ty::Stream(Box::new(self.resolve(&a))),
            Ty::Fn(f) => Ty::Fn(Box::new(FnTy {
                params: f.params.iter().map(|p| self.resolve(p)).collect(),
                ret: self.resolve(&f.ret),
                is_async: f.is_async,
            })),
            other => other,
        }
    }

    /// Purpose: This function says whether a type still has an empty slot in it.
    ///
    /// Inputs:
    /// - `t`: any type.
    ///
    /// Outputs: true when some part of `t` is not known yet.
    pub fn has_empty_slot(&mut self, t: &Ty) -> bool {
        let r = self.resolve(t);
        contains_var(&r)
    }

    /// Purpose: This function sets a slot's type, counting the change.
    ///
    /// Inputs:
    /// - `root`: a representative slot id.
    /// - `t`: its new type.
    ///
    /// Outputs: none.
    fn set(&mut self, root: u32, t: Ty) {
        if self.slots[root as usize].ty.as_ref() != Some(&t) {
            self.slots[root as usize].ty = Some(t);
            self.version += 1;
        }
    }

    /// Purpose: This function finds the one type two types both fit in, and
    /// widens any slot involved to it.
    ///
    /// Inputs:
    /// - `a`, `b`: the two types.
    ///
    /// Outputs: the joined type, or a [`Mismatch`] naming the two that do not join.
    pub fn join(&mut self, a: &Ty, b: &Ty) -> Result<Ty, Mismatch> {
        let sa = self.shallow(a);
        let sb = self.shallow(b);
        match (&sa, &sb) {
            (Ty::Var(x), Ty::Var(y)) => {
                let (x, y) = (*x, *y);
                if x == y {
                    return Ok(Ty::Var(x));
                }
                // Both empty, since `shallow` stops at an empty slot: merge them.
                self.slots[y as usize].parent = x;
                self.version += 1;
                Ok(Ty::Var(x))
            }
            (Ty::Var(x), other) | (other, Ty::Var(x)) => {
                let x = *x;
                let other = other.clone();
                if self.occurs(x, &other) {
                    return Err(Mismatch { left: sa.clone(), right: sb.clone(), recursive: true });
                }
                // An empty slot: it takes the other type. Keep the slot in the
                // answer so later widening still reaches whoever holds it.
                self.set(x, other);
                Ok(Ty::Var(x))
            }
            _ => {
                let joined = self.join_concrete(&sa, &sb)?;
                // A filled slot on either side widens to the join, unless the
                // join would make the slot contain itself.
                for side in [a, b] {
                    if let Ty::Var(id) = side {
                        let r = self.root(*id);
                        if self.occurs(r, &joined) {
                            return Err(Mismatch {
                                left: sa.clone(),
                                right: sb.clone(),
                                recursive: true,
                            });
                        }
                    }
                }
                for side in [a, b] {
                    if let Ty::Var(id) = side {
                        let r = self.root(*id);
                        self.set(r, joined.clone());
                    }
                }
                Ok(match (a, b) {
                    (Ty::Var(_), _) => a.clone(),
                    (_, Ty::Var(_)) => b.clone(),
                    _ => joined,
                })
            }
        }
    }

    /// Purpose: This function joins two types neither of which is a slot.
    ///
    /// Inputs:
    /// - `a`, `b`: two concrete types.
    ///
    /// Outputs: the joined type, or a [`Mismatch`].
    fn join_concrete(&mut self, a: &Ty, b: &Ty) -> Result<Ty, Mismatch> {
        let mismatch = || Mismatch { left: a.clone(), right: b.clone(), recursive: false };
        Ok(match (a, b) {
            _ if a == b => a.clone(),
            (Ty::Nil, Ty::Opt(_)) | (Ty::Opt(_), Ty::Nil) => {
                if matches!(a, Ty::Opt(_)) {
                    a.clone()
                } else {
                    b.clone()
                }
            }
            (Ty::Nil, t) | (t, Ty::Nil) => Ty::opt(t.clone()),
            (Ty::Opt(x), Ty::Opt(y)) => {
                let j = self.join(x, y).map_err(|_| mismatch())?;
                Ty::opt(j)
            }
            (Ty::Opt(x), t) | (t, Ty::Opt(x)) => {
                let j = self.join(x, t).map_err(|_| mismatch())?;
                Ty::opt(j)
            }
            (Ty::Array(x), Ty::Array(y)) => {
                Ty::Array(Box::new(self.join(x, y).map_err(|_| mismatch())?))
            }
            (Ty::Map(x), Ty::Map(y)) => Ty::Map(Box::new(self.join(x, y).map_err(|_| mismatch())?)),
            (Ty::Future(x), Ty::Future(y)) => {
                Ty::Future(Box::new(self.join(x, y).map_err(|_| mismatch())?))
            }
            (Ty::Stream(x), Ty::Stream(y)) => {
                Ty::Stream(Box::new(self.join(x, y).map_err(|_| mismatch())?))
            }
            (Ty::Struct(x), Ty::Struct(y)) => {
                Ty::Struct(self.common_ancestor(x, y).ok_or_else(mismatch)?)
            }
            (Ty::Fn(f), Ty::Fn(g)) => {
                if f.params.len() != g.params.len() || f.is_async != g.is_async {
                    return Err(mismatch());
                }
                let mut params = Vec::with_capacity(f.params.len());
                for (p, q) in f.params.iter().zip(&g.params) {
                    params.push(self.join(p, q).map_err(|_| mismatch())?);
                }
                let ret = self.join(&f.ret, &g.ret).map_err(|_| mismatch())?;
                let joined = FnTy { params, ret, is_async: f.is_async };
                // Both sides' members now flow into one function type.
                let mut members = self.take_members(&f.ret);
                for m in self.take_members(&g.ret) {
                    if !members.contains(&m) {
                        members.push(m);
                    }
                }
                self.fn_members.push((Ty::Fn(Box::new(joined.clone())), members));
                Ty::Fn(Box::new(joined))
            }
            (Ty::Func(x), Ty::Func(y)) => {
                // Two different functions in one place: they become one function
                // type, and each is checked at its parameter types once known.
                let (nx, ax) = self.arities.get(x).copied().ok_or_else(mismatch)?;
                let (ny, ay) = self.arities.get(y).copied().ok_or_else(mismatch)?;
                if nx != ny || ax != ay {
                    return Err(mismatch());
                }
                let params = (0..nx).map(|_| self.fresh()).collect();
                let ret = self.fresh();
                let ft = Ty::Fn(Box::new(FnTy { params, ret, is_async: ax }));
                self.fn_members.push((ft.clone(), vec![x.clone(), y.clone()]));
                self.version += 1;
                ft
            }
            (Ty::Fn(f), Ty::Func(x)) | (Ty::Func(x), Ty::Fn(f)) => {
                let (n, is_async) = self.arities.get(x).copied().ok_or_else(mismatch)?;
                if n != f.params.len() || is_async != f.is_async {
                    return Err(mismatch());
                }
                let ret = f.ret.clone();
                let mut found = false;
                for i in 0..self.fn_members.len() {
                    let entry_ret = match &self.fn_members[i].0 {
                        Ty::Fn(e) => e.ret.clone(),
                        _ => continue,
                    };
                    if self.same_slot(&entry_ret, &ret) {
                        found = true;
                        if !self.fn_members[i].1.contains(x) {
                            self.fn_members[i].1.push(x.clone());
                            self.version += 1;
                        }
                        break;
                    }
                }
                if !found {
                    self.fn_members.push((Ty::Fn(f.clone()), vec![x.clone()]));
                    self.version += 1;
                }
                Ty::Fn(f.clone())
            }
            _ => return Err(mismatch()),
        })
    }

    /// Purpose: This function says whether a slot appears inside a type.
    ///
    /// Inputs:
    /// - `x`: a representative slot id.
    /// - `t`: a type.
    ///
    /// Outputs: true when filling `x` with `t` would make a type contain itself.
    fn occurs(&mut self, x: u32, t: &Ty) -> bool {
        match t {
            Ty::Var(y) => {
                // Compare before following the slot: the slot being filled may
                // already hold something, and following it would look past itself.
                let ry = self.root(*y);
                if ry == self.root(x) {
                    return true;
                }
                match self.slots[ry as usize].ty.clone() {
                    Some(inner) => self.occurs(x, &inner),
                    None => false,
                }
            }
            Ty::Opt(a) | Ty::Array(a) | Ty::Map(a) | Ty::Future(a) | Ty::Stream(a) => {
                self.occurs(x, a)
            }
            Ty::Fn(f) => f.params.iter().any(|p| self.occurs(x, p)) || self.occurs(x, &f.ret),
            _ => false,
        }
    }

    /// Purpose: This function says whether two types are the same slot.
    ///
    /// Inputs:
    /// - `a`, `b`: two types.
    ///
    /// Outputs: true when both are slots with one representative.
    pub fn same_slot(&mut self, a: &Ty, b: &Ty) -> bool {
        match (a, b) {
            (Ty::Var(x), Ty::Var(y)) => self.root(*x) == self.root(*y),
            _ => false,
        }
    }

    /// Purpose: This function removes and returns the members recorded for the
    /// function type whose return slot is `ret`.
    ///
    /// Inputs:
    /// - `ret`: a function type's return slot.
    ///
    /// Outputs: the functions that flowed into it.
    fn take_members(&mut self, ret: &Ty) -> Vec<FuncRef> {
        for i in 0..self.fn_members.len() {
            let entry_ret = match &self.fn_members[i].0 {
                Ty::Fn(e) => e.ret.clone(),
                _ => continue,
            };
            if self.same_slot(&entry_ret, ret) {
                return self.fn_members.remove(i).1;
            }
        }
        Vec::new()
    }

    /// Purpose: This function finds the nearest struct both types are, or descend from.
    ///
    /// Inputs:
    /// - `a`, `b`: two struct type names.
    ///
    /// Outputs: the nearest common ancestor's name, or `None` when they share none.
    pub fn common_ancestor(&self, a: &str, b: &str) -> Option<String> {
        let line = |n: &str| -> Vec<String> {
            let mut v = vec![n.to_string()];
            v.extend(self.ancestors.get(n).cloned().unwrap_or_default());
            v
        };
        let la = line(a);
        let lb = line(b);
        la.into_iter().find(|x| lb.contains(x))
    }

    /// Purpose: This function says whether one struct is another or descends from it.
    ///
    /// Inputs:
    /// - `child`: the struct that may descend.
    /// - `parent`: the struct it may descend from.
    ///
    /// Outputs: true when `child` is `parent` or one of its descendants.
    pub fn is_a(&self, child: &str, parent: &str) -> bool {
        child == parent || self.ancestors.get(child).is_some_and(|a| a.iter().any(|x| x == parent))
    }

    /// Purpose: This function fills every empty slot with `nil`, once the
    /// passes have settled.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    pub fn default_empty_slots(&mut self) {
        for id in 0..self.slots.len() as u32 {
            let r = self.root(id);
            if self.slots[r as usize].ty.is_none() {
                self.set(r, Ty::Nil);
            }
        }
    }
}

/// Purpose: This function says whether a resolved type still holds a slot.
///
/// Inputs:
/// - `t`: a type from [`Store::resolve`].
///
/// Outputs: true when a `Var` appears anywhere in it.
fn contains_var(t: &Ty) -> bool {
    match t {
        Ty::Var(_) => true,
        Ty::Opt(a) | Ty::Array(a) | Ty::Map(a) | Ty::Future(a) | Ty::Stream(a) => contains_var(a),
        Ty::Fn(f) => f.params.iter().any(contains_var) || contains_var(&f.ret),
        _ => false,
    }
}

impl fmt::Display for Ty {
    /// Purpose: This function writes a type the way error messages name it.
    ///
    /// Inputs:
    /// - `f`: the formatter Rust writes the text into.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Int => write!(f, "int"),
            Ty::Float => write!(f, "float"),
            Ty::Bool => write!(f, "bool"),
            Ty::Char => write!(f, "char"),
            Ty::Str => write!(f, "str"),
            Ty::Bytes => write!(f, "bytes"),
            Ty::Nil => write!(f, "nil"),
            Ty::Opt(t) => write!(f, "{t}?"),
            Ty::Array(t) => write!(f, "[{t}]"),
            Ty::Map(t) => write!(f, "{{str: {t}}}"),
            Ty::Struct(n) => write!(f, "{}", display_name(n)),
            Ty::Fn(ft) => {
                let ps: Vec<String> = ft.params.iter().map(|p| p.to_string()).collect();
                let a = if ft.is_async { "async " } else { "" };
                write!(f, "{a}fn({}) -> {}", ps.join(", "), ft.ret)
            }
            Ty::Func(FuncRef::Named(n)) => write!(f, "fn {}", display_name(n)),
            Ty::Func(FuncRef::Builtin(n)) => write!(f, "builtin {n}"),
            Ty::Future(t) => write!(f, "future of {t}"),
            Ty::Stream(t) => write!(f, "stream of {t}"),
            Ty::Prompt => write!(f, "prompt"),
            Ty::Grammar => write!(f, "grammar"),
            Ty::Json => write!(f, "json"),
            Ty::Error => write!(f, "error"),
            Ty::Handle(n) => write!(f, "handle<{n}>"),
            Ty::Var(_) => write!(f, "an unknown type"),
        }
    }
}

/// Purpose: This function strips the import pass's `$<id>` suffix from a name.
///
/// Inputs:
/// - `n`: a possibly mangled name.
///
/// Outputs: the name the programmer wrote.
pub fn display_name(n: &str) -> &str {
    n.split_once('$').map_or(n, |(base, _)| base)
}
