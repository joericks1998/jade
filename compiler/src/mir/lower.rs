//! Lowering the checked program into MIR.
//!
//! The checker's answer (`typechecker::infer::Typed`) says, for every instance it
//! reached, the type of every expression and slot and the target of every call,
//! keyed by the address of the syntax node. This walks each instance's body
//! once with those answers in hand and writes MIR: locals for variables and
//! temporaries, blocks for control flow, and an explicit conversion wherever a
//! value meets a slot of a wider type, such as an `int` stored in an `int?`.
//!
//! Anything the static backend does not compile yet stops here with
//! `JadeError::NotYetStatic`, naming the feature. The phases that add them are
//! listed in `design/v2-static-values.md`.

use std::collections::{HashMap, HashSet};

use super::*;
use crate::frontend::ast::{self, Expr, ExprKind, FStrPart};
use crate::frontend::ast::{BinOpKind, UnaryOpKind};
use crate::frontend::error::{JadeError, Span};
use crate::typechecker::infer::{FnKey, InstKey, StaticTest, Target, Typed};

type Result<T> = std::result::Result<T, JadeError>;

/// Purpose: This struct holds one function definition as the lowering reads it.
#[derive(Clone, Copy)]
pub(super) struct FnDef<'a> {
    pub params: &'a [(String, Option<Expr>)],
    pub body: &'a [ast::Stmt],
    pub is_async: bool,
}

/// Purpose: This struct holds what every function's lowering shares.
pub(super) struct Lowerer<'a> {
    top: &'a [ast::Stmt],
    /// What each Rust package function takes and returns, by
    /// `__native$<pkg>$<fn>`.
    native_sigs: HashMap<String, crate::typechecker::infer::native::NativeSig>,
    fns: HashMap<&'a str, FnDef<'a>>,
    methods: HashMap<(&'a str, &'a str), FnDef<'a>>,
    /// Each class field's declared default, by class and field, for a
    /// copy-with literal whose base's class lacks the field.
    defaults: HashMap<(&'a str, &'a str), &'a Expr>,
    func_ids: HashMap<InstKey, FuncId>,
    /// Each instance's parameter and return types.
    sigs: HashMap<InstKey, (Vec<Ty>, Ty)>,
    exprs: HashMap<InstKey, HashMap<usize, Ty>>,
    targets: HashMap<InstKey, HashMap<usize, Target>>,
    slots: HashMap<InstKey, HashMap<usize, Ty>>,
    pub(super) globals: HashMap<String, (GlobalId, Ty)>,
    pub(super) struct_ids: HashMap<String, u32>,
    pub(super) structs: Vec<StructDef>,
    pub(super) packages: HashSet<String>,
    /// Functions made while lowering, after the instances: adapters, which let
    /// a named function stand behind a joined function type's pointer, and
    /// each pipe's stages and generator copy.
    extra: std::cell::RefCell<Vec<Extra>>,
    /// How many instances there are, so a made function's id follows them.
    n_instances: usize,
    /// Each pipe over a call to a generator, by the instance it is in and its
    /// address: the function holding its stages and the generator's copy that
    /// calls them. Made once, so a generator that pipes itself terminates.
    pub(super) pipes: std::cell::RefCell<HashMap<(InstKey, usize), (FuncId, FuncId)>>,
    /// Every expression the checker typed as a stream, by instance and
    /// address. MIR's types read a stream as an array, and a pipe needs to
    /// know which it has.
    pub(super) streams: HashSet<(InstKey, usize)>,
}

/// Purpose: This struct holds one function made while lowering.
struct Extra {
    /// For an adapter, the named function and the joined type it stands
    /// behind, so a second request finds it.
    adapter: Option<(String, crate::typechecker::infer::FnTy)>,
    func: Func,
}

/// Purpose: This struct describes a generator's copy made for a pipe, in
/// which each `yield` calls the pipe's stages on the value.
pub(super) struct Sink {
    /// The copy's name.
    pub name: String,
    /// The function holding the stages, called with the piece and then the
    /// captured values.
    pub stages: FuncId,
    /// The piece's type, the stages function's first parameter.
    pub piece: Ty,
    /// The captured values' types, passed after the generator's own
    /// parameters.
    pub captured: Vec<Ty>,
    /// What the stages give back, collected into the copy's result.
    pub out: Ty,
}

/// Purpose: This struct holds, while a generator's copy is lowered, what each
/// `yield` hands its value to.
#[derive(Clone)]
pub(super) struct FxSink {
    pub stages: FuncId,
    pub piece: Ty,
    pub captured: Vec<Local>,
}

/// Purpose: This function lowers a whole checked program into MIR.
///
/// Inputs:
/// - `program`: the merged program the checker checked.
/// - `typed`: the checker's answer for it.
///
/// Outputs: the MIR program, or the first feature the static backend cannot
/// compile yet.
pub fn lower_program(program: &ast::Program, typed: &Typed) -> Result<Program> {
    lower_program_with(program, typed, None, &HashMap::new())
}

/// Purpose: This function lowers a program that calls Rust packages.
///
/// Inputs:
/// - `program`, `typed`: as for [`lower_program`].
/// - `rust`: the staticlib to link and its system libraries, when the program
///   calls a Rust package.
/// - `sigs`: what each Rust function takes and returns, by
///   `__native$<pkg>$<fn>`.
///
/// Outputs: the MIR program.
pub fn lower_program_with(
    program: &ast::Program,
    typed: &Typed,
    rust: Option<crate::rustpkg::Link>,
    sigs: &HashMap<String, crate::typechecker::infer::native::NativeSig>,
) -> Result<Program> {
    let mut l = Lowerer::new(&program.stmts, typed);
    l.native_sigs = sigs.clone();
    l.collect(&program.stmts);

    let mut keys: Vec<&(InstKey, Vec<Ty>, Ty)> = typed.instances.iter().collect();
    keys.sort_by_key(|(k, _, _)| (k.func != FnKey::Main, inst_label(k)));
    for (i, (k, params, ret)) in keys.iter().enumerate() {
        l.func_ids.insert(k.clone(), FuncId(i as u32));
        l.sigs.insert(k.clone(), (params.iter().map(norm).collect(), norm(ret)));
    }

    let mut funcs = Vec::with_capacity(keys.len());
    for (k, _, _) in &keys {
        funcs.push(l.lower_instance(k)?);
    }
    // Functions asked for while the bodies above were lowered: adapters, and
    // each pipe's stages and generator copy. Their ids follow the instances'.
    for x in l.extra.borrow().iter() {
        funcs.push(x.func.clone());
    }
    let mut globals: Vec<(GlobalId, Global)> = l
        .globals
        .iter()
        .map(|(n, (id, t))| (*id, Global { name: n.clone(), ty: t.clone() }))
        .collect();
    globals.sort_by_key(|(id, _)| id.0);
    let mut drops: Vec<(String, Vec<FuncId>)> = typed
        .drops
        .iter()
        .map(|(class, keys)| {
            (class.clone(), keys.iter().filter_map(|k| l.func_ids.get(k).copied()).collect())
        })
        .collect();
    drops.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Program {
        funcs,
        globals: globals.into_iter().map(|(_, g)| g).collect(),
        structs: l.structs,
        main: FuncId(0),
        drops,
        rust,
        infer: None,
    })
}

/// Purpose: This function names an instance readably and uniquely.
///
/// Inputs:
/// - `k`: the instance.
///
/// Outputs: a label such as `add(int, int)` or `Point.norm(Point)`.
pub(super) fn inst_label(k: &InstKey) -> String {
    let base = match &k.func {
        FnKey::Main => "main".to_string(),
        FnKey::Named(n) => n.clone(),
        FnKey::Method(t, n) => format!("{t}.{n}"),
    };
    let args: Vec<String> = k.args.iter().map(|a| norm(a).to_string()).collect();
    format!("{base}({})", args.join(", "))
}

/// Purpose: This function replaces any slot the checker left empty with `nil`.
///
/// Inputs:
/// - `t`: a resolved type.
///
/// Outputs: the same type with no `Var` in it.
pub fn norm(t: &Ty) -> Ty {
    match t {
        Ty::Var(_) => Ty::Nil,
        Ty::Opt(a) => Ty::opt(norm(a)),
        Ty::Array(a) => Ty::Array(Box::new(norm(a))),
        Ty::Map(a) => Ty::Map(Box::new(norm(a))),
        Ty::Future(a) => Ty::Future(Box::new(norm(a))),
        // Calling a generator runs it to the end and buffers what it yields,
        // and a stream reads like an array, so in MIR a stream is an array.
        Ty::Stream(a) => Ty::Array(Box::new(norm(a))),
        // A prompt is its text. `?p` itself becomes a call into the
        // inference prelude before checking; see `src/infer/rewrite.rs`.
        Ty::Prompt => Ty::Str,
        other => other.clone(),
    }
}

/// Purpose: This function gives a syntax node its identity, as the checker does.
///
/// Inputs:
/// - `x`: any node.
///
/// Outputs: its address.
pub(super) fn addr<T>(x: &T) -> usize {
    x as *const T as usize
}

/// Purpose: This function makes the error for a feature not compiled yet.
///
/// Inputs:
/// - `what`: the feature, as a noun phrase.
/// - `span`: where it is.
///
/// Outputs: the error.
pub(super) fn not_yet(what: impl Into<String>, span: Span) -> JadeError {
    JadeError::NotYetStatic { what: what.into(), span }
}

/// Purpose: This function makes the error for a checker answer that is missing.
///
/// Inputs:
/// - `what`: what was missing.
/// - `span`: where.
///
/// Outputs: the error, worded as a compiler bug.
pub(super) fn internal(what: impl std::fmt::Display, span: Span) -> JadeError {
    JadeError::TypeError {
        message: format!("internal compiler error in the static backend: {what}"),
        span,
    }
}

