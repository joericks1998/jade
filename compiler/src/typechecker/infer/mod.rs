//! Whole-program type inference for Jade 2.0's static values.
//!
//! The checker works out one concrete type for every value in a program, so the
//! backend can compile ints to registers and structs to fixed layouts with no
//! type tags. It needs no annotations. A function's parameter types come from
//! each call: `add(1, 2)` checks `add`'s body with both parameters `int`, and
//! `add(1.5, 2.0)` checks it again with both `float`. Each distinct set of
//! argument types is an *instance*, and the backend compiles one copy per
//! instance.
//!
//! ## How it settles
//!
//! Types flow in every direction: `let xs = []` learns its element type from a
//! `push` three functions away. So the checker does not try to get everything
//! right in one walk. Every value lives in a slot in [`ty::Store`], slots only
//! ever widen, and the checker repeats a pass over `main` and every instance it
//! reaches until a whole pass changes no slot. Errors are collected only on the
//! last pass, once every type has settled; a type that is merely not known yet
//! on an early pass is not an error.
//!
//! Slots are cached per instance by the address of the syntax node that made
//! them, so a second pass reuses the first pass's slots instead of making new
//! ones. That is what makes "nothing changed" mean the types have settled.
//!
//! ## Where it sits
//!
//! It runs on the whole program after imports are merged and struct defaults
//! are filled, the same point `aot::compile_with_mode` already reaches. Until
//! the static backend replaces the bytecode backend, it is a gate: a program it
//! rejects does not build, even though the bytecode backend could run it.
//! See `design/v2-static-values.md` for the rules it enforces and why.

mod builtins;
mod check;
mod members;
pub mod native;
mod ty;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use crate::frontend::ast::StructFieldDef;
use crate::frontend::error::{JadeError, Span};
use crate::typechecker::tir::{TExpr, TExprKind, TProgram, TStmt};

pub use members::check_members;
use ty::Store;
pub use ty::{FnTy, FuncRef, Ty, display_name as display_type_name};

/// The most instances one function may have. A function that calls itself with
/// a different type every time, such as `f(x)` calling `f([x])`, would make
/// instances forever; this is where that is refused.
const MAX_INSTANCES_PER_FN: usize = 64;

/// The most passes the checker makes before it calls the types unsettled.
const MAX_PASSES: usize = 200;

/// Purpose: This enum names the code an instance checks.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FnKey {
    /// The program's top-level statements.
    Main,
    /// A top-level `fn` or `async fn`, by its global name.
    Named(String),
    /// A method, by the struct whose `extend` block defines it and its name.
    Method(String, String),
}

/// Purpose: This struct holds an instance's identity: the code and the
/// argument types it was checked with.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstKey {
    pub func: FnKey,
    pub args: Vec<Ty>,
}

/// Purpose: This struct holds one function's definition, as the checker reads it.
#[derive(Debug, Clone)]
struct FnInfo<'a> {
    /// The name as the program wrote it, possibly mangled by the import pass.
    name: String,
    params: &'a [(String, Option<TExpr>)],
    body: &'a [TStmt],
    is_async: bool,
    is_generator: bool,
}

/// Purpose: This struct holds what the checker knows about one struct type.
#[derive(Debug, Clone, Default)]
struct StructInfo {
    /// Every field, inherited ones included, in layout order.
    fields: Vec<String>,
    /// Fields declared `prompt`.
    prompt_fields: HashSet<String>,
    /// Direct parents, as written.
    parents: Vec<String>,
    /// Members marked `@public`, or `None` for a built-in struct, whose
    /// members are all public.
    public: Option<HashSet<String>>,
    /// Methods the body declares without defining.
    declared: Vec<String>,
}

/// Purpose: This struct holds one instance's state across passes.
#[derive(Debug, Clone)]
struct Instance {
    /// The parameter types, slots shared with the call that made the instance.
    params: Vec<Ty>,
    /// The return slot. For a generator it holds nil and `elem` is the stream.
    ret: Ty,
    /// For a generator, the slot every `yield` joins into.
    elem: Option<Ty>,
    /// Slots made while checking this instance, by syntax node address.
    slots: HashMap<usize, Ty>,
    /// The pass this instance was last checked in.
    checked_in: usize,
}

