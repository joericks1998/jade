//! Rejects shared mutation across task boundaries.
//!
//! Jade tasks run on real OS threads and share one heap: `jrt_spawn` hands the
//! task live `TAG_PTR` pointers, and the objects behind them are aliased, not
//! copied. `ObjHeader`'s refcount is atomic so the *header* survives that, but
//! the payload does not — `ArrayObj::data` is a plain `Vec` with no lock on the
//! AOT path. Two tasks pushing to one array is a data race today.
//!
//! There are three ways to close that: lock every collection (a cost paid by
//! every program, concurrent or not), deep-copy task arguments (sound, but it
//! silently changes what `async` means — a mutation inside a task stops being
//! visible outside it), or refuse to compile the programs that would race.
//! This module is the third. Nothing is locked, nothing is copied, and the
//! programs that survive are the ones that were already correct.
//!
//! That is also the better language: a task that mutates a global or reaches
//! into a caller's struct is order-dependent by construction, and its result
//! depends on which thread got there first. Passing values in and returning
//! results out is not a workaround for this rule — it is the thing the rule
//! exists to encourage.
//!
//! ## What is rejected
//!
//! For any function reachable from a `Spawn`, transitively:
//!
//!  * **`SetGlobal`** — a task writing a global. Note the two engines do not
//!    even agree on what this means today: the VM snapshots globals per task
//!    (`VmState::new_for_spawn`), so the write is invisible to the parent,
//!    while AOT lowers globals to shared LLVM cells and the write is a race.
//!    Rejecting the program removes a live divergence rather than picking a
//!    side.
//!  * **`SetIndex` / `SetField` on a shared object** — assigning through a
//!    value that arrived as a parameter or came from a global.
//!  * **A mutating method on a shared object** — `push`, `sort`, and friends.
//!
//! ## What is allowed
//!
//!  * *Reading* globals. Every async example calls other `async fn`s and
//!    references struct types, and both are global reads; forbidding them
//!    would reject essentially every real program. Reads are only unsound if
//!    something writes concurrently, and writes are what this pass removes.
//!  * Mutating anything the task allocated itself. `MakeArray`/`MakeDict`/
//!    `MakeStruct` produce unaliased objects, so taint stops there — a task
//!    building and sorting a local array is fine.
//!  * `SetLocal`, including on a parameter slot. `x = 5` rebinds a slot; it
//!    does not touch the object the caller still holds.
//!
//! ## Precision
//!
//! Taint is conservative in the one direction that matters: a `Call` whose
//! target cannot be resolved statically returns a tainted value, so an unknown
//! callee can never launder a shared object into an untainted register. False
//! positives are therefore possible and false negatives are not, which is the
//! correct bias — a false positive is a compile error the author can see and
//! work around, a false negative is a data race nobody sees.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::bytecode::{Chunk, CompiledFn, Instr, Reg};
use crate::frontend::error::Span;

/// Methods that mutate their receiver in place. A call to one of these on a
/// shared object is the array/dict equivalent of `SetIndex`.
const MUTATING_METHODS: &[&str] =
    &["push", "pop", "insert", "remove", "clear", "sort", "reverse", "extend", "set", "update"];

/// What a function does to state it does not exclusively own.
///
/// Both flags are "does this happen anywhere in the call tree", so they compose
/// by `|=` up the graph and reach a fixed point.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Effects {
    /// Writes a global somewhere in its call tree.
    writes_global: bool,
    /// Mutates an object reachable from one of its parameters.
    mutates_shared: bool,
}

impl Effects {
    fn is_clean(self) -> bool {
        !self.writes_global && !self.mutates_shared
    }
}

/// Why a spawn was rejected, in the words the user needs to fix it.
#[derive(Debug, Clone)]
pub struct Violation {
    /// The async function that cannot be spawned.
    pub task: String,
    /// The specific thing it does, e.g. "writes to a global".
    pub what: String,
    /// Where the offending operation is, not where the spawn is — the spawn is
    /// only the point at which the operation becomes a race.
    pub span: Span,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "async function `{}` {}.\n\
             Tasks run concurrently on a shared heap, so this is a data race.\n\
             Pass the value in as a parameter and return the result instead.",
            self.task, self.what
        )
    }
}