impl<'a> Lowerer<'a> {
    /// Purpose: This function makes the shared state, regrouping the checker's
    /// answers by instance.
    ///
    /// Inputs:
    /// - `top`: the program's statements.
    /// - `typed`: the checker's answer.
    ///
    /// Outputs: the lowerer.
    fn new(top: &'a [ast::Stmt], typed: &'a Typed) -> Self {
        let mut exprs: HashMap<InstKey, HashMap<usize, Ty>> = HashMap::new();
        for ((k, a), t) in &typed.types {
            exprs.entry(k.clone()).or_default().insert(*a, norm(t));
        }
        let mut targets: HashMap<InstKey, HashMap<usize, Target>> = HashMap::new();
        for ((k, a), t) in &typed.targets {
            targets.entry(k.clone()).or_default().insert(*a, t.clone());
        }
        let mut slots: HashMap<InstKey, HashMap<usize, Ty>> = HashMap::new();
        for ((k, a), t) in &typed.slots {
            slots.entry(k.clone()).or_default().insert(*a, norm(t));
        }
        let streams = typed
            .types
            .iter()
            .filter(|(_, t)| matches!(t, Ty::Stream(_)))
            .map(|(k, _)| k.clone())
            .collect();

        let mut names: Vec<&String> = typed.structs.keys().collect();
        names.sort();
        let mut structs = Vec::new();
        let mut struct_ids = HashMap::new();
        for n in names {
            let info = &typed.structs[n];
            struct_ids.insert(n.clone(), structs.len() as u32);
            structs.push(StructDef {
                name: n.clone(),
                fields: info.fields.iter().map(|(f, t)| (f.clone(), norm(t))).collect(),
                ancestors: info.ancestors.clone(),
            });
        }

        let mut gnames: Vec<&String> = typed.globals.keys().collect();
        gnames.sort();
        let globals = gnames
            .into_iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), (GlobalId(i as u32), norm(&typed.globals[n]))))
            .collect();

        Lowerer {
            top,
            native_sigs: HashMap::new(),
            fns: HashMap::new(),
            methods: HashMap::new(),
            defaults: HashMap::new(),
            func_ids: HashMap::new(),
            sigs: HashMap::new(),
            exprs,
            targets,
            slots,
            globals,
            struct_ids,
            structs,
            extra: std::cell::RefCell::new(Vec::new()),
            n_instances: typed.instances.len(),
            pipes: std::cell::RefCell::new(HashMap::new()),
            streams,
            packages: crate::builtins::all_packages()
                .iter()
                .map(|p| p.global_name.to_string())
                .chain(std::iter::once(crate::stdlib::SYS.to_string()))
                .collect(),
        }
    }

    /// Purpose: This function records every function and method definition,
    /// and every class field's default.
    ///
    /// Inputs:
    /// - `stmts`: the program's statements.
    ///
    /// Outputs: none.
    fn collect(&mut self, stmts: &'a [ast::Stmt]) {
        for s in stmts {
            match s {
                ast::Stmt::FnDef { name, params, body, .. }
                | ast::Stmt::AsyncFnDef { name, params, body, .. } => {
                    let is_async = matches!(s, ast::Stmt::AsyncFnDef { .. });
                    self.fns.insert(name, FnDef { params, body, is_async });
                }
                ast::Stmt::ExtendBlock { type_name, methods, .. } => {
                    for m in methods {
                        if let ast::Stmt::FnDef { name, params, body, .. }
                        | ast::Stmt::AsyncFnDef { name, params, body, .. } = m
                        {
                            let is_async = matches!(m, ast::Stmt::AsyncFnDef { .. });
                            self.methods.insert(
                                (type_name.as_str(), name.as_str()),
                                FnDef { params, body, is_async },
                            );
                        }
                    }
                }
                ast::Stmt::StructDef { name, fields, .. } => {
                    for f in fields {
                        if let ast::StructFieldDef::Let { name: field, default }
                        | ast::StructFieldDef::Prompt { name: field, default } = f
                        {
                            self.defaults.insert((name.as_str(), field.as_str()), default);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Purpose: This function returns the adapter that lets a named function
    /// stand behind a pointer of a joined function type, making it the first
    /// time.
    ///
    /// The checker checked the function at the joined type's parameter types,
    /// with its remaining parameters' defaults after them. The adapter takes
    /// exactly the joined type's parameters, fills the defaults, calls that
    /// instance, and converts its result to the joined type's.
    ///
    /// Inputs:
    /// - `name`: the named function.
    /// - `ft`: the joined function type.
    /// - `span`: where, for errors.
    ///
    /// Outputs: the adapter's id.
    pub(super) fn adapter(
        &self,
        name: &str,
        ft: &crate::typechecker::infer::FnTy,
        span: Span,
    ) -> Result<FuncId> {
        let made =
            self.extra.borrow().iter().position(|x| {
                x.adapter.as_ref().is_some_and(|(n, t)| n.as_str() == name && t == ft)
            });
        if let Some(i) = made {
            return Ok(FuncId((self.n_instances + i) as u32));
        }
        let def = self
            .fns
            .get(name)
            .copied()
            .ok_or_else(|| internal("an unknown function value", span))?;
        let mut args: Vec<Ty> = ft.params.iter().map(norm).collect();
        let mut defaults = Vec::new();
        for (_, d) in &def.params[args.len().min(def.params.len())..] {
            let c = match d.as_ref().map(|d| &d.kind) {
                Some(ExprKind::Integer(i)) => (Const::Int(*i), Ty::Int),
                Some(ExprKind::Float(x)) => (Const::Float(*x), Ty::Float),
                Some(ExprKind::Bool(b)) => (Const::Bool(*b), Ty::Bool),
                Some(ExprKind::Str(s)) => (Const::Str(s.clone()), Ty::Str),
                _ => (Const::Nil(Ty::Nil), Ty::Nil),
            };
            args.push(c.1.clone());
            defaults.push(c.0);
        }
        let key = InstKey { func: FnKey::Named(name.to_string()), args };
        let (id, params, ret) = self.sig(&key, span)?;
        let want = norm(&ft.ret);
        let mut locals: Vec<LocalDecl> =
            ft.params.iter().map(|t| LocalDecl { ty: norm(t), name: None }).collect();
        let r = Local(locals.len() as u32);
        locals.push(LocalDecl { ty: ret.clone(), name: None });
        let e = Local(locals.len() as u32);
        locals.push(LocalDecl { ty: Ty::Bool, name: None });
        let mut argv: Vec<Operand> =
            (0..ft.params.len()).map(|i| Operand::Copy(Place::Local(Local(i as u32)))).collect();
        argv.extend(defaults.into_iter().map(Operand::Const));
        let _ = params;
        let mut done = vec![
            Stmt::Assign(Place::Local(r), Rvalue::Call(id, argv)),
            Stmt::Assign(Place::Local(e), Rvalue::ErrPending),
        ];
        let result = if ret == want {
            Operand::Move(r)
        } else {
            match (&ret, &want) {
                (_, Ty::Opt(_)) if ret != Ty::Nil => {
                    let w = Local(locals.len() as u32);
                    locals.push(LocalDecl { ty: want.clone(), name: None });
                    done.push(Stmt::Assign(
                        Place::Local(w),
                        Rvalue::Wrap(want.clone(), Operand::Move(r)),
                    ));
                    Operand::Move(w)
                }
                (Ty::Nil, Ty::Opt(_)) => Operand::Const(Const::Nil(want.clone())),
                (Ty::Struct(_), Ty::Struct(_)) => Operand::Move(r),
                _ => {
                    return Err(not_yet(
                        format!("a function returning {ret} stored as one returning {want}"),
                        span,
                    ));
                }
            }
        };
        let blocks = vec![
            Block {
                spans: vec![Span { line: 0, col: 0 }; done.len()],
                stmts: done,
                term: Term::If(Operand::Copy(Place::Local(e)), BlockId(2), BlockId(1)),
            },
            Block::new(Term::Return(result)),
            Block::new(Term::Unwind),
        ];
        let func = Func {
            name: format!("adapter {name} as {ft:?}"),
            params: ft.params.len() as u32,
            locals,
            ret: want,
            blocks,
            library: false,
        };
        let mut all = self.extra.borrow_mut();
        all.push(Extra { adapter: Some((name.to_string(), ft.clone())), func });
        Ok(FuncId((self.n_instances + all.len() - 1) as u32))
    }

    /// Purpose: This function reserves an id for a function made while
    /// lowering, before its body is lowered, so the body can call it.
    ///
    /// Inputs:
    /// - `name`: the function's name.
    ///
    /// Outputs: its id. `fill` gives it its body.
    pub(super) fn reserve(&self, name: String) -> FuncId {
        let mut all = self.extra.borrow_mut();
        all.push(Extra {
            adapter: None,
            func: Func {
                name,
                params: 0,
                locals: Vec::new(),
                ret: Ty::Nil,
                blocks: vec![Block::new(Term::Unreachable)],
                library: false,
            },
        });
        FuncId((self.n_instances + all.len() - 1) as u32)
    }

    /// Purpose: This function gives a reserved function its body.
    ///
    /// Inputs:
    /// - `id`: what `reserve` gave.
    /// - `func`: the function.
    ///
    /// Outputs: none.
    pub(super) fn fill(&self, id: FuncId, func: Func) {
        let i = id.0 as usize - self.n_instances;
        self.extra.borrow_mut()[i].func = func;
    }

    /// Purpose: This function lists the globals in the order the program first
    /// sets them.
    ///
    /// Inputs: none.
    ///
    /// Outputs: their ids, first set first.
    pub(super) fn global_order(&self) -> Vec<GlobalId> {
        let mut out = Vec::new();
        for s in self.top {
            let name = match s {
                ast::Stmt::Let { name, .. }
                | ast::Stmt::Assign { name, .. }
                | ast::Stmt::PromptDecl { name, .. } => name,
                _ => continue,
            };
            if let Some((g, _)) = self.globals.get(name)
                && !out.contains(g)
            {
                out.push(*g);
            }
        }
        out
    }

    /// Purpose: This function finds the definition an instance checks.
    ///
    /// Inputs:
    /// - `f`: the code.
    ///
    /// Outputs: the definition, or `None` for the top level and closures.
    pub(super) fn def_of(&self, f: &FnKey) -> Option<FnDef<'a>> {
        match f {
            FnKey::Named(n) => self.fns.get(n.as_str()).copied(),
            FnKey::Method(t, n) => self.methods.get(&(t.as_str(), n.as_str())).copied(),
            _ => None,
        }
    }

    /// Purpose: This function returns a function's id and signature.
    ///
    /// Inputs:
    /// - `k`: the instance.
    /// - `span`: where it is called, for errors.
    ///
    /// Outputs: the id, parameter types and return type.
    pub(super) fn sig(&self, k: &InstKey, span: Span) -> Result<(FuncId, Vec<Ty>, Ty)> {
        let id = *self
            .func_ids
            .get(k)
            .ok_or_else(|| internal("a call to an unchecked instance", span))?;
        let (p, r) = self.sigs[k].clone();
        Ok((id, p, r))
    }

    /// Purpose: This function returns a struct's layout.
    ///
    /// Inputs:
    /// - `name`: the struct.
    ///
    /// Outputs: its definition, if it has one.
    pub(super) fn strukt(&self, name: &str) -> Option<&StructDef> {
        self.struct_ids.get(name).map(|i| &self.structs[*i as usize])
    }

    /// Purpose: This function finds a field's index in a struct's layout.
    ///
    /// Inputs:
    /// - `s`: the struct.
    /// - `field`: the field.
    ///
    /// Outputs: the index and the field's type.
    pub(super) fn field(&self, s: &str, field: &str) -> Option<(u32, Ty)> {
        let def = self.strukt(s)?;
        def.fields.iter().position(|(n, _)| n == field).map(|i| (i as u32, def.fields[i].1.clone()))
    }

    /// Purpose: This function finds the nearest struct two structs share.
    ///
    /// Inputs:
    /// - `a`, `b`: two struct names.
    ///
    /// Outputs: the nearest common ancestor, if any.
    fn common_ancestor(&self, a: &str, b: &str) -> Option<String> {
        let line = |n: &str| -> Vec<String> {
            let mut v = vec![n.to_string()];
            if let Some(s) = self.strukt(n) {
                v.extend(s.ancestors.iter().cloned());
            }
            v
        };
        let lb = line(b);
        line(a).into_iter().find(|x| lb.contains(x))
    }

    /// Purpose: This function lowers one instance into a function.
    ///
    /// Inputs:
    /// - `k`: the instance.
    ///
    /// Outputs: the function.
    fn lower_instance(&self, k: &InstKey) -> Result<Func> {
        self.lower_instance_with(k, None)
    }

    /// Purpose: This function makes the state for lowering one function body
    /// with an instance's answers.
    ///
    /// Inputs:
    /// - `k`: the instance whose types, targets and slots the body is read
    ///   with.
    /// - `ret`: the function's return type.
    ///
    /// Outputs: the state, with one empty block and one empty scope.
    pub(super) fn new_fx<'l>(&'l self, k: &InstKey, ret: Ty) -> Fx<'a, 'l> {
        static EMPTY: std::sync::OnceLock<HashMap<usize, Ty>> = std::sync::OnceLock::new();
        static EMPTY_T: std::sync::OnceLock<HashMap<usize, Target>> = std::sync::OnceLock::new();
        let empty = EMPTY.get_or_init(HashMap::new);
        let empty_t = EMPTY_T.get_or_init(HashMap::new);
        Fx {
            l: self,
            key: k.clone(),
            types: self.exprs.get(k).unwrap_or(empty),
            targets: self.targets.get(k).unwrap_or(empty_t),
            slots: self.slots.get(k).unwrap_or(empty),
            is_main: k.func == FnKey::Main,
            ret: if k.func == FnKey::Main { Ty::Nil } else { ret },
            locals: Vec::new(),
            blocks: vec![Block::new(Term::Unreachable)],
            done: vec![false],
            cur: BlockId(0),
            scopes: vec![HashMap::new()],
            loops: Vec::new(),
            temps: Vec::new(),
            fresh: HashSet::new(),
            handlers: Vec::new(),
            unwind: None,
            stream: None,
            sink: None,
            span: Span { line: 0, col: 0 },
        }
    }

    /// Purpose: This function lowers one instance's body, or a generator's
    /// copy made for a pipe.
    ///
    /// Inputs:
    /// - `k`: the instance.
    /// - `sink`: for a pipe's copy of a generator, the stages each `yield`
    ///   calls; `None` for the instance itself.
    ///
    /// Outputs: the function.
    pub(super) fn lower_instance_with(&self, k: &InstKey, sink: Option<Sink>) -> Result<Func> {
        let (params, ret) = self.sigs[k].clone();
        let mut fx = self.new_fx(k, ret);
        let (name, body) = match &k.func {
            FnKey::Main => ("main".to_string(), self.top),
            other => {
                let def = self.def_of(other).ok_or_else(|| {
                    internal(
                        format!("no definition for {}", inst_label(k)),
                        Span { line: 0, col: 0 },
                    )
                })?;
                // An async function's body returns its value; a call to it
                // starts a task that holds the value in a future.
                if def.is_async
                    && let Ty::Future(inner) = fx.ret.clone()
                {
                    fx.ret = *inner;
                }
                for ((p, _), t) in def.params.iter().zip(&params) {
                    let l = fx.new_local(t.clone(), Some(p.clone()));
                    fx.scopes[0].insert(p.clone(), l);
                }

                (inst_label(k), def.body)
            }
        };
        // A pipe's copy takes the values its stages capture after the
        // generator's own parameters. They have no names, so they shadow
        // nothing in the generator's body.
        let captured: Vec<Local> = match &sink {
            Some(sk) => sk.captured.iter().map(|t| fx.new_local(t.clone(), None)).collect(),
            None => Vec::new(),
        };
        let nparams = fx.locals.len() as u32;
        let name = match &sink {
            Some(sk) => sk.name.clone(),
            None => name,
        };
        if k.func != FnKey::Main && super::ops::body_yields(body) {
            let Ty::Array(el) = fx.ret.clone() else {
                return Err(internal(
                    "a generator that does not return a stream",
                    Span { line: 0, col: 0 },
                ));
            };
            // The instance collects what it yields; a pipe's copy collects
            // what its stages give back for each value it yields.
            let el = match &sink {
                Some(sk) => {
                    fx.ret = Ty::Array(Box::new(sk.out.clone()));
                    fx.sink = Some(FxSink { stages: sk.stages, piece: sk.piece.clone(), captured });
                    sk.out.clone()
                }
                None => *el,
            };
            let s = fx.new_local(fx.ret.clone(), None);
            fx.emit(Stmt::Assign(Place::Local(s), Rvalue::Array(el.clone(), Vec::new())));
            fx.stream = Some((s, el));
        } else if sink.is_some() {
            return Err(internal(
                "a pipe copy of a function that does not yield",
                Span { line: 0, col: 0 },
            ));
        }
        fx.lower_body(body)?;
        let library = sink.is_none() && crate::typechecker::infer::is_library_fn(&k.func);
        Ok(Func {
            name,
            params: nparams,
            locals: fx.locals,
            ret: fx.ret,
            blocks: fx.blocks,
            library,
        })
    }
}

/// Purpose: This function gives a statement's source position.
///
/// Inputs:
/// - `s`: the statement.
///
/// Outputs: its span, or `None` for one that carries none.
fn stmt_span(s: &ast::Stmt) -> Option<Span> {
    Some(match s {
        ast::Stmt::Let { span, .. }
        | ast::Stmt::Assign { span, .. }
        | ast::Stmt::FnDef { span, .. }
        | ast::Stmt::Return { span, .. }
        | ast::Stmt::Yield { span, .. }
        | ast::Stmt::If { span, .. }
        | ast::Stmt::While { span, .. }
        | ast::Stmt::For { span, .. }
        | ast::Stmt::Break { span, .. }
        | ast::Stmt::Continue { span, .. }
        | ast::Stmt::StructDef { span, .. }
        | ast::Stmt::ExtendBlock { span, .. }
        | ast::Stmt::FieldAssign { span, .. }
        | ast::Stmt::IndexAssign { span, .. }
        | ast::Stmt::PromptDecl { span, .. }
        | ast::Stmt::Use { span, .. }
        | ast::Stmt::FromUse { span, .. }
        | ast::Stmt::Raise { span, .. }
        | ast::Stmt::TryCatch { span, .. }
        | ast::Stmt::AsyncFnDef { span, .. } => *span,
        ast::Stmt::Expr(e) => e.span,
    })
}

/// Purpose: This struct holds the state of lowering one function.
pub(super) struct Fx<'a, 'l> {
    pub(super) l: &'l Lowerer<'a>,
    pub(super) types: &'l HashMap<usize, Ty>,
    pub(super) targets: &'l HashMap<usize, Target>,
    /// The instance being lowered.
    pub(super) key: InstKey,
    pub(super) slots: &'l HashMap<usize, Ty>,
    pub(super) is_main: bool,
    pub(super) ret: Ty,
    pub(super) locals: Vec<LocalDecl>,
    pub(super) blocks: Vec<Block>,
    /// Whether each block has its terminator.
    pub(super) done: Vec<bool>,
    pub(super) cur: BlockId,
    /// Variables in scope, innermost last.
    pub(super) scopes: Vec<HashMap<String, Local>>,
    /// Each enclosing loop's continue and break targets, and how many scopes
    /// were open outside it, innermost last.
    pub(super) loops: Vec<(BlockId, BlockId, usize)>,
    /// Heap temporaries of the statement being lowered, dropped when it ends.
    pub(super) temps: Vec<Local>,
    /// Temporaries whose reference has not been handed over yet.
    pub(super) fresh: HashSet<Local>,
    /// Each enclosing `try`'s handler block, innermost last.
    pub(super) handlers: Vec<BlockId>,
    /// The block that leaves the function with an error raised, once made.
    pub(super) unwind: Option<BlockId>,
    /// For a generator, the array each `yield` appends to, and its element type.
    pub(super) stream: Option<(Local, Ty)>,
    /// For a generator's copy made for a pipe, the stages each `yield` calls
    /// before what they give back is appended to `stream`.
    pub(super) sink: Option<FxSink>,
    /// The source position of what is being lowered, recorded with each
    /// statement emitted.
    pub(super) span: Span,
}

impl<'a, 'l> Fx<'a, 'l> {
    // ── building blocks ──────────────────────────────────────────────────

    /// Purpose: This function makes a new local.
    ///
    /// Inputs:
    /// - `ty`: its type.
    /// - `name`: the variable's name, or `None` for a temporary.
    ///
    /// Outputs: the local.
    pub(super) fn new_local(&mut self, ty: Ty, name: Option<String>) -> Local {
        let l = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl { ty, name });
        l
    }

    /// Purpose: This function makes a temporary for the current statement.
    ///
    /// Inputs:
    /// - `ty`: its type.
    ///
    /// Outputs: the local. A heap temporary is dropped when the statement ends.
    pub(super) fn temp(&mut self, ty: Ty) -> Local {
        let l = self.new_local(ty.clone(), None);
        if is_heap(&ty) {
            self.temps.push(l);
        }
        l
    }

    /// Purpose: This function computes an rvalue into a new temporary.
    ///
    /// Inputs:
    /// - `ty`: the value's type.
    /// - `rv`: the computation.
    ///
    /// Outputs: an operand naming the temporary.
    pub(super) fn assign_temp(&mut self, ty: Ty, rv: Rvalue) -> Operand {
        let t = self.temp(ty);
        self.emit(Stmt::Assign(Place::Local(t), rv));
        self.fresh.insert(t);
        Operand::Copy(Place::Local(t))
    }

    /// Purpose: This function turns an operand into one that hands over its
    /// reference when it is a temporary nothing else uses.
    ///
    /// Inputs:
    /// - `op`: the operand.
    ///
    /// Outputs: `Move` for a fresh temporary, the operand unchanged otherwise.
    pub(super) fn consume(&mut self, op: Operand) -> Operand {
        if let Operand::Copy(Place::Local(t)) = &op
            && self.fresh.remove(t)
        {
            return Operand::Move(*t);
        }
        op
    }

    /// Purpose: This function appends a statement to the current block.
    ///
    /// Inputs:
    /// - `s`: the statement.
    ///
    /// Outputs: none. Code after a terminator goes into a fresh block nothing
    /// reaches.
    pub(super) fn emit(&mut self, s: Stmt) {
        if self.done[self.cur.0 as usize] {
            let b = self.new_block();
            self.cur = b;
        }
        let raises = match &s {
            Stmt::Assign(_, rv) | Stmt::Eval(rv) => rv.may_raise(),
            _ => false,
        };
        self.blocks[self.cur.0 as usize].stmts.push(s);
        self.blocks[self.cur.0 as usize].spans.push(self.span);
        if raises {
            self.check_err();
        }
    }

    /// Purpose: This function sends control to the error handler when the
    /// statement just emitted raised.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none. Lowering continues in the block where nothing was raised.
    fn check_err(&mut self) {
        let e = self.new_local(Ty::Bool, None);
        self.blocks[self.cur.0 as usize]
            .stmts
            .push(Stmt::Assign(Place::Local(e), Rvalue::ErrPending));
        self.blocks[self.cur.0 as usize].spans.push(self.span);
        let handler = self.handler();
        let cont = self.new_block();
        self.terminate(Term::If(Operand::Copy(Place::Local(e)), handler, cont));
        self.switch_to(cont);
    }

    /// Purpose: This function returns where a raised error goes from here: the
    /// innermost `try`'s handler, or out of the function.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the block.
    fn handler(&mut self) -> BlockId {
        if let Some(h) = self.handlers.last() {
            return *h;
        }
        if let Some(u) = self.unwind {
            return u;
        }
        let u = self.new_block();
        self.blocks[u.0 as usize].term = Term::Unwind;
        self.done[u.0 as usize] = true;
        self.unwind = Some(u);
        u
    }

    /// Purpose: This function makes a new empty block.
    ///
    /// Inputs: none.
    ///
    /// Outputs: its id.
    pub(super) fn new_block(&mut self) -> BlockId {
        self.blocks.push(Block::new(Term::Unreachable));
        self.done.push(false);
        BlockId(self.blocks.len() as u32 - 1)
    }

    /// Purpose: This function ends the current block.
    ///
    /// Inputs:
    /// - `t`: its terminator.
    ///
    /// Outputs: none.
    pub(super) fn terminate(&mut self, t: Term) {
        let i = self.cur.0 as usize;
        if !self.done[i] {
            self.blocks[i].term = t;
            self.done[i] = true;
        }
    }

    /// Purpose: This function continues lowering in another block.
    ///
    /// Inputs:
    /// - `b`: the block.
    ///
    /// Outputs: none.
    pub(super) fn switch_to(&mut self, b: BlockId) {
        self.cur = b;
    }

    /// Purpose: This function frees the variables of every scope from a depth
    /// in, for a `break` or `continue` that leaves them, innermost first.
    ///
    /// Inputs:
    /// - `depth`: how many scopes stay open.
    ///
    /// Outputs: none.
    fn drop_scopes_from(&mut self, depth: usize) {
        let mut drops = Vec::new();
        for scope in self.scopes[depth.min(self.scopes.len())..].iter().rev() {
            let mut ls: Vec<Local> = scope.values().copied().collect();
            ls.sort_by_key(|l| std::cmp::Reverse(l.0));
            drops.extend(ls.into_iter().filter(|l| is_heap(&self.locals[l.0 as usize].ty)));
        }
        for l in drops {
            self.emit(Stmt::Drop(Place::Local(l)));
        }
    }

    /// Purpose: This function drops the temporaries made since a mark.
    ///
    /// Inputs:
    /// - `mark`: the length of the temporary list when the statement began.
    ///
    /// Outputs: none.
    pub(super) fn drop_temps(&mut self, mark: usize) {
        let ts: Vec<Local> = self.temps.drain(mark..).collect();
        for t in ts {
            self.fresh.remove(&t);
            self.emit(Stmt::Drop(Place::Local(t)));
        }
    }

    /// Purpose: This function returns an expression's checked type.
    ///
    /// Inputs:
    /// - `e`: the expression.
    ///
    /// Outputs: its type.
    pub(super) fn ty(&self, e: &Expr) -> Result<Ty> {
        self.types
            .get(&addr(e))
            .cloned()
            .ok_or_else(|| internal("an expression with no type", e.span))
    }

    /// Purpose: This function answers a condition made of built-in type
    /// tests in library code, from this instance's variable types.
    ///
    /// Inputs:
    /// - `condition`: the condition.
    ///
    /// Outputs: the answer, or `NotOne` outside library code, where `x is
    /// int` names a class.
    pub(super) fn static_answer(&self, condition: &Expr) -> StaticTest {
        if !crate::typechecker::infer::is_library_fn(&self.key.func) {
            return StaticTest::NotOne;
        }
        // The type the checker recorded for each tested variable where it is
        // tested, which is its narrowed type inside a branch that narrowed it.
        crate::typechecker::infer::static_test(condition, &mut |e| self.ty(e).ok())
    }

    /// Purpose: This function finds a variable: a local in scope, then a global.
    ///
    /// Inputs:
    /// - `name`: the variable.
    ///
    /// Outputs: its place and type, or `None`.
    pub(super) fn lookup(&self, name: &str) -> Option<(Place, Ty)> {
        for s in self.scopes.iter().rev() {
            if let Some(l) = s.get(name) {
                return Some((Place::Local(*l), self.locals[l.0 as usize].ty.clone()));
            }
        }
        self.l.globals.get(name).map(|(g, t)| (Place::Global(*g), t.clone()))
    }

    /// Purpose: This function declares a variable in the innermost scope.
    ///
    /// Inputs:
    /// - `name`: the variable.
    /// - `node`: the address of the statement that made its slot.
    /// - `span`: where, for errors.
    ///
    /// Outputs: its place and type.
    pub(super) fn declare(&mut self, name: &str, node: usize, span: Span) -> Result<(Place, Ty)> {
        let ty = self
            .slots
            .get(&node)
            .cloned()
            .ok_or_else(|| internal("a variable with no type", span))?;
        let l = self.new_local(ty.clone(), Some(name.to_string()));
        self.scopes.last_mut().expect("scope").insert(name.to_string(), l);
        Ok((Place::Local(l), ty))
    }

    // ── conversions ──────────────────────────────────────────────────────

    /// Purpose: This function converts a value to a type it fits in.
    ///
    /// Inputs:
    /// - `op`: the value.
    /// - `from`: its type.
    /// - `to`: the type wanted.
    /// - `span`: where, for errors.
    ///
    /// Outputs: an operand of type `to`.
    pub(super) fn coerce(
        &mut self,
        op: Operand,
        from: &Ty,
        to: &Ty,
        span: Span,
    ) -> Result<Operand> {
        if from == to {
            return Ok(op);
        }
        // A number literal takes the sized type it meets; the checker made
        // sure it fits.
        if let Ty::Sized(s) = to {
            match (&op, s.is_float()) {
                (Operand::Const(Const::Int(i)), false) => {
                    return Ok(Operand::Const(Const::Sized(*s, *i)));
                }
                (Operand::Const(Const::Int(i)), true) => {
                    return Ok(Operand::Const(Const::Float32(*i as f32)));
                }
                (Operand::Const(Const::Float(x)), true) => {
                    return Ok(Operand::Const(Const::Float32(*x as f32)));
                }
                _ => {}
            }
        }
        match (from, to) {
            // A missing error is a null pointer too.
            (Ty::Nil, Ty::Opt(_) | Ty::Error) => Ok(Operand::Const(Const::Nil(to.clone()))),
            (Ty::Opt(a), Ty::Opt(b)) => match (&**a, &**b) {
                // A class value is the same object read as any of its classes.
                (Ty::Struct(_), Ty::Struct(_)) => Ok(op),
                _ => Err(internal(format!("cannot convert {from} to {to}"), span)),
            },
            (_, Ty::Opt(inner)) => {
                let v = self.coerce(op, from, inner, span)?;
                Ok(self.assign_temp(to.clone(), Rvalue::Wrap(to.clone(), v)))
            }
            (Ty::Opt(inner), _) => {
                let v = self.assign_temp((**inner).clone(), Rvalue::Unwrap(op));
                self.coerce(v, inner, to, span)
            }
            (Ty::Struct(_), Ty::Struct(_)) => Ok(op),
            // A named function joining others becomes a pointer to an adapter
            // with the joined signature.
            (Ty::Func(crate::typechecker::infer::FuncRef::Named(n)), Ty::Fn(ft)) => {
                let id = self.l.adapter(n, ft, span)?;
                Ok(self.assign_temp(to.clone(), Rvalue::FnAddr(id)))
            }
            (Ty::Func(_), Ty::Fn(_)) => Err(not_yet("a builtin stored with other functions", span)),
            _ => Err(internal(format!("cannot convert {from} to {to}"), span)),
        }
    }

    /// Purpose: This function finds the one type two compared values both fit in.
    ///
    /// Inputs:
    /// - `a`, `b`: the two types.
    ///
    /// Outputs: the wider type.
    pub(super) fn common(&self, a: &Ty, b: &Ty) -> Ty {
        match (a, b) {
            _ if a == b => a.clone(),
            (Ty::Nil, t) | (t, Ty::Nil) => Ty::opt(t.clone()),
            (Ty::Opt(x), y) | (y, Ty::Opt(x)) => Ty::opt(self.common(x, y)),
            (Ty::Struct(x), Ty::Struct(y)) => {
                Ty::Struct(self.l.common_ancestor(x, y).unwrap_or_else(|| x.clone()))
            }
            _ => a.clone(),
        }
    }

    // ── bodies and statements ────────────────────────────────────────────

    /// Purpose: This function lowers a function's whole body, including the
    /// return at its end.
    ///
    /// Inputs:
    /// - `body`: the statements.
    ///
    /// Outputs: `Ok(())` or the first error.
    fn lower_body(&mut self, body: &'a [ast::Stmt]) -> Result<()> {
        // A function's trailing bare expression is its return value.
        if !self.is_main
            && self.stream.is_none()
            && let Some(ast::Stmt::Expr(e)) = body.last()
        {
            for s in &body[..body.len() - 1] {
                self.stmt(s)?;
            }
            let mark = self.temps.len();
            let (v, t) = self.expr(e)?;
            let ret = self.ret.clone();
            let v = self.coerce(v, &t, &ret, e.span)?;
            let v = self.consume(v);
            self.terminate(Term::Return(v));
            self.temps.truncate(mark);
            return Ok(());
        }
        for s in body {
            self.stmt(s)?;
        }
        // A program that ends normally releases its globals, newest first, so
        // every destructor of a value nothing else holds runs.
        if self.is_main {
            for g in self.l.global_order().into_iter().rev() {
                let ty = self.l.globals.values().find(|(id, _)| *id == g).map(|(_, t)| t.clone());
                if ty.as_ref().is_some_and(is_heap) {
                    self.emit(Stmt::Drop(Place::Global(g)));
                }
            }
        }
        // A generator returns what it yielded.
        if let Some((s, _)) = &self.stream {
            let s = *s;
            self.terminate(Term::Return(Operand::Copy(Place::Local(s))));
            return Ok(());
        }
        // Falling off the end returns nil, where the return type can hold it.
        match &self.ret {
            Ty::Nil | Ty::Opt(_) => {
                let r = self.ret.clone();
                self.terminate(Term::Return(Operand::Const(Const::Nil(r))));
            }
            _ => self.terminate(Term::Unreachable),
        }
        Ok(())
    }

    /// Purpose: This function lowers a block of statements in a new scope.
    ///
    /// Inputs:
    /// - `body`: the statements.
    ///
    /// Outputs: `Ok(())` or the first error.
    fn block(&mut self, body: &'a [ast::Stmt]) -> Result<()> {
        self.scopes.push(HashMap::new());
        for s in body {
            self.stmt(s)?;
        }
        let scope = self.scopes.pop().expect("scope");
        // Free what the block's variables hold once it ends normally.
        let mut ls: Vec<Local> = scope.into_values().collect();
        ls.sort_by_key(|l| l.0);
        for l in ls {
            if is_heap(&self.locals[l.0 as usize].ty) {
                self.emit(Stmt::Drop(Place::Local(l)));
            }
        }
        Ok(())
    }

    /// Purpose: This function lowers one statement and drops its temporaries.
    ///
    /// Inputs:
    /// - `s`: the statement.
    ///
    /// Outputs: `Ok(())` or the first error.
    fn stmt(&mut self, s: &'a ast::Stmt) -> Result<()> {
        let outer = self.span;
        if let Some(sp) = stmt_span(s) {
            self.span = sp;
        }
        let mark = self.temps.len();
        let r = self.stmt_inner(s);
        if r.is_ok() {
            self.drop_temps(mark);
        }
        self.span = outer;
        r
    }

    /// Purpose: This function stores a value into a place, converting it to the
    /// place's type.
    ///
    /// Inputs:
    /// - `place`, `pty`: the place and its type.
    /// - `v`, `vty`: the value and its type.
    /// - `span`: where.
    ///
    /// Outputs: `Ok(())` or an error.
    pub(super) fn store(
        &mut self,
        place: Place,
        pty: &Ty,
        v: Operand,
        vty: &Ty,
        span: Span,
    ) -> Result<()> {
        let v = self.coerce(v, vty, pty, span)?;
        let v = self.consume(v);
        self.emit(Stmt::Assign(place, Rvalue::Use(v)));
        Ok(())
    }

    /// Purpose: This function lowers one statement.
    ///
    /// Inputs:
    /// - `s`: the statement.
    ///
    /// Outputs: `Ok(())` or the first error.
    fn stmt_inner(&mut self, s: &'a ast::Stmt) -> Result<()> {
        match s {
            ast::Stmt::Let { name, value, span }
            | ast::Stmt::PromptDecl { name, body: value, span } => {
                let (v, vt) = self.expr(value)?;
                let (place, pt) = if self.is_main && self.scopes.len() == 1 {
                    let (g, t) = self
                        .l
                        .globals
                        .get(name)
                        .cloned()
                        .ok_or_else(|| internal("an unknown global", *span))?;
                    (Place::Global(g), t)
                } else {
                    self.declare(name, addr(s), *span)?
                };
                self.store(place, &pt, v, &vt, *span)
            }
            ast::Stmt::Assign { name, value, span } => {
                let (v, vt) = self.expr(value)?;
                let (place, pt) = match self.lookup(name) {
                    Some(p) => p,
                    None => self.declare(name, addr(s), *span)?,
                };
                self.store(place, &pt, v, &vt, *span)
            }
            ast::Stmt::FnDef { decorators, span, .. }
            | ast::Stmt::AsyncFnDef { decorators, span, .. } => {
                if !decorators.is_empty() {
                    return Err(not_yet("decorators", *span));
                }
                Ok(())
            }
            ast::Stmt::StructDef { .. }
            | ast::Stmt::ExtendBlock { .. }
            | ast::Stmt::Use { .. }
            | ast::Stmt::FromUse { .. } => Ok(()),
            ast::Stmt::Return { span, .. } if self.stream.is_some() => {
                let (s, _) = self.stream.clone().ok_or_else(|| internal("no stream", *span))?;
                self.terminate(Term::Return(Operand::Copy(Place::Local(s))));
                Ok(())
            }
            ast::Stmt::Return { value, span } => {
                let ret = self.ret.clone();
                let v = match value {
                    Some(e) => {
                        let (v, t) = self.expr(e)?;
                        self.coerce(v, &t, &ret, *span)?
                    }
                    None => {
                        self.coerce(Operand::Const(Const::Nil(Ty::Nil)), &Ty::Nil, &ret, *span)?
                    }
                };
                let v = self.consume(v);
                self.terminate(Term::Return(v));
                Ok(())
            }
            ast::Stmt::If { condition, then_body, else_body, .. } => {
                // Library code's `if x is int`, which the checker answered for
                // this instance: only the branch it checked is lowered.
                if let StaticTest::Known(taken) = self.static_answer(condition) {
                    return match (taken, else_body) {
                        (true, _) => self.block(then_body),
                        (false, Some(e)) => self.block(e),
                        (false, None) => Ok(()),
                    };
                }
                let mark = self.temps.len();
                let (c, _) = self.expr(condition)?;
                self.drop_temps(mark);
                let bt = self.new_block();
                let be = self.new_block();
                let end = if else_body.is_some() { self.new_block() } else { be };
                self.terminate(Term::If(c, bt, be));
                self.switch_to(bt);
                self.block(then_body)?;
                self.terminate(Term::Goto(end));
                if let Some(e) = else_body {
                    self.switch_to(be);
                    self.block(e)?;
                    self.terminate(Term::Goto(end));
                }
                self.switch_to(end);
                Ok(())
            }
            ast::Stmt::While { condition, body, .. } => {
                let head = self.new_block();
                let bb = self.new_block();
                let exit = self.new_block();
                self.terminate(Term::Goto(head));
                self.switch_to(head);
                let mark = self.temps.len();
                let (c, _) = self.expr(condition)?;
                self.drop_temps(mark);
                self.terminate(Term::If(c, bb, exit));
                self.switch_to(bb);
                self.loops.push((head, exit, self.scopes.len()));
                self.block(body)?;
                self.loops.pop();
                self.terminate(Term::Goto(head));
                self.switch_to(exit);
                Ok(())
            }
            ast::Stmt::For { var, iterable, body, span } => {
                self.for_loop(s, var, iterable, body, *span)
            }
            ast::Stmt::Break { span } => {
                let (_, brk, depth) =
                    *self.loops.last().ok_or_else(|| internal("break outside a loop", *span))?;
                self.drop_scopes_from(depth);
                self.terminate(Term::Goto(brk));
                Ok(())
            }
            ast::Stmt::Continue { span } => {
                let (cont, _, depth) =
                    *self.loops.last().ok_or_else(|| internal("continue outside a loop", *span))?;
                self.drop_scopes_from(depth);
                self.terminate(Term::Goto(cont));
                Ok(())
            }
            ast::Stmt::FieldAssign { object, field, value, span } => {
                let (v, vt) = self.expr(value)?;
                let (place, ot) =
                    self.lookup(object).ok_or_else(|| internal("an unknown variable", *span))?;
                let Ty::Struct(sn) = &ot else {
                    return Err(internal(format!("a field set on {ot}"), *span));
                };
                let (index, ft) =
                    self.l.field(sn, field).ok_or_else(|| internal("an unknown field", *span))?;
                let v = self.coerce(v, &vt, &ft, *span)?;
                let v = self.consume(v);
                self.emit(Stmt::SetField {
                    object: Operand::Copy(place),
                    strukt: sn.clone(),
                    index,
                    value: v,
                });
                Ok(())
            }
            ast::Stmt::IndexAssign { name, index, value, span } => {
                let (i, it) = self.expr(index)?;
                let (v, vt) = self.expr(value)?;
                let (place, tt) =
                    self.lookup(name).ok_or_else(|| internal("an unknown variable", *span))?;
                match &tt {
                    Ty::Array(e) => {
                        let v = self.coerce(v, &vt, e, *span)?;
                        let v = self.consume(v);
                        self.emit(Stmt::Eval(Rvalue::Op(
                            Op::ArraySet((**e).clone()),
                            vec![Operand::Copy(place), i, v],
                        )));
                        Ok(())
                    }
                    Ty::Map(e) => {
                        let k = self.coerce(i, &it, &Ty::Str, *span)?;
                        let k = self.consume(k);
                        let v = self.coerce(v, &vt, e, *span)?;
                        let v = self.consume(v);
                        self.emit(Stmt::DictSet { dict: place, key: k, value: v });
                        Ok(())
                    }
                    Ty::Bytes => {
                        let v = self.coerce(v, &vt, &Ty::Int, *span)?;
                        self.emit(Stmt::Eval(Rvalue::Op(
                            Op::BytesSet,
                            vec![Operand::Copy(place), i, v],
                        )));
                        Ok(())
                    }
                    other => Err(internal(format!("an index assignment into {other}"), *span)),
                }
            }
            ast::Stmt::Yield { value, span } => {
                let (s, el) = self
                    .stream
                    .clone()
                    .ok_or_else(|| internal("yield outside a generator", *span))?;
                if let Some(sink) = self.sink.clone() {
                    return self.yield_to_stages(value, &sink, s, &el, *span);
                }
                let (v, t) = self.expr(value)?;
                let v = self.coerce(v, &t, &el, *span)?;
                let v = self.consume(v);
                self.emit(Stmt::Eval(Rvalue::Op(
                    Op::ArrayPush(el),
                    vec![Operand::Copy(Place::Local(s)), v],
                )));
                Ok(())
            }
            ast::Stmt::Raise { value, span } => {
                let (v, vt) = self.expr(value)?;
                let obj = match &vt {
                    // `raise "x"` raises a RuntimeError carrying the message.
                    Ty::Str => {
                        let v = self.consume(v);
                        self.assign_temp(
                            Ty::Struct("RuntimeError".into()),
                            Rvalue::Struct("RuntimeError".into(), vec![v]),
                        )
                    }
                    Ty::Struct(_) | Ty::Error => v,
                    other => return Err(internal(format!("raising a {other}"), *span)),
                };
                let obj = self.consume(obj);
                self.emit(Stmt::Raise(obj));
                let h = self.handler();
                self.terminate(Term::Goto(h));
                Ok(())
            }
            ast::Stmt::TryCatch { body, arms, .. } => self.try_catch(body, arms),
            ast::Stmt::Expr(e) => {
                self.expr(e)?;
                Ok(())
            }
        }
    }

    /// Purpose: This function lowers `try { … } catch … { … }`.
    ///
    /// The body runs with a handler block of its own, which takes the raised
    /// error out of the thread's slot and tests each arm in order: a typed arm
    /// matches the class it names and every class that descends from it, and
    /// an untyped arm matches anything. An error no arm matches is raised again
    /// to the enclosing handler.
    ///
    /// Inputs:
    /// - `body`: the statements tried.
    /// - `arms`: the catch arms.
    ///
    /// Outputs: `Ok(())` or an error.
    fn try_catch(
        &mut self,
        body: &'a [ast::Stmt],
        arms: &'a [crate::frontend::ast::CatchArm],
    ) -> Result<()> {
        let h = self.new_block();
        let end = self.new_block();
        self.handlers.push(h);
        self.block(body)?;
        self.handlers.pop();
        self.terminate(Term::Goto(end));

        self.switch_to(h);
        let err = self.new_local(Ty::Error, None);
        self.emit(Stmt::Assign(Place::Local(err), Rvalue::TakeErr));
        for arm in arms {
            let (bound, next) = match &arm.catch_type {
                Some(class) => {
                    let ids: Vec<i64> = self
                        .l
                        .structs
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| s.name == *class || s.ancestors.iter().any(|a| a == class))
                        .map(|(i, _)| i as i64)
                        .collect();
                    let hit = self.new_local(Ty::Bool, None);
                    self.emit(Stmt::Assign(
                        Place::Local(hit),
                        Rvalue::IsA(Operand::Copy(Place::Local(err)), ids),
                    ));
                    let yes = self.new_block();
                    let next = self.new_block();
                    self.terminate(Term::If(Operand::Copy(Place::Local(hit)), yes, next));
                    self.switch_to(yes);
                    (Ty::Struct(class.clone()), Some(next))
                }
                None => (Ty::Error, None),
            };
            self.scopes.push(HashMap::new());
            let b = self.new_local(bound, Some(arm.binding.clone()));
            self.scopes.last_mut().expect("scope").insert(arm.binding.clone(), b);
            self.emit(Stmt::Assign(Place::Local(b), Rvalue::Use(Operand::Move(err))));
            for s in &arm.body {
                self.stmt(s)?;
            }
            let scope = self.scopes.pop().expect("scope");
            for l in scope.into_values() {
                self.emit(Stmt::Drop(Place::Local(l)));
            }
            self.terminate(Term::Goto(end));
            match next {
                Some(n) => self.switch_to(n),
                None => {
                    self.switch_to(end);
                    return Ok(());
                }
            }
        }
        // No arm matched: raise it again for the enclosing handler.
        self.emit(Stmt::Raise(Operand::Move(err)));
        let outer = self.handler();
        self.terminate(Term::Goto(outer));
        self.switch_to(end);
        Ok(())
    }

    /// Purpose: This function lowers a `for` loop over an array, a string or a
    /// dict's keys.
    ///
    /// Inputs:
    /// - `s`: the statement, whose address names the loop variable's slot.
    /// - `var`, `iterable`, `body`: its parts.
    /// - `span`: where.
    ///
    /// Outputs: `Ok(())` or an error.
    fn for_loop(
        &mut self,
        s: &'a ast::Stmt,
        var: &str,
        iterable: &'a Expr,
        body: &'a [ast::Stmt],
        span: Span,
    ) -> Result<()> {
        let (it, itt) = self.expr(iterable)?;
        // What is walked is an array, kept in a local of its own so the loop
        // owns it for as long as it runs.
        let (arr, elem) = match &itt {
            Ty::Array(e) => (it, (**e).clone()),
            Ty::Str => {
                let a = self
                    .assign_temp(Ty::Array(Box::new(Ty::Char)), Rvalue::Op(Op::StrChars, vec![it]));
                (a, Ty::Char)
            }
            Ty::Map(v) => {
                let a = self.assign_temp(
                    Ty::Array(Box::new(Ty::Str)),
                    Rvalue::Op(Op::DictKeys((**v).clone()), vec![it]),
                );
                (a, Ty::Str)
            }
            Ty::Bytes => {
                let a = self
                    .assign_temp(Ty::Array(Box::new(Ty::Int)), Rvalue::Op(Op::BytesInts, vec![it]));
                (a, Ty::Int)
            }
            Ty::Stream(_) => return Err(not_yet("streams", span)),
            other => return Err(not_yet(format!("a for loop over {other}"), span)),
        };
        let aty = Ty::Array(Box::new(elem.clone()));
        let held = self.new_local(aty.clone(), None);
        let arr = self.consume(arr);
        self.emit(Stmt::Assign(Place::Local(held), Rvalue::Use(arr)));
        let idx = self.new_local(Ty::Int, None);
        self.emit(Stmt::Assign(Place::Local(idx), Rvalue::Use(Operand::Const(Const::Int(0)))));
        // The length is read once, before the first pass, as v1 did.
        let len = self.new_local(Ty::Int, None);
        self.emit(Stmt::Assign(
            Place::Local(len),
            Rvalue::Op(Op::ArrayLen, vec![Operand::Copy(Place::Local(held))]),
        ));

        let head = self.new_block();
        let bb = self.new_block();
        let step = self.new_block();
        let exit = self.new_block();
        self.terminate(Term::Goto(head));
        self.switch_to(head);
        let more = self.new_local(Ty::Bool, None);
        self.emit(Stmt::Assign(
            Place::Local(more),
            Rvalue::Bin(
                BinOp::Cmp(CmpOp::Lt, Ty::Int),
                Operand::Copy(Place::Local(idx)),
                Operand::Copy(Place::Local(len)),
            ),
        ));
        self.terminate(Term::If(Operand::Copy(Place::Local(more)), bb, exit));

        self.switch_to(bb);
        self.scopes.push(HashMap::new());
        let (vp, vty) = self.declare(var, addr(s) + 1, span)?;
        let mark = self.temps.len();
        let el = self.assign_temp(
            elem.clone(),
            Rvalue::Op(
                Op::ArrayGet(elem.clone()),
                vec![Operand::Copy(Place::Local(held)), Operand::Copy(Place::Local(idx))],
            ),
        );
        self.store(vp, &vty, el, &elem, span)?;
        self.drop_temps(mark);
        self.loops.push((step, exit, self.scopes.len()));
        for st in body {
            self.stmt(st)?;
        }
        self.loops.pop();
        self.scopes.pop();
        self.terminate(Term::Goto(step));

        self.switch_to(step);
        self.emit(Stmt::Assign(
            Place::Local(idx),
            Rvalue::Bin(
                BinOp::Arith(ArithOp::Add, NumTy::Int),
                Operand::Copy(Place::Local(idx)),
                Operand::Const(Const::Int(1)),
            ),
        ));
        self.terminate(Term::Goto(head));

        self.switch_to(exit);
        self.emit(Stmt::Drop(Place::Local(held)));
        if is_heap(&vty) {
            self.emit(Stmt::Drop(vp));
        }
        Ok(())
    }

    // ── expressions ──────────────────────────────────────────────────────

    /// Purpose: This function lowers an expression.
    ///
    /// Inputs:
    /// - `e`: the expression.
    ///
    /// Outputs: an operand holding its value, and its type.
    pub(super) fn expr(&mut self, e: &'a Expr) -> Result<(Operand, Ty)> {
        let outer = self.span;
        self.span = e.span;
        let r = self.expr_inner(e);
        self.span = outer;
        r
    }

    /// Purpose: This function lowers an expression, with its span recorded.
    ///
    /// Inputs:
    /// - `e`: the expression.
    ///
    /// Outputs: an operand holding its value, and its type.
    fn expr_inner(&mut self, e: &'a Expr) -> Result<(Operand, Ty)> {
        let span = e.span;
        let et = self.ty(e)?;
        let op = match &e.kind {
            ExprKind::Integer(i) => Operand::Const(Const::Int(*i)),
            ExprKind::Float(x) => Operand::Const(Const::Float(*x)),
            ExprKind::Bool(b) => Operand::Const(Const::Bool(*b)),
            ExprKind::Str(s) => Operand::Const(Const::Str(s.clone())),
            // Library code's test of a built-in type: known for a value of one
            // type, and for a `T?` whether it holds its value, or for `x is
            // nil` whether it does not.
            ExprKind::Is { value, class }
                if let Some(want) = crate::typechecker::infer::builtin_test_type(class)
                    && crate::typechecker::infer::is_library_fn(&self.key.func) =>
            {
                let (v, t) = self.expr(value)?;
                match crate::typechecker::infer::type_test(&t, &want) {
                    StaticTest::Known(answer) => Operand::Const(Const::Bool(answer)),
                    _ => {
                        let missing = self.assign_temp(Ty::Bool, Rvalue::IsNil(v));
                        if want == Ty::Nil {
                            missing
                        } else {
                            self.assign_temp(Ty::Bool, Rvalue::Un(UnOp::Not, missing))
                        }
                    }
                }
            }
            ExprKind::Is { value, class } => {
                let (v, _) = self.expr(value)?;
                let ids: Vec<i64> = self
                    .l
                    .structs
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.name == *class || s.ancestors.iter().any(|a| a == class))
                    .map(|(i, _)| i as i64)
                    .collect();
                self.assign_temp(Ty::Bool, Rvalue::IsA(v, ids))
            }
            ExprKind::Identifier(name) => {
                if matches!(name.as_str(), "nil" | "None" | "null") {
                    Operand::Const(Const::Nil(et.clone()))
                } else if let Some((place, pt)) = self.lookup(name) {
                    // A narrowed read: the variable is `T?` and known present here.
                    self.coerce(Operand::Copy(place), &pt, &et, span)?
                } else if matches!(et, Ty::Func(_)) {
                    // A named function or builtin as a value: known now, called
                    // directly wherever it goes.
                    Operand::Const(Const::Static(et.clone()))
                } else {
                    return Err(internal(format!("an unknown name '{name}'"), span));
                }
            }
            ExprKind::Call { callee, args, kwargs } => {
                return self.call(e, &et, callee, args, kwargs);
            }
            ExprKind::BinOp { op, left, right } => self.binop(op, left, right, &et, span)?,
            ExprKind::UnaryOp { op, operand } => {
                let (v, _) = self.expr(operand)?;
                // A negated literal is a constant, so it can take the type it
                // meets, as `@short let s = -5` needs.
                match (op, &v) {
                    (UnaryOpKind::Neg, Operand::Const(Const::Int(i))) if *i != i64::MIN => {
                        return Ok((Operand::Const(Const::Int(-*i)), et));
                    }
                    (UnaryOpKind::Neg, Operand::Const(Const::Float(x))) => {
                        return Ok((Operand::Const(Const::Float(-*x)), et));
                    }
                    _ => {}
                }
                let u = match op {
                    UnaryOpKind::Not => UnOp::Not,
                    UnaryOpKind::BitNot => UnOp::BitNot(NumTy::of(&et)),
                    UnaryOpKind::Neg => UnOp::Neg(NumTy::of(&et)),
                };
                self.assign_temp(et.clone(), Rvalue::Un(u, v))
            }
            ExprKind::StructLiteral { type_name, base, fields, .. } => {
                self.struct_literal(type_name, base.as_deref(), fields, span)?
            }
            ExprKind::FieldAccess { object, field } => {
                let (o, ot) = self.expr(object)?;
                self.read_field(o, &ot, field, &et, span)?
            }
            ExprKind::Index { object, index } => {
                let (o, ot) = self.expr(object)?;
                match &ot {
                    Ty::Array(el) => {
                        let (i, _) = self.expr(index)?;
                        self.assign_temp(
                            et.clone(),
                            Rvalue::Op(Op::ArrayGet((**el).clone()), vec![o, i]),
                        )
                    }
                    Ty::Str => {
                        let (i, _) = self.expr(index)?;
                        self.assign_temp(Ty::Char, Rvalue::Op(Op::StrIndex, vec![o, i]))
                    }
                    Ty::Map(v) => {
                        let (i, _) = self.expr(index)?;
                        self.assign_temp(
                            et.clone(),
                            Rvalue::Op(Op::DictGet((**v).clone()), vec![o, i]),
                        )
                    }
                    Ty::Struct(_) => match &index.kind {
                        ExprKind::Str(field) => self.read_field(o, &ot, field, &et, span)?,
                        _ => return Err(internal("a struct indexed by a non-literal", span)),
                    },
                    Ty::Bytes => {
                        let (i, _) = self.expr(index)?;
                        self.assign_temp(Ty::Int, Rvalue::Op(Op::BytesGet, vec![o, i]))
                    }
                    Ty::Stream(_) => return Err(not_yet("streams", span)),
                    other => return Err(internal(format!("an index into {other}"), span)),
                }
            }
            ExprKind::Array { elements } => {
                let Ty::Array(el) = &et else {
                    return Err(internal("an array literal that is not an array", span));
                };
                let mut ops = Vec::with_capacity(elements.len());
                for x in elements {
                    let (v, t) = self.expr(x)?;
                    let v = self.coerce(v, &t, el, x.span)?;
                    ops.push(self.consume(v));
                }
                self.assign_temp(et.clone(), Rvalue::Array((**el).clone(), ops))
            }
            ExprKind::Dict { entries } => {
                let Ty::Map(vt) = &et else {
                    return Err(internal("a dict literal that is not a dict", span));
                };
                let mut ops = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let (kv, kt) = self.expr(k)?;
                    let kv = self.coerce(kv, &kt, &Ty::Str, k.span)?;
                    let kv = self.consume(kv);
                    let (vv, vty) = self.expr(v)?;
                    let vv = self.coerce(vv, &vty, vt, v.span)?;
                    let vv = self.consume(vv);
                    ops.push((kv, vv));
                }
                self.assign_temp(et.clone(), Rvalue::Dict((**vt).clone(), ops))
            }
            ExprKind::FStr { parts } => {
                let mut ops = Vec::new();
                for p in parts {
                    match p {
                        FStrPart::Literal(s) => {
                            if !s.is_empty() {
                                ops.push(Operand::Const(Const::Str(s.clone())));
                            }
                        }
                        FStrPart::Expr(x) => {
                            let (v, t) = self.expr(x)?;
                            ops.push(self.text_of(v, &t));
                        }
                    }
                }
                match ops.len() {
                    0 => Operand::Const(Const::Str(String::new())),
                    _ => self.assign_temp(Ty::Str, Rvalue::Op(Op::StrCat, ops)),
                }
            }
            ExprKind::Await { expr } => {
                let (f, _) = self.expr(expr)?;
                self.assign_temp(et.clone(), Rvalue::Op(Op::Await(et.clone()), vec![f]))
            }
            ExprKind::PromptLiteral { body } => {
                let (v, t) = self.expr(body)?;
                self.coerce(v, &t, &Ty::Str, span)?
            }
            // `src/infer/rewrite.rs` turns every dereference it can into a call
            // into the inference prelude; what is left is a kind it cannot.
            ExprKind::PromptDeref { grammar_expr: Some(_), .. } => {
                return Err(not_yet("a prompt constrained by a grammar", span));
            }
            ExprKind::PromptDeref { output_type: Some(t), .. } => {
                return Err(not_yet(format!("a prompt decoded into {t}"), span));
            }
            ExprKind::PromptDeref { .. } => {
                return Err(internal("a prompt dereference the rewrite left", span));
            }
            // The per-file pass replaces every raw pipe with a `Pipe`.
            ExprKind::RawPipe { .. } => {
                return Err(internal("a pipe the per-file pass did not flatten", span));
            }
            ExprKind::Pipe { source, hoisted, piece, body } => {
                return self.pipe(e, &et, source, hoisted, piece, body);
            }
        };
        Ok((op, et))
    }

    /// Purpose: This function gives the text of a value, as `print` shows it.
    ///
    /// Inputs:
    /// - `v`, `t`: the value and its type.
    ///
    /// Outputs: a string operand; a string is itself.
    pub(super) fn text_of(&mut self, v: Operand, t: &Ty) -> Operand {
        match t {
            Ty::Str => v,
            Ty::Char => self.assign_temp(Ty::Str, Rvalue::Op(Op::CharToStr, vec![v])),
            other => self.assign_temp(Ty::Str, Rvalue::Op(Op::ToStr(other.clone()), vec![v])),
        }
    }

    /// Purpose: This function reads a field off a struct value.
    ///
    /// Inputs:
    /// - `o`, `ot`: the object and its static type.
    /// - `field`: the field.
    /// - `et`: the expression's type.
    /// - `span`: where.
    ///
    /// Outputs: the field's value.
    fn read_field(
        &mut self,
        o: Operand,
        ot: &Ty,
        field: &str,
        et: &Ty,
        span: Span,
    ) -> Result<Operand> {
        match ot {
            Ty::Struct(sn) => match self.l.field(sn, field) {
                Some((index, ft)) => {
                    let v = self.assign_temp(
                        ft.clone(),
                        Rvalue::Field { object: o, strukt: sn.clone(), index },
                    );
                    self.coerce(v, &ft, et, span)
                }
                None => Err(not_yet("bound methods used as values", span)),
            },
            Ty::Error if field == "message" => {
                Ok(self.assign_temp(Ty::Str, Rvalue::Op(Op::ErrorMessage, vec![o])))
            }
            other => Err(not_yet(format!("methods of {other} used as values"), span)),
        }
    }

    /// Purpose: This function lowers a struct literal.
    ///
    /// Inputs:
    /// - `type_name`: the struct.
    /// - `base`: the `...base` value, if any.
    /// - `fields`: the fields the literal names, or all of them with no base.
    /// - `span`: where.
    ///
    /// Outputs: the new struct.
    fn struct_literal(
        &mut self,
        type_name: &str,
        base: Option<&'a Expr>,
        fields: &'a [(String, Expr, bool)],
        span: Span,
    ) -> Result<Operand> {
        let def =
            self.l.strukt(type_name).ok_or_else(|| internal("an unknown struct", span))?.clone();
        let mut given: HashMap<&str, Operand> = HashMap::new();
        // A prompt field holds its text, as any prompt does.
        for (name, value, _) in fields {
            let (v, t) = self.expr(value)?;
            let ft = self
                .l
                .field(type_name, name)
                .map(|(_, t)| t)
                .ok_or_else(|| internal("an unknown field", span))?;
            let v = self.coerce(v, &t, &ft, value.span)?;
            given.insert(name.as_str(), v);
        }
        let base = match base {
            Some(b) => Some(self.expr(b)?),
            None => None,
        };
        let mut ops = Vec::with_capacity(def.fields.len());
        for (fname, fty) in &def.fields {
            let v = match given.remove(fname.as_str()) {
                Some(v) => self.consume(v),
                None => {
                    let (bo, bt) = base
                        .clone()
                        .ok_or_else(|| internal(format!("no value for field '{fname}'"), span))?;
                    let Ty::Struct(bn) = &bt else {
                        return Err(internal("a copy-with base that is not a struct", span));
                    };
                    let v = match self.l.field(bn, fname) {
                        Some((bi, bft)) => {
                            let v = self.assign_temp(
                                bft.clone(),
                                Rvalue::Field { object: bo, strukt: bn.clone(), index: bi },
                            );
                            self.coerce(v, &bft, fty, span)?
                        }
                        // The base's class lacks the field, so it takes its
                        // declared default, which the checker checked here.
                        None => {
                            let default = self
                                .l
                                .defaults
                                .get(&(type_name, fname.as_str()))
                                .copied()
                                .ok_or_else(|| internal("a base missing a field", span))?;
                            let (v, t) = self.expr(default)?;
                            self.coerce(v, &t, fty, default.span)?
                        }
                    };
                    self.consume(v)
                }
            };
            ops.push(v);
        }
        Ok(self.assign_temp(
            Ty::Struct(type_name.to_string()),
            Rvalue::Struct(type_name.to_string(), ops),
        ))
    }

    /// Purpose: This function lowers a binary operator.
    ///
    /// Inputs:
    /// - `op`: the operator.
    /// - `left`, `right`: the operands.
    /// - `et`: the result type.
    /// - `span`: where.
    ///
    /// Outputs: the result.
    fn binop(
        &mut self,
        op: &BinOpKind,
        left: &'a Expr,
        right: &'a Expr,
        et: &Ty,
        span: Span,
    ) -> Result<Operand> {
        if matches!(op, BinOpKind::And | BinOpKind::Or) {
            return self.short_circuit(matches!(op, BinOpKind::And), left, right);
        }
        let nil_l = is_nil(left);
        let nil_r = is_nil(right);
        if matches!(op, BinOpKind::Eq | BinOpKind::Ne) && (nil_l || nil_r) {
            let other = if nil_l { right } else { left };
            let (v, t) = self.expr(other)?;
            let is = match t {
                Ty::Nil => Operand::Const(Const::Bool(true)),
                // A missing error is a null pointer.
                Ty::Opt(_) | Ty::Error => self.assign_temp(Ty::Bool, Rvalue::IsNil(v)),
                _ => Operand::Const(Const::Bool(false)),
            };
            return Ok(if matches!(op, BinOpKind::Ne) {
                self.assign_temp(Ty::Bool, Rvalue::Un(UnOp::Not, is))
            } else {
                is
            });
        }
        let (l, lt) = self.expr(left)?;
        let (r, rt) = self.expr(right)?;
        let arith = |o: &BinOpKind| match o {
            BinOpKind::Add => Some(ArithOp::Add),
            BinOpKind::Sub => Some(ArithOp::Sub),
            BinOpKind::Mul => Some(ArithOp::Mul),
            BinOpKind::Div => Some(ArithOp::Div),
            BinOpKind::Mod => Some(ArithOp::Rem),
            _ => None,
        };
        let cmp = |o: &BinOpKind| match o {
            BinOpKind::Eq => Some(CmpOp::Eq),
            BinOpKind::Ne => Some(CmpOp::Ne),
            BinOpKind::Lt => Some(CmpOp::Lt),
            BinOpKind::Le => Some(CmpOp::Le),
            BinOpKind::Gt => Some(CmpOp::Gt),
            BinOpKind::Ge => Some(CmpOp::Ge),
            _ => None,
        };
        if let Some(a) = arith(op) {
            return Ok(match et {
                Ty::Str => {
                    let l = self.char_as_str(l, &lt);
                    let r = self.char_as_str(r, &rt);
                    self.assign_temp(Ty::Str, Rvalue::Bin(BinOp::Concat, l, r))
                }
                Ty::Float => {
                    let l = self.as_float(l, &lt);
                    let r = self.as_float(r, &rt);
                    self.assign_temp(Ty::Float, Rvalue::Bin(BinOp::Arith(a, NumTy::Float), l, r))
                }
                // Both sides have the one sized type; a literal takes it here.
                Ty::Sized(s) => {
                    let l = self.coerce(l, &lt, et, span)?;
                    let r = self.coerce(r, &rt, et, span)?;
                    self.assign_temp(
                        et.clone(),
                        Rvalue::Bin(BinOp::Arith(a, NumTy::Sized(*s)), l, r),
                    )
                }
                _ => self.assign_temp(Ty::Int, Rvalue::Bin(BinOp::Arith(a, NumTy::Int), l, r)),
            });
        }
        if let Some(c) = cmp(op) {
            let ordered = !matches!(c, CmpOp::Eq | CmpOp::Ne);
            // A char compares with a string as the one-character string it spells.
            if matches!((&lt, &rt), (Ty::Char, Ty::Str) | (Ty::Str, Ty::Char)) {
                let l = self.char_as_str(l, &lt);
                let r = self.char_as_str(r, &rt);
                return Ok(self.assign_temp(Ty::Bool, Rvalue::Bin(BinOp::Cmp(c, Ty::Str), l, r)));
            }
            if ordered && matches!((&lt, &rt), (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int)) {
                let l = self.as_float(l, &lt);
                let r = self.as_float(r, &rt);
                return Ok(self.assign_temp(Ty::Bool, Rvalue::Bin(BinOp::Cmp(c, Ty::Float), l, r)));
            }
            let ct = match (&lt, &rt) {
                (Ty::Sized(_), _) => lt.clone(),
                (_, Ty::Sized(_)) => rt.clone(),
                _ => self.common(&lt, &rt),
            };
            let l = self.coerce(l, &lt, &ct, span)?;
            let r = self.coerce(r, &rt, &ct, span)?;
            return Ok(self.assign_temp(Ty::Bool, Rvalue::Bin(BinOp::Cmp(c, ct), l, r)));
        }
        let bit = match op {
            BinOpKind::BitAnd => Some(BitOp::And),
            BinOpKind::BitOr => Some(BitOp::Or),
            BinOpKind::BitXor => Some(BitOp::Xor),
            BinOpKind::Shl => Some(BitOp::Shl),
            BinOpKind::Shr => Some(BitOp::Shr),
            _ => None,
        };
        if let Some(b) = bit {
            // A shift's amount stays an int; the other operators take both
            // sides at the result's type, so a literal takes a sized one.
            let l = self.coerce(l, &lt, et, span)?;
            let r = if matches!(b, BitOp::Shl | BitOp::Shr) {
                r
            } else {
                self.coerce(r, &rt, et, span)?
            };
            return Ok(
                self.assign_temp(et.clone(), Rvalue::Bin(BinOp::Bit(b, NumTy::of(et)), l, r))
            );
        }
        // `in` and `not in`.
        let found = match &rt {
            Ty::Array(el) => {
                let l = self.coerce(l, &lt, el, span)?;
                self.assign_temp(
                    Ty::Bool,
                    Rvalue::Op(Op::ArrayContains((**el).clone()), vec![r, l]),
                )
            }
            Ty::Map(v) => {
                self.assign_temp(Ty::Bool, Rvalue::Op(Op::DictHas((**v).clone()), vec![r, l]))
            }
            Ty::Str => {
                let l = self.char_as_str(l, &lt);
                self.assign_temp(Ty::Bool, Rvalue::Op(Op::Str(StrMethod::Contains), vec![r, l]))
            }
            other => return Err(not_yet(format!("'in' on {other}"), span)),
        };
        Ok(if matches!(op, BinOpKind::NotIn) {
            self.assign_temp(Ty::Bool, Rvalue::Un(UnOp::Not, found))
        } else {
            found
        })
    }

    /// Purpose: This function turns a char into the string spelling it.
    ///
    /// Inputs:
    /// - `v`, `t`: the value and its type.
    ///
    /// Outputs: a string operand; anything but a char is returned unchanged.
    pub(super) fn char_as_str(&mut self, v: Operand, t: &Ty) -> Operand {
        if *t == Ty::Char {
            self.assign_temp(Ty::Str, Rvalue::Op(Op::CharToStr, vec![v]))
        } else {
            v
        }
    }

    /// Purpose: This function turns an int into a float.
    ///
    /// Inputs:
    /// - `v`, `t`: the value and its type.
    ///
    /// Outputs: a float operand.
    pub(super) fn as_float(&mut self, v: Operand, t: &Ty) -> Operand {
        match (t, &v) {
            (Ty::Int, Operand::Const(Const::Int(i))) => Operand::Const(Const::Float(*i as f64)),
            (Ty::Int, _) => self.assign_temp(Ty::Float, Rvalue::IntToFloat(v)),
            _ => v,
        }
    }

    /// Purpose: This function lowers a call into a Rust package. Each argument
    /// goes as its parameter's type, which the package's record declares, and
    /// the result is read back as the declared result.
    ///
    /// Inputs:
    /// - `name`: the `__native$<pkg>$<fn>` reference.
    /// - `args`: the arguments.
    /// - `span`: where.
    ///
    /// Outputs: the result.
    fn native_call(&mut self, name: &str, args: &'a [Expr], span: Span) -> Result<Operand> {
        let sig = self.l.native_sigs.get(name).cloned().ok_or_else(|| {
            internal(format!("a call to '{name}', which no Rust package exports"), span)
        })?;
        let mut ops = Vec::with_capacity(args.len());
        let mut tys = Vec::with_capacity(args.len());
        for (a, p) in args.iter().zip(&sig.params) {
            let (v, t) = self.expr(a)?;
            let want = norm(p);
            let v = self.coerce(v, &t, &want, a.span)?;
            ops.push(v);
            tys.push(want);
        }
        let ret = norm(&sig.ret);
        let call = NativeCall { symbol: sig.symbol, display: sig.display, params: tys, ret };
        let ret = call.ret.clone();
        Ok(self.assign_temp(ret, Rvalue::Op(Op::Native(Box::new(call)), ops)))
    }

    /// Purpose: This function lowers `&&` and `||`, which skip the right side
    /// when the left already decides.
    ///
    /// Inputs:
    /// - `and`: true for `&&`.
    /// - `left`, `right`: the operands.
    ///
    /// Outputs: the result.
    fn short_circuit(&mut self, and: bool, left: &'a Expr, right: &'a Expr) -> Result<Operand> {
        let (l, _) = self.expr(left)?;
        let res = self.new_local(Ty::Bool, None);
        self.emit(Stmt::Assign(Place::Local(res), Rvalue::Use(l)));
        let rhs = self.new_block();
        let end = self.new_block();
        let cond = Operand::Copy(Place::Local(res));
        self.terminate(if and { Term::If(cond, rhs, end) } else { Term::If(cond, end, rhs) });
        self.switch_to(rhs);
        let (r, _) = self.expr(right)?;
        self.emit(Stmt::Assign(Place::Local(res), Rvalue::Use(r)));
        self.terminate(Term::Goto(end));
        self.switch_to(end);
        Ok(Operand::Copy(Place::Local(res)))
    }

    // ── calls ────────────────────────────────────────────────────────────

    /// Purpose: This function lowers a call, by what the checker resolved it to.
    ///
    /// Inputs:
    /// - `e`: the call node.
    /// - `et`: its type.
    /// - `callee`, `args`, `kwargs`: its parts.
    ///
    /// Outputs: the result and its type.
    fn call(
        &mut self,
        e: &'a Expr,
        et: &Ty,
        callee: &'a Expr,
        args: &'a [Expr],
        kwargs: &'a [(String, Expr)],
    ) -> Result<(Operand, Ty)> {
        let span = e.span;
        let target = self.targets.get(&addr(e)).cloned();
        match target {
            Some(Target::Instance(k)) => {
                let def = self
                    .l
                    .def_of(&k.func)
                    .ok_or_else(|| internal("a call to a function with no definition", span))?;
                let vals = self.instance_vals(&k, def, callee, args, kwargs, span)?;
                let r = self.call_instance(&k, def, vals, span)?;
                Ok((r, et.clone()))
            }
            Some(Target::Dispatch(kinds)) => {
                let ExprKind::FieldAccess { object, .. } = &callee.kind else {
                    return Err(internal("a dispatched call with no receiver", span));
                };
                let (recv, rt) = self.expr(object)?;
                let mut argv = Vec::new();
                for a in args {
                    let (v, t) = self.expr(a)?;
                    argv.push((v, t, a.span));
                }
                if !kwargs.is_empty() {
                    return Err(not_yet("keyword arguments to an inherited method", span));
                }
                let res = self.temp(et.clone());
                let tid = self.assign_temp(Ty::Int, Rvalue::TypeId(recv.clone()));
                let end = self.new_block();
                let default = self.new_block();
                let mut cases = Vec::new();
                let mut bodies = Vec::new();
                for (kind, k) in &kinds {
                    let b = self.new_block();
                    let id = *self
                        .l
                        .struct_ids
                        .get(kind)
                        .ok_or_else(|| internal("an unknown struct", span))?;
                    cases.push((id as i64, b));
                    bodies.push((b, k.clone()));
                }
                self.terminate(Term::Switch { value: tid, cases, default });
                for (b, k) in bodies {
                    self.switch_to(b);
                    let def = self
                        .l
                        .def_of(&k.func)
                        .ok_or_else(|| internal("a method with no body", span))?;
                    let mut vals: Vec<Option<(Operand, Ty, Span)>> = vec![None; def.params.len()];
                    vals[0] = Some((recv.clone(), rt.clone(), span));
                    for (i, a) in argv.iter().enumerate() {
                        if i + 1 < vals.len() {
                            vals[i + 1] = Some(a.clone());
                        }
                    }
                    let (_, _, ret) = self.l.sig(&k, span)?;
                    let r = self.call_instance(&k, def, vals, span)?;
                    let r = self.coerce(r, &ret, et, span)?;
                    let r = self.consume(r);
                    self.emit(Stmt::Assign(Place::Local(res), Rvalue::Use(r)));
                    self.terminate(Term::Goto(end));
                }
                self.switch_to(default);
                self.terminate(Term::Unreachable);
                self.switch_to(end);
                self.fresh.insert(res);
                Ok((Operand::Copy(Place::Local(res)), et.clone()))
            }
            Some(Target::Builtin(name)) => self.builtin(e, et, &name, callee, args, kwargs),
            None => match &callee.kind {
                ExprKind::Identifier(n) if n.starts_with("__native$") => {
                    let o = self.native_call(n, args, span)?;
                    let t = self.l.native_sigs.get(n.as_str()).map_or(et.clone(), |s| norm(&s.ret));
                    let o = self.coerce(o, &t, et, span)?;
                    Ok((o, et.clone()))
                }
                _ => {
                    // A call through a joined function type: a pointer.
                    let (c, ct) = self.expr(callee)?;
                    let Ty::Fn(ft) = &ct else {
                        return Err(internal(format!("a call through a {ct}"), span));
                    };
                    let mut argv = Vec::new();
                    for (a, pt) in args.iter().zip(&ft.params) {
                        let (v, t) = self.expr(a)?;
                        argv.push(self.coerce(v, &t, &norm(pt), a.span)?);
                    }
                    let rt = norm(&ft.ret);
                    Ok((self.assign_temp(rt.clone(), Rvalue::CallIndirect(c, argv)), et.clone()))
                }
            },
        }
    }

    /// Purpose: This function calls one instance with its arguments bound,
    /// filling defaults and converting each to its parameter's type.
    ///
    /// Inputs:
    /// - `k`: the instance.
    /// - `def`: its definition.
    /// - `vals`: each parameter's argument, if the call gave one.
    /// - `span`: where.
    ///
    /// Outputs: the call's result.
    pub(super) fn call_instance(
        &mut self,
        k: &InstKey,
        def: FnDef<'a>,
        vals: Vec<Option<(Operand, Ty, Span)>>,
        span: Span,
    ) -> Result<Operand> {
        let (id, _, ret) = self.l.sig(k, span)?;
        let argv = self.instance_args(k, def, vals, span)?;
        if def.is_async {
            return Ok(self.assign_temp(ret, Rvalue::Spawn(id, argv)));
        }
        Ok(self.assign_temp(ret, Rvalue::Call(id, argv)))
    }

    /// Purpose: This function works out a call's arguments, in the order the
    /// called function's parameters take them.
    ///
    /// Inputs:
    /// - `k`: the instance called.
    /// - `def`: its definition.
    /// - `callee`, `args`, `kwargs`: the call's parts.
    /// - `span`: where.
    ///
    /// Outputs: one value per parameter, `None` where the default applies.
    #[allow(clippy::type_complexity)]
    pub(super) fn instance_vals(
        &mut self,
        k: &InstKey,
        def: FnDef<'a>,
        callee: &'a Expr,
        args: &'a [Expr],
        kwargs: &'a [(String, Expr)],
        span: Span,
    ) -> Result<Vec<Option<(Operand, Ty, Span)>>> {
        let mut vals: Vec<Option<(Operand, Ty, Span)>> = vec![None; def.params.len()];
        let mut next = 0;
        // A method called on an object takes the object as its first
        // argument. `Parent::m(self, …)` already has it among the args.
        if let (FnKey::Method(..), ExprKind::FieldAccess { object, .. }) = (&k.func, &callee.kind) {
            let (o, t) = self.expr(object)?;
            vals[0] = Some((o, t, object.span));
            next = 1;
        }
        for a in args {
            let (v, t) = self.expr(a)?;
            if next >= vals.len() {
                return Err(internal("too many arguments", span));
            }
            vals[next] = Some((v, t, a.span));
            next += 1;
        }
        for (name, v) in kwargs {
            let i = def
                .params
                .iter()
                .position(|(p, _)| p == name)
                .ok_or_else(|| internal("an unknown keyword argument", span))?;
            let (o, t) = self.expr(v)?;
            vals[i] = Some((o, t, v.span));
        }
        Ok(vals)
    }

    /// Purpose: This function converts a call's values to the called
    /// instance's parameter types, filling defaults where none was given.
    ///
    /// Inputs:
    /// - `k`: the instance called.
    /// - `def`: its definition.
    /// - `vals`: what `instance_vals` gave.
    /// - `span`: where.
    ///
    /// Outputs: the arguments.
    pub(super) fn instance_args(
        &mut self,
        k: &InstKey,
        def: FnDef<'a>,
        vals: Vec<Option<(Operand, Ty, Span)>>,
        span: Span,
    ) -> Result<Vec<Operand>> {
        let (_, params, _) = self.l.sig(k, span)?;
        let mut argv = Vec::with_capacity(params.len());
        for (i, (v, pt)) in vals.into_iter().zip(&params).enumerate() {
            let (o, t, sp) = match v {
                Some(x) => x,
                None => {
                    let d = def.params[i]
                        .1
                        .as_ref()
                        .ok_or_else(|| internal("a missing argument", span))?;
                    let (o, t) = self.default_arg(d)?;
                    (o, t, d.span)
                }
            };
            argv.push(self.coerce(o, &t, pt, sp)?);
        }
        Ok(argv)
    }

    /// Purpose: This function lowers a parameter's default value at a call.
    ///
    /// Inputs:
    /// - `d`: the default, always a literal.
    ///
    /// Outputs: its value and type.
    fn default_arg(&mut self, d: &'a Expr) -> Result<(Operand, Ty)> {
        if self.types.contains_key(&addr(d)) {
            return self.expr(d);
        }
        Ok(match &d.kind {
            ExprKind::Integer(i) => (Operand::Const(Const::Int(*i)), Ty::Int),
            ExprKind::Float(x) => (Operand::Const(Const::Float(*x)), Ty::Float),
            ExprKind::Bool(b) => (Operand::Const(Const::Bool(*b)), Ty::Bool),
            ExprKind::Str(s) => (Operand::Const(Const::Str(s.clone())), Ty::Str),
            _ => (Operand::Const(Const::Nil(Ty::Nil)), Ty::Nil),
        })
    }
}

/// Purpose: This function says whether an expression is the literal `nil`.
///
/// Inputs:
/// - `e`: the expression.
///
/// Outputs: true for `nil`, `None` or `null`.
fn is_nil(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Identifier(n) if n == "nil" || n == "None" || n == "null")
}