/// Purpose: This struct holds what a call or name resolved to, for the backend.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A direct call to one instance.
    Instance(InstKey),
    /// A method call dispatched on the receiver's actual struct type: one
    /// instance per type the receiver may hold.
    Dispatch(Vec<(String, InstKey)>),
    /// A builtin or std package function, by its name.
    Builtin(String),
}

/// Purpose: This struct holds the checker's answer for a whole program.
#[derive(Debug, Default)]
pub struct Typed {
    /// Every instance reached from `main`, with its parameter and return types.
    pub instances: Vec<(InstKey, Vec<Ty>, Ty)>,
    /// The resolved type of every expression, by instance and node address.
    pub types: HashMap<(InstKey, usize), Ty>,
    /// What every call resolved to, by instance and call node address.
    pub targets: HashMap<(InstKey, usize), Target>,
    /// Each global's resolved type.
    pub globals: HashMap<String, Ty>,
    /// Each struct field's resolved type, by struct and field.
    pub fields: HashMap<(String, String), Ty>,
    /// Every slot an instance made, resolved, by instance and the address of
    /// the node that made it. A variable's type is its slot's, which can be
    /// wider than the expression that first set it: `let x = nil` later
    /// assigned `5` is `int?`.
    pub slots: HashMap<(InstKey, usize), Ty>,
    /// Every struct type, built-in ones included, by name.
    pub structs: HashMap<String, StructType>,
    /// Each class that has a destructor chain, with the `drop` instances to run
    /// when a value of it is freed: its own first, then its ancestors', nearest
    /// first, a shared ancestor's once and last.
    pub drops: HashMap<String, Vec<InstKey>>,
    /// The native calls whose result nothing declared and nothing in the
    /// program settled, by instance and call node. The static backend cannot
    /// read such a result, so it names the declaration to add.
    pub unsettled_natives: HashSet<(InstKey, usize)>,
}

/// Purpose: This struct holds one struct type's layout as the backend needs it.
#[derive(Debug, Clone)]
pub struct StructType {
    /// Every field, inherited ones first, with its resolved type. A prompt
    /// field's type is `Ty::Prompt`.
    pub fields: Vec<(String, Ty)>,
    /// Every ancestor, nearest first.
    pub ancestors: Vec<String>,
    /// Direct parents, as written.
    pub parents: Vec<String>,
}

/// Purpose: This struct holds the whole checker's state.
pub struct Checker<'a> {
    store: Store,
    /// What each native function is declared to take and return, by its
    /// `__native$<pkg>$<fn>` name. See `native.rs`.
    natives: HashMap<String, native::NativeSig>,
    /// The result slot of every native call with no declared result, by
    /// instance and call node, to tell which ones use never settled.
    native_results: HashMap<(InstKey, usize), Ty>,
    fns: HashMap<String, FnInfo<'a>>,
    /// Methods by (defining struct, method name).
    methods: HashMap<(String, String), FnInfo<'a>>,
    structs: HashMap<String, StructInfo>,
    /// The slot of each field, by the struct that owns it and its name. An
    /// inherited field shares its owner's slot, so a child and its parent
    /// always agree on a field's type.
    field_slots: HashMap<(String, String), Ty>,
    /// Struct types some literal or decode constructs, for method dispatch.
    constructed: HashSet<String>,
    /// Literal field defaults, joined into their field's slot: a declared
    /// default is a value the field can hold, even where every literal sets it.
    field_defaults: Vec<(String, String, Ty)>,
    /// Global variable slots.
    globals: HashMap<String, Ty>,
    /// Each class member's decorators, with whether the member is a field and
    /// where its class is: `(class, member, decorators, is_field, span)`.
    member_decorators: Vec<(String, String, Vec<String>, bool, Span)>,
    /// Globals bound to the result of a decorator applied to a function.
    decorated: HashMap<String, crate::typechecker::tir::TDecorators>,
    /// Std package names, which a variable or function of the same name shadows.
    packages: HashSet<String>,
    instances: HashMap<InstKey, Instance>,
    instance_count: HashMap<FnKey, usize>,
    worklist: Vec<InstKey>,
    top: &'a [TStmt],
    pass: usize,
    final_pass: bool,
    /// Whether a pass left an `==` between an unknown side and a string or
    /// char, in case the code that settles the side has not been checked yet.
    deferred_eq: bool,
    /// Whether such comparisons make their sides one type now, because the
    /// passes have settled and nothing else will.
    force_eq: bool,
    error: Option<JadeError>,
    out: Typed,
}