/// Every function in the program, addressable by pointer identity and by name.
struct FnTable {
    fns: Vec<Arc<CompiledFn>>,
    /// `Arc::as_ptr` → index. Function bodies are shared by `Arc`, so pointer
    /// identity is the only reliable key; names collide across modules.
    by_ptr: HashMap<usize, usize>,
    /// Declared name → index. Used to resolve a `GetGlobal` callee.
    by_name: HashMap<String, usize>,
}

impl FnTable {
    fn collect(
        top: &Chunk,
        extend_methods: &HashMap<String, HashMap<String, Arc<CompiledFn>>>,
    ) -> Self {
        let mut t = FnTable { fns: Vec::new(), by_ptr: HashMap::new(), by_name: HashMap::new() };
        t.walk_chunk(top);
        for methods in extend_methods.values() {
            for f in methods.values() {
                t.add(f);
            }
        }
        t
    }

    fn add(&mut self, f: &Arc<CompiledFn>) {
        let key = Arc::as_ptr(f) as usize;
        if self.by_ptr.contains_key(&key) {
            return;
        }
        let idx = self.fns.len();
        self.by_ptr.insert(key, idx);
        // First definition wins: a local function shadows an imported one of the
        // same name, matching how the VM merges module scopes.
        self.by_name.entry(f.chunk.name.clone()).or_insert(idx);
        self.fns.push(Arc::clone(f));
        let f = Arc::clone(f);
        self.walk_chunk(&f.chunk);
    }

    fn walk_chunk(&mut self, c: &Chunk) {
        for f in &c.fn_defs {
            self.add(f);
        }
    }

    fn idx_of(&self, f: &Arc<CompiledFn>) -> Option<usize> {
        self.by_ptr.get(&(Arc::as_ptr(f) as usize)).copied()
    }
}

/// Per-chunk dataflow: which registers currently hold a shared (aliased) value.
///
/// Registers are SSA-ish but reused across branches, so this runs to a fixed
/// point rather than assuming a single linear pass converges. Taint only ever
/// grows, so the iteration terminates.
struct Taint {
    regs: HashSet<Reg>,
    slots: HashSet<u32>,
    /// reg holding a `GetField` result → (receiver reg, field name). Needed to
    /// recognize `arr.push(x)`, which emits `GetField` then `Call`.
    getfield: HashMap<Reg, (Reg, String)>,
    /// reg → the function it statically holds, for resolving call targets.
    reg_fn: HashMap<Reg, usize>,
}

/// Analyze one function: what it does to shared state, and where.
///
/// `eff` is the current (possibly incomplete) effect map for callees; the
/// caller iterates this to a fixed point.
fn analyze(f: &CompiledFn, table: &FnTable, eff: &[Effects]) -> (Effects, Vec<(String, Span)>) {
    let n_params = f.params.len() as u32;
    let mut t = Taint {
        regs: HashSet::new(),
        // Parameters arrive in slots 0..n and alias whatever the caller passed.
        slots: (0..n_params).collect(),
        getfield: HashMap::new(),
        reg_fn: HashMap::new(),
    };
    let mut effects = Effects::default();
    let mut found: Vec<(String, Span)> = Vec::new();

    // Taint is monotonic, so re-running until nothing new appears converges.
    // Two passes suffice for straight-line code; loops may need more.
    for pass in 0..8 {
        let before = (t.regs.len(), t.slots.len());
        found.clear();
        let mut e = Effects::default();

        for (i, instr) in f.chunk.code.iter().enumerate() {
            let span = f.chunk.spans.get(i).copied().unwrap_or(Span { line: 0, col: 0 });
            step(instr, span, &mut t, table, eff, &mut e, &mut found);
        }

        effects = e;
        if (t.regs.len(), t.slots.len()) == before && pass > 0 {
            break;
        }
    }

    (effects, found)
}