/// Purpose: This function checks a whole merged program.
///
/// Inputs:
/// - `program`: the program after imports are merged and struct defaults filled.
///
/// Outputs: the types of everything in it, or the first type error.
pub fn check_program(program: &TProgram) -> Result<Typed, JadeError> {
    check_program_with(program, &HashMap::new())
}

/// Purpose: This function checks a program that calls native packages, with
/// what `jade.toml` declares about their functions.
///
/// Inputs:
/// - `program`: the merged program.
/// - `natives`: each declared native function, by its `__native$<pkg>$<fn>`
///   name.
///
/// Outputs: the checker's answer, as `check_program` gives it.
pub fn check_program_with(
    program: &TProgram,
    natives: &HashMap<String, native::NativeSig>,
) -> Result<Typed, JadeError> {
    let mut c = Checker::new(&program.stmts);
    c.natives = natives.clone();
    c.register(&program.stmts)?;
    c.run()
}

impl<'a> Checker<'a> {
    /// Purpose: This function makes an empty checker for a program's statements.
    ///
    /// Inputs:
    /// - `top`: the program's top-level statements.
    ///
    /// Outputs: the checker.
    fn new(top: &'a [TStmt]) -> Self {
        Checker {
            store: Store::default(),
            natives: HashMap::new(),
            native_results: HashMap::new(),
            fns: HashMap::new(),
            methods: HashMap::new(),
            structs: HashMap::new(),
            field_slots: HashMap::new(),
            constructed: HashSet::new(),
            field_defaults: Vec::new(),
            globals: HashMap::new(),
            decorated: HashMap::new(),
            member_decorators: Vec::new(),
            packages: HashSet::new(),
            instances: HashMap::new(),
            instance_count: HashMap::new(),
            worklist: Vec::new(),
            top,
            pass: 0,
            final_pass: false,
            deferred_eq: false,
            force_eq: false,
            error: None,
            out: Typed::default(),
        }
    }

    /// Purpose: This function records every top-level definition before checking.
    ///
    /// Inputs:
    /// - `stmts`: the program's top-level statements.
    ///
    /// Outputs: `Ok(())`, or an error for a definition the checker cannot accept.
    fn register(&mut self, stmts: &'a [TStmt]) -> Result<(), JadeError> {
        builtins::register_builtin_structs(self);
        // The import pass drops a std `use` once it has checked it, so every
        // package name is available here; a variable of the same name still
        // shadows it.
        for pkg in crate::builtins::all_packages() {
            self.packages.insert(pkg.global_name.to_string());
        }
        for s in stmts {
            match s {
                TStmt::FnDef { name, params, body, decorators, .. }
                | TStmt::AsyncFnDef { name, params, body, decorators, .. } => {
                    let is_async = matches!(s, TStmt::AsyncFnDef { .. });
                    self.store
                        .arities
                        .insert(FuncRef::Named(name.clone()), (params.len(), is_async));
                    self.fns.insert(
                        name.clone(),
                        FnInfo {
                            name: name.clone(),
                            params,
                            body,
                            is_async,
                            is_generator: body_yields(body),
                        },
                    );
                    if !decorators.is_empty() {
                        let slot = self.store.fresh();
                        self.globals.insert(name.clone(), slot);
                        self.decorated.insert(name.clone(), decorators.clone());
                    }
                }
                TStmt::StructDef { name, fields, parents, members, span } => {
                    for (member, decs) in &members.decorators {
                        let is_field = fields.iter().any(|fd| fd.name() == member);
                        self.member_decorators.push((
                            name.clone(),
                            member.clone(),
                            decs.clone(),
                            is_field,
                            *span,
                        ));
                    }
                    for fd in fields {
                        if let StructFieldDef::Let { name: fname, default } = fd
                            && let Some(t) = default_type(default)
                        {
                            self.field_defaults.push((name.clone(), fname.clone(), t));
                        }
                    }
                    let info = StructInfo {
                        fields: fields.iter().map(|f| f.name().to_string()).collect(),
                        prompt_fields: fields
                            .iter()
                            .filter(|f| matches!(f, StructFieldDef::Prompt { .. }))
                            .map(|f| f.name().to_string())
                            .collect(),
                        parents: parents.clone(),
                        public: Some(
                            members
                                .decorators
                                .iter()
                                .filter(|(_, ds)| ds.iter().any(|d| d == "public"))
                                .map(|(m, _)| m.clone())
                                .collect(),
                        ),
                        declared: members.declared.clone(),
                    };
                    self.structs.insert(name.clone(), info);
                }
                TStmt::ExtendBlock { type_name, methods, span, .. } => {
                    for m in methods {
                        if let TStmt::FnDef { name, params, .. }
                        | TStmt::AsyncFnDef { name, params, .. } = m
                            && name == "drop"
                            && (params.len() != 1 || matches!(m, TStmt::AsyncFnDef { .. }))
                        {
                            return Err(JadeError::TypeError {
                                message: format!(
                                    "{}'s destructor `drop` takes no arguments and is not async; \
                                     the language calls it when the last reference goes",
                                    ty::display_name(type_name)
                                ),
                                span: *span,
                            });
                        }
                        if let TStmt::FnDef { name, params, body, .. }
                        | TStmt::AsyncFnDef { name, params, body, .. } = m
                        {
                            self.methods.insert(
                                (type_name.clone(), name.clone()),
                                FnInfo {
                                    name: name.clone(),
                                    params,
                                    body,
                                    is_async: matches!(m, TStmt::AsyncFnDef { .. }),
                                    is_generator: body_yields(body),
                                },
                            );
                        }
                    }
                }
                TStmt::Let { name, .. }
                | TStmt::Assign { name, .. }
                | TStmt::PromptDecl { name, .. }
                    if !self.globals.contains_key(name) =>
                {
                    let slot = self.store.fresh();
                    self.globals.insert(name.clone(), slot);
                }
                _ => {}
            }
        }
        self.build_hierarchy();
        self.apply_member_decorators()
    }

    /// Purpose: This function applies the decorators on class members: a
    /// type decorator gives a field its type, and `@public` was already read.
    ///
    /// Inputs: none.
    ///
    /// Outputs: `Ok(())`, or an error for a decorator a member cannot take.
    fn apply_member_decorators(&mut self) -> Result<(), JadeError> {
        for (class, member, decs, is_field, span) in std::mem::take(&mut self.member_decorators) {
            let c = ty::display_name(&class).to_string();
            for d in decs.iter().filter(|d| *d != "public") {
                let t = match d.as_str() {
                    "int" => Ty::Int,
                    "float" => Ty::Float,
                    "str" => Ty::Str,
                    "bool" => Ty::Bool,
                    "char" => Ty::Char,
                    other if self.structs.contains_key(other) => Ty::Struct(other.to_string()),
                    // A type no program can spell as a decorator, such as `[char]`,
                    // which a native record's class gives its fields.
                    other
                        if let Ok(t) = native::parse_type_in(
                            other,
                            &self.structs.keys().map(|k| (k.clone(), k.clone())).collect(),
                        ) =>
                    {
                        t
                    }
                    other => {
                        return Err(JadeError::TypeError {
                            message: format!(
                                "@{other} on {c}.{member} is not @public or a type; a member takes \
                                 @public, and a field also takes a type such as @int or a class name"
                            ),
                            span,
                        });
                    }
                };
                if !is_field {
                    return Err(JadeError::TypeError {
                        message: format!(
                            "@{d} on the method {c}.{member}: a type decorator goes on a field or a \
                             `let`, and a method takes only @public"
                        ),
                        span,
                    });
                }
                if let Some(slot) = self.field_slot(&class, &member)
                    && self.store.join(&slot, &t).is_err()
                {
                    let have = self.store.resolve(&slot);
                    return Err(JadeError::TypeError {
                        message: format!(
                            "{c}.{member} is declared @{d}, but its default is a {have}"
                        ),
                        span,
                    });
                }
            }
        }
        Ok(())
    }