/// One instruction's effect on taint, plus any violation it constitutes.
fn step(
    instr: &Instr,
    span: Span,
    t: &mut Taint,
    table: &FnTable,
    eff: &[Effects],
    e: &mut Effects,
    found: &mut Vec<(String, Span)>,
) {
    use Instr::*;

    // Any instruction writing `d` invalidates what `d` previously held.
    let clear = |t: &mut Taint, d: &Reg| {
        t.getfield.remove(d);
        t.reg_fn.remove(d);
    };

    match instr {
        // ── Sources of sharing ──────────────────────────────────────────────
        GetGlobal(d, name) => {
            clear(t, d);
            // A global holds an object the whole program can see.
            t.regs.insert(*d);
            if let Some(&idx) = table.by_name.get(name) {
                t.reg_fn.insert(*d, idx);
            }
        }
        GetLocal(d, s) => {
            clear(t, d);
            if t.slots.contains(s) {
                t.regs.insert(*d);
            } else {
                t.regs.remove(d);
            }
        }
        SetLocal(s, r) => {
            // Rebinding a slot, including a parameter slot, is not a mutation of
            // the caller's object — it only changes what this frame points at.
            if t.regs.contains(r) {
                t.slots.insert(*s);
            }
        }

        // ── Taint propagation ───────────────────────────────────────────────
        Move(d, s) => {
            clear(t, d);
            if t.regs.contains(s) {
                t.regs.insert(*d);
            } else {
                t.regs.remove(d);
            }
            if let Some(&u) = t.reg_fn.get(s) {
                t.reg_fn.insert(*d, u);
            }
        }
        GetIndex(d, o, _) => {
            clear(t, d);
            // Reaching into a shared object yields a shared sub-object.
            if t.regs.contains(o) {
                t.regs.insert(*d);
            } else {
                t.regs.remove(d);
            }
        }
        GetField(d, o, name) => {
            clear(t, d);
            if t.regs.contains(o) {
                t.regs.insert(*d);
            } else {
                t.regs.remove(d);
            }
            t.getfield.insert(*d, (*o, name.clone()));
        }
        LoadFn(d, _) | MakeClosure(d, _) => {
            clear(t, d);
            t.regs.remove(d);
        }

        // ── Fresh allocations are unaliased: taint stops here ───────────────
        MakeArray(d, _) | MakeDict(d, _) | MakeStruct(d, _, _) => {
            clear(t, d);
            t.regs.remove(d);
        }

        // ── Violations ──────────────────────────────────────────────────────
        SetGlobal(name, _) => {
            e.writes_global = true;
            found.push((format!("writes to the global `{name}`"), span));
        }
        SetIndex(o, _, _) => {
            if t.regs.contains(o) {
                e.mutates_shared = true;
                found.push(("assigns into a shared collection".to_string(), span));
            }
        }
        SetField(o, field, _) => {
            if t.regs.contains(o) {
                e.mutates_shared = true;
                found.push((format!("assigns to the field `{field}` of a shared struct"), span));
            }
        }

        // ── Calls: mutating methods, and effects inherited from the callee ──
        Call(d, callee, args) => {
            // `recv.push(x)` — a mutating method on a shared receiver.
            if let Some((recv, name)) = t.getfield.get(callee)
                && MUTATING_METHODS.contains(&name.as_str())
                && t.regs.contains(recv)
            {
                e.mutates_shared = true;
                found.push((format!("calls `{name}()` on a shared collection"), span));
            }
            // A statically-known callee contributes its own effects.
            if let Some(&uid) = t.reg_fn.get(callee)
                && let Some(callee_eff) = eff.get(uid)
            {
                if callee_eff.writes_global {
                    e.writes_global = true;
                    found.push((
                        format!("calls `{}`, which writes a global", table.fns[uid].chunk.name),
                        span,
                    ));
                }
                if callee_eff.mutates_shared && args.iter().any(|a| t.regs.contains(a)) {
                    e.mutates_shared = true;
                    found.push((
                        format!(
                            "passes a shared value to `{}`, which mutates it",
                            table.fns[uid].chunk.name
                        ),
                        span,
                    ));
                }
            }
            clear(t, d);
            // An unresolved callee may return anything, including an alias of an
            // argument. Assume the worst so nothing launders taint through it.
            t.regs.insert(*d);
        }

        // Everything else either produces a scalar or does not move objects in a
        // way that can create a new alias to caller state.
        _ => {
            if let Some(d) = dest_of(instr) {
                clear(t, &d);
                t.regs.remove(&d);
            }
        }
    }
}