    /// Purpose: This function works out each struct's ancestors and gives each
    /// field one slot, owned by the struct that declares it.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn build_hierarchy(&mut self) {
        let names: Vec<String> = self.structs.keys().cloned().collect();
        for n in &names {
            let mut line = Vec::new();
            let mut stack: Vec<String> = self.structs[n].parents.clone();
            while let Some(p) = stack.first().cloned() {
                stack.remove(0);
                if line.contains(&p) || &p == n {
                    continue;
                }
                line.push(p.clone());
                if let Some(info) = self.structs.get(&p) {
                    stack.extend(info.parents.clone());
                }
            }
            self.store.ancestors.insert(n.clone(), line);
        }
        for n in &names {
            let fields = self.structs[n].fields.clone();
            for f in fields {
                let owner = self.field_owner(n, &f);
                if !self.field_slots.contains_key(&(owner.clone(), f.clone())) {
                    let slot = self.store.fresh();
                    self.field_slots.insert((owner, f), slot);
                }
            }
        }
        for (s, f, t) in std::mem::take(&mut self.field_defaults) {
            if let Some(slot) = self.field_slot(&s, &f) {
                let _ = self.store.join(&slot, &t);
            }
        }
    }

    /// Purpose: This function finds which struct declares a field: the
    /// furthest ancestor that has it.
    ///
    /// Inputs:
    /// - `s`: the struct the field is read on.
    /// - `field`: the field name.
    ///
    /// Outputs: the owning struct's name, which is `s` when no ancestor has the field.
    fn field_owner(&self, s: &str, field: &str) -> String {
        let ancestors = self.store.ancestors.get(s).cloned().unwrap_or_default();
        ancestors
            .iter()
            .rev()
            .find(|a| self.structs.get(*a).is_some_and(|i| i.fields.iter().any(|f| f == field)))
            .cloned()
            .unwrap_or_else(|| s.to_string())
    }

    /// Purpose: This function returns a struct field's slot, or `None` when the
    /// struct has no such field.
    ///
    /// Inputs:
    /// - `s`: the struct type name.
    /// - `field`: the field name.
    ///
    /// Outputs: the field's slot.
    fn field_slot(&self, s: &str, field: &str) -> Option<Ty> {
        if !self.structs.get(s)?.fields.iter().any(|f| f == field) {
            return None;
        }
        let owner = self.field_owner(s, field);
        self.field_slots.get(&(owner, field.to_string())).cloned()
    }

    /// Purpose: This function runs passes until the types settle, then a final
    /// pass that reports errors and records every type.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the program's types, or its first type error.
    fn run(mut self) -> Result<Typed, JadeError> {
        let mut settled = false;
        for _ in 0..MAX_PASSES {
            let before = self.store.version;
            self.deferred_eq = false;
            self.one_pass();
            if let Some(e) = self.error.take() {
                return Err(e);
            }
            if self.store.version == before {
                // A comparison waited on a side nothing else settled; one
                // more pass makes both sides one type.
                if self.deferred_eq && !self.force_eq {
                    self.force_eq = true;
                    continue;
                }
                settled = true;
                break;
            }
        }
        if !settled {
            return Err(JadeError::TypeError {
                message: "the program's types never settle; a function probably calls itself \
                          with a different type each time"
                    .to_string(),
                span: Span { line: 0, col: 0 },
            });
        }
        // A native result left open would become nil below, which is not what
        // the library returns; record those for the static backend to refuse.
        let open: Vec<(InstKey, usize)> = self
            .native_results
            .clone()
            .into_iter()
            .filter(|(_, t)| self.store.has_empty_slot(t))
            .map(|(k, _)| k)
            .collect();
        self.out.unsettled_natives = open.into_iter().collect();
        // Whatever no part of the program constrained is nil: an empty array
        // nothing pushes into, or a function that only ever raises.
        self.store.default_empty_slots();
        self.final_pass = true;
        self.one_pass();
        if let Some(e) = self.error.take() {
            return Err(e);
        }
        self.finish();
        Ok(self.out)
    }

    /// Purpose: This function checks `main` and every instance it reaches, once each.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn one_pass(&mut self) {
        self.pass += 1;
        let main = InstKey { func: FnKey::Main, args: vec![] };
        if !self.instances.contains_key(&main) {
            let ret = self.store.fresh();
            self.instances.insert(
                main.clone(),
                Instance { params: vec![], ret, elem: None, slots: HashMap::new(), checked_in: 0 },
            );
        }
        self.worklist.push(main);
        self.instantiate_drops();
        loop {
            while let Some(key) = self.worklist.pop() {
                let Some(inst) = self.instances.get_mut(&key) else { continue };
                if inst.checked_in == self.pass {
                    continue;
                }
                inst.checked_in = self.pass;
                self.check_instance(&key);
                if self.error.is_some() {
                    return;
                }
            }
            // Settling a function type checks its members at its parameter
            // types. A member nothing in the program calls directly, such as a
            // function only a C library calls back, is reached only here, so
            // it is checked in this same pass rather than left for the next.
            self.settle_fn_members();
            let pass = self.pass;
            let instances = &self.instances;
            self.worklist.retain(|k| instances.get(k).is_some_and(|i| i.checked_in != pass));
            if self.worklist.is_empty() {
                break;
            }
        }
    }

    /// Purpose: This function lists the `drop` methods a value of a class runs
    /// when it is freed: the class's own, then each ancestor's that defines
    /// one, nearest first.
    ///
    /// Inputs:
    /// - `class`: the class.
    ///
    /// Outputs: the classes whose `drop` runs, in order.
    fn drop_chain(&self, class: &str) -> Vec<String> {
        std::iter::once(class.to_string())
            .chain(self.store.ancestors.get(class).cloned().unwrap_or_default())
            .filter(|c| self.methods.contains_key(&(c.clone(), "drop".to_string())))
            .collect()
    }

    /// Purpose: This function checks every destructor a constructed class will
    /// run. Nothing calls `drop`; the language does, when a value is freed, so
    /// each one is instantiated here for each class that may be freed.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn instantiate_drops(&mut self) {
        let mut kinds: Vec<String> = self.constructed.iter().cloned().collect();
        kinds.sort();
        for kind in kinds {
            for owner in self.drop_chain(&kind) {
                let span = Span { line: 0, col: 0 };
                let _ = self.instantiate(
                    FnKey::Method(owner, "drop".to_string()),
                    vec![Ty::Struct(kind.clone())],
                    span,
                );
            }
        }
    }

    /// Purpose: This function instantiates the functions that flowed into each
    /// joined function type, once its parameter types are known.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn settle_fn_members(&mut self) {
        let entries: Vec<(Ty, Vec<FuncRef>)> = self.store.fn_members.clone();
        for (fn_ty, members) in entries {
            let Ty::Fn(f) = self.store.resolve(&fn_ty) else { continue };
            if f.params.iter().any(|p| self.store.has_empty_slot(p)) {
                continue;
            }
            for m in members {
                let span = Span { line: 0, col: 0 };
                if let Some(r) = self.call_func_ref(&m, f.params.clone(), span, None) {
                    let _ = self.store.join(&f.ret, &r);
                }
            }
        }
    }

    /// Purpose: This function returns the instance for a function at some
    /// argument types, making it the first time, and marks it reached.
    ///
    /// Inputs:
    /// - `func`: the code to instantiate.
    /// - `args`: the argument types, one per parameter, defaults already applied.
    /// - `span`: where the call is, for errors.
    ///
    /// Outputs: the instance's key, or `None` after reporting an error.
    fn instantiate(&mut self, func: FnKey, args: Vec<Ty>, span: Span) -> Option<InstKey> {
        let canon: Vec<Ty> = args.iter().map(|a| self.store.resolve(a)).collect();
        let key = InstKey { func: func.clone(), args: canon };
        if let Some(inst) = self.instances.get(&key) {
            // Share slots with the instance's first caller, so a reference type
            // seen by both is typed the same in both.
            let params = inst.params.clone();
            for (a, p) in args.iter().zip(&params) {
                let _ = self.store.join(a, p);
            }
        } else {
            let n = self.instance_count.entry(func.clone()).or_insert(0);
            *n += 1;
            if *n > MAX_INSTANCES_PER_FN {
                self.report(
                    span,
                    format!(
                        "{} is called with too many different argument types; it probably \
                         calls itself with a different type each time",
                        self.fn_display(&func)
                    ),
                );
                return None;
            }
            let ret = self.store.fresh();
            let elem = self.is_generator(&func).then(|| self.store.fresh());
            self.instances.insert(
                key.clone(),
                Instance { params: args, ret, elem, slots: HashMap::new(), checked_in: 0 },
            );
        }
        if self.instances[&key].checked_in != self.pass {
            self.worklist.push(key.clone());
        }
        Some(key)
    }

    /// Purpose: This function returns the type a call to an instance produces.
    ///
    /// Inputs:
    /// - `key`: the instance.
    ///
    /// Outputs: its return type, wrapped as a future for an async function and
    /// as a stream for a generator.
    fn instance_result(&mut self, key: &InstKey) -> Ty {
        let inst = &self.instances[key];
        let base = match &inst.elem {
            Some(e) => Ty::Stream(Box::new(e.clone())),
            None => inst.ret.clone(),
        };
        if self.is_async(&key.func) { Ty::Future(Box::new(base)) } else { base }
    }

    /// Purpose: This function says whether a function's body yields.
    ///
    /// Inputs:
    /// - `f`: the code.
    ///
    /// Outputs: true for a generator.
    fn is_generator(&self, f: &FnKey) -> bool {
        match f {
            FnKey::Named(n) => self.fns.get(n).is_some_and(|i| i.is_generator),
            FnKey::Method(t, n) => {
                self.methods.get(&(t.clone(), n.clone())).is_some_and(|i| i.is_generator)
            }
            FnKey::Main => false,
        }
    }

    /// Purpose: This function says whether a function was declared `async`.
    ///
    /// Inputs:
    /// - `f`: the code.
    ///
    /// Outputs: true for an async function.
    fn is_async(&self, f: &FnKey) -> bool {
        match f {
            FnKey::Named(n) => self.fns.get(n).is_some_and(|i| i.is_async),
            FnKey::Method(t, n) => {
                self.methods.get(&(t.clone(), n.clone())).is_some_and(|i| i.is_async)
            }
            _ => false,
        }
    }

    /// Purpose: This function names a function the way an error message should.
    ///
    /// Inputs:
    /// - `f`: the code.
    ///
    /// Outputs: a short description.
    fn fn_display(&self, f: &FnKey) -> String {
        match f {
            FnKey::Main => "the program".to_string(),
            FnKey::Named(n) => format!("'{}'", ty::display_name(n)),
            FnKey::Method(t, n) => format!("'{}.{}'", ty::display_name(t), n),
        }
    }

    /// Purpose: This function records a type error, on the final pass only.
    ///
    /// Inputs:
    /// - `span`: where the error is.
    /// - `message`: what is wrong and how to fix it.
    ///
    /// Outputs: none.
    fn report(&mut self, span: Span, message: String) {
        if self.final_pass && self.error.is_none() {
            self.error = Some(JadeError::TypeError { message, span });
        }
    }

    /// Purpose: This function copies the settled types into the answer.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn finish(&mut self) {
        let keys: Vec<InstKey> = self.instances.keys().cloned().collect();
        for k in keys {
            if self.instances[&k].checked_in != self.pass {
                continue;
            }
            let inst = self.instances[&k].clone();
            let params = inst.params.iter().map(|p| self.store.resolve(p)).collect();
            let ret = self.instance_result(&k);
            let ret = self.store.resolve(&ret);
            for (node, slot) in &inst.slots {
                let r = self.store.resolve(slot);
                self.out.slots.insert((k.clone(), *node), r);
            }
            self.out.instances.push((k, params, ret));
        }
        let names: Vec<String> = self.structs.keys().cloned().collect();
        for n in names {
            let info = self.structs[&n].clone();
            let mut fields = Vec::with_capacity(info.fields.len());
            for f in &info.fields {
                let t = if info.prompt_fields.contains(f) {
                    Ty::Prompt
                } else {
                    match self.field_slot(&n, f) {
                        Some(slot) => self.store.resolve(&slot),
                        None => Ty::Nil,
                    }
                };
                fields.push((f.clone(), t));
            }
            let ancestors = self.store.ancestors.get(&n).cloned().unwrap_or_default();
            self.out.structs.insert(n, StructType { fields, ancestors, parents: info.parents });
        }
        let globals: Vec<(String, Ty)> =
            self.globals.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (n, t) in globals {
            let r = self.store.resolve(&t);
            self.out.globals.insert(n, r);
        }
        let mut kinds: Vec<String> = self.constructed.iter().cloned().collect();
        kinds.sort();
        for kind in kinds {
            let chain: Vec<InstKey> = self
                .drop_chain(&kind)
                .into_iter()
                .map(|owner| InstKey {
                    func: FnKey::Method(owner, "drop".to_string()),
                    args: vec![Ty::Struct(kind.clone())],
                })
                .filter(|k| self.instances.contains_key(k))
                .collect();
            if !chain.is_empty() {
                self.out.drops.insert(kind, chain);
            }
        }
        let fields: Vec<((String, String), Ty)> =
            self.field_slots.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (k, t) in fields {
            let r = self.store.resolve(&t);
            self.out.fields.insert(k, r);
        }
    }
}