/// The register an instruction writes, for the catch-all taint-clearing arm.
/// Only covers opcodes that produce a value; `None` means "writes nothing".
fn dest_of(instr: &Instr) -> Option<Reg> {
    use Instr::*;
    Some(match instr {
        LoadInt(d, _) | LoadFloat(d, _) | LoadBool(d, _) | LoadStr(d, _) | LoadNil(d) => *d,
        BuildFStr(d, _) | GetTypeName(d, _) | Not(d, _) | BitNot(d, _) => *d,
        Await(d, _) | Join(d, _) | Spawn(d, _, _) => *d,
        MakePrompt(d, _) | PromptDeref(d, _, _, _) => *d,
        CallNamed(d, _, _) => *d,
        _ => return None,
    })
}

/// Reject any spawn whose target mutates state it shares with the spawner.
///
/// Runs after `emit`, because the mutation opcodes (`SetGlobal`, `SetIndex`,
/// `SetField`) only exist in bytecode — the AST has assignment expressions that
/// do not distinguish rebinding a local from writing through a reference.
pub fn check(
    top: &Chunk,
    extend_methods: &HashMap<String, HashMap<String, Arc<CompiledFn>>>,
) -> Result<(), Violation> {
    let table = FnTable::collect(top, extend_methods);
    let n = table.fns.len();

    // Fixed point over the call graph. Effects only ever turn on, so this
    // terminates; the bound guards against a bug in that reasoning rather than
    // against a real program.
    let mut eff = vec![Effects::default(); n];
    let mut sites: Vec<Vec<(String, Span)>> = vec![Vec::new(); n];
    for _ in 0..n.max(1) + 2 {
        let mut changed = false;
        for i in 0..n {
            let (new_eff, new_sites) = analyze(&table.fns[i], &table, &eff);
            if new_eff != eff[i] {
                changed = true;
                eff[i] = new_eff;
            }
            sites[i] = new_sites;
        }
        if !changed {
            break;
        }
    }

    // Now find the spawns and check what they point at.
    let mut chunks: Vec<&Chunk> = vec![top];
    for f in &table.fns {
        chunks.push(&f.chunk);
    }

    for chunk in chunks {
        let mut reg_fn: HashMap<Reg, usize> = HashMap::new();
        for (i, instr) in chunk.code.iter().enumerate() {
            match instr {
                Instr::GetGlobal(d, name) => match table.by_name.get(name) {
                    Some(&idx) => {
                        reg_fn.insert(*d, idx);
                    }
                    None => {
                        reg_fn.remove(d);
                    }
                },
                Instr::LoadFn(d, idx) | Instr::MakeClosure(d, idx) => {
                    match chunk.fn_defs.get(*idx).and_then(|f| table.idx_of(f)) {
                        Some(u) => {
                            reg_fn.insert(*d, u);
                        }
                        None => {
                            reg_fn.remove(d);
                        }
                    }
                }
                Instr::Move(d, s) => match reg_fn.get(s).copied() {
                    Some(u) => {
                        reg_fn.insert(*d, u);
                    }
                    None => {
                        reg_fn.remove(d);
                    }
                },
                Instr::Spawn(_, callee, _) => {
                    let Some(&uid) = reg_fn.get(callee) else { continue };
                    if eff[uid].is_clean() {
                        continue;
                    }
                    // Report the operation, not the spawn: the author needs the
                    // line to change, and the spawn is only where it becomes a race.
                    let (what, span) = sites[uid].first().cloned().unwrap_or_else(|| {
                        (
                            "mutates shared state".to_string(),
                            chunk.spans.get(i).copied().unwrap_or(Span { line: 0, col: 0 }),
                        )
                    });
                    return Err(Violation { task: table.fns[uid].chunk.name.clone(), what, span });
                }
                _ => {}
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::compiler::{emit, type_infer};
    use crate::frontend::{lexer, parser};

    /// Compile `src`, returning the error text if the shared-mutation check
    /// rejected it. Goes through the real pipeline so the test exercises what
    /// `jade run` and `jade check` actually do.
    fn compile(src: &str) -> Result<(), String> {
        let tokens = lexer::tokenize(src).map_err(|e| e.to_string())?;
        let program = parser::parse(tokens).map_err(|e| e.to_string())?;
        let tprogram = type_infer::infer(program).map_err(|e| e.to_string())?;
        emit::emit(tprogram).map(|_| ()).map_err(|e| e.to_string())
    }

    fn rejection(src: &str) -> String {
        match compile(src) {
            Err(e) => e,
            Ok(()) => panic!("expected rejection, but the program compiled:\n{src}"),
        }
    }

    #[test]
    fn task_writing_a_global_is_rejected() {
        let e = rejection(
            r#"
            let counter = 0
            async fn bump() { counter = counter + 1 }
            await bump()
            "#,
        );
        assert!(e.contains("bump"), "should name the task: {e}");
        assert!(e.contains("counter"), "should name the global: {e}");
    }

    #[test]
    fn task_mutating_a_passed_collection_is_rejected() {
        let e = rejection(
            r#"
            async fn fill(arr) { arr.push(1) }
            let a = []
            await fill(a)
            "#,
        );
        assert!(e.contains("push"), "should name the mutating method: {e}");
    }

    #[test]
    fn task_assigning_a_field_of_a_passed_struct_is_rejected() {
        let e = rejection(
            r#"
            struct Counter { n }
            async fn bump(c) { c.n = c.n + 1 }
            let c = Counter { n: 0 }
            await bump(c)
            "#,
        );
        assert!(e.contains("n"), "should name the field: {e}");
    }

    /// The check must see through a synchronous helper, or any mutation can be
    /// laundered by moving it one call deeper.
    #[test]
    fn mutation_through_a_sync_helper_is_rejected() {
        let e = rejection(
            r#"
            let total = 0
            fn helper() { total = total + 1 }
            async fn worker() { helper() }
            await worker()
            "#,
        );
        assert!(e.contains("helper"), "should name the helper that mutates: {e}");
    }

    /// Taint stops at a fresh allocation. A task that builds its own array owns
    /// it outright, so mutating it races with nothing — rejecting this would
    /// make the rule useless in practice.
    #[test]
    fn task_mutating_its_own_array_is_allowed() {
        compile(
            r#"
            async fn build(n) {
                let out = []
                out.push(n)
                return out
            }
            let r = await build(3)
            "#,
        )
        .expect("a task may mutate what it allocated itself");
    }

    #[test]
    fn task_mutating_its_own_struct_is_allowed() {
        compile(
            r#"
            struct P { x }
            async fn make(n) {
                let p = P { x: 0 }
                p.x = n
                return p
            }
            let p = await make(7)
            "#,
        )
        .expect("a task may mutate a struct it allocated itself");
    }

    /// Rebinding a parameter slot is not a mutation of the caller's object.
    #[test]
    fn rebinding_a_parameter_is_allowed() {
        compile(
            r#"
            async fn f(x) {
                x = x + 1
                return x
            }
            let r = await f(1)
            "#,
        )
        .expect("SetLocal on a parameter slot rebinds, it does not mutate");
    }

    /// Reading globals must stay legal: calling another async fn and naming a
    /// struct type are both global reads, so forbidding them would reject
    /// essentially every real program.
    #[test]
    fn reading_globals_from_a_task_is_allowed() {
        compile(
            r#"
            let base = 10
            async fn inner(n) { return n * 2 }
            async fn outer(n) {
                let d = await inner(n)
                return d + base
            }
            let r = await outer(5)
            "#,
        )
        .expect("tasks may read globals; only writes race");
    }

    /// A sync function is not a task, so it may do whatever it likes.
    #[test]
    fn sync_functions_are_unrestricted() {
        compile(
            r#"
            let counter = 0
            fn bump() { counter = counter + 1 }
            bump()
            "#,
        )
        .expect("the rule applies to spawned functions only");
    }
}