/// Purpose: This function gives a struct field default's type when it is a
/// plain literal.
///
/// Inputs:
/// - `e`: the default expression, as written.
///
/// Outputs: its type, or `None` for anything that is not a scalar literal.
fn default_type(e: &crate::frontend::ast::Expr) -> Option<Ty> {
    use crate::frontend::ast::Expr;
    match e {
        Expr::Integer { .. } => Some(Ty::Int),
        Expr::Float { .. } => Some(Ty::Float),
        Expr::Str { .. } => Some(Ty::Str),
        Expr::Bool { .. } => Some(Ty::Bool),
        Expr::Identifier { name, .. } if matches!(name.as_str(), "nil" | "None" | "null") => {
            Some(Ty::Nil)
        }
        _ => None,
    }
}

/// Purpose: This function says whether a function body contains a `yield`,
/// outside any closure.
///
/// Inputs:
/// - `body`: the statements.
///
/// Outputs: true when the function is a generator.
fn body_yields(body: &[TStmt]) -> bool {
    body.iter().any(|s| match s {
        TStmt::Yield { .. } => true,
        TStmt::If { then_body, else_body, .. } => {
            body_yields(then_body) || else_body.as_deref().is_some_and(body_yields)
        }
        TStmt::While { body, .. } | TStmt::For { body, .. } => body_yields(body),
        TStmt::TryCatch { body, arms, .. } => {
            body_yields(body) || arms.iter().any(|a| body_yields(&a.body))
        }
        _ => false,
    })
}

/// Purpose: This function gives a syntax node an identity that stays the same
/// across passes.
///
/// Inputs:
/// - `x`: any node of the program, which outlives the checker.
///
/// Outputs: the node's address.
fn addr<T>(x: &T) -> usize {
    x as *const T as usize
}

/// Purpose: This function says whether an expression is the literal `nil`.
///
/// Inputs:
/// - `e`: the expression.
///
/// Outputs: true for `nil`, `None` or `null`.
fn is_nil_literal(e: &TExpr) -> bool {
    matches!(&e.kind, TExprKind::Identifier(n) if n == "nil" || n == "None" || n == "null")
}
