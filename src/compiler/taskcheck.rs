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

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use crate::bytecode::{Chunk, CompiledFn, Instr, Reg};
use crate::frontend::error::Span;

/// Methods that mutate their receiver in place. A call to one of these on a
/// shared object is the array/dict equivalent of `SetIndex`.
const MUTATING_METHODS: &[&str] =
    &["push", "pop", "insert", "remove", "clear", "sort", "reverse", "extend", "set", "update"];

/// `std/bytes` functions that build a blob rather than hand one back.
///
/// Each returns storage nothing else points at, so taint stops at the call the
/// same way it stops at `MakeArray` — which is what lets a task allocate its own
/// buffer and write octets into it. Without this a task that does
/// `let buf = bytes.zeros(64)` and then writes to `buf` is rejected for
/// assigning into a shared collection, which is the opposite of true.
const BYTES_CONSTRUCTORS: &[&str] = &["zeros", "from_ints", "concat"];

/// What a function does to state it does not exclusively own.
///
/// Both flags are "does this happen anywhere in the call tree", so they compose
/// by `|=` up the graph and reach a fixed point.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Effects {
    /// Writes a global somewhere in its call tree.
    writes_global: bool,
    /// Mutates an object reachable from one of its parameters.
    mutates_shared: bool,
    /// User globals it *reads* anywhere in its call tree.
    ///
    /// Reading a global is harmless on its own, which is why it is a set rather
    /// than a flag: what makes it a race is the *spawner* assigning that same
    /// name while the task is still running. Ordered, so a message naming one
    /// does not depend on hash order.
    reads_globals: BTreeSet<String>,
}

impl Effects {
    fn is_clean(&self) -> bool {
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
    /// reg → the global name it was loaded from. `bytes.zeros(n)` and
    /// `buf.zeros(n)` are the same three opcodes, so where the receiver came
    /// from is the only thing that tells the stdlib module from a user value.
    reg_global: HashMap<Reg, String>,
    /// reg → the function it statically holds, for resolving call targets.
    reg_fn: HashMap<Reg, usize>,
    /// Every global the *program* binds. Immutable and program-wide, but it
    /// rides here rather than as a ninth argument to `step`: a user variable
    /// shadows a stdlib module name, and the only question it answers is
    /// whether `bytes` still means the module.
    user_globals: HashSet<String>,
}

/// Analyze one function: what it does to shared state, and where.
///
/// `eff` is the current (possibly incomplete) effect map for callees; the
/// caller iterates this to a fixed point.
fn analyze(
    f: &CompiledFn,
    table: &FnTable,
    eff: &[Effects],
    user_globals: &HashSet<String>,
) -> (Effects, Vec<(String, Span)>) {
    let n_params = f.params.len() as u32;
    let mut t = Taint {
        regs: HashSet::new(),
        // Parameters arrive in slots 0..n and alias whatever the caller passed.
        slots: (0..n_params).collect(),
        getfield: HashMap::new(),
        reg_global: HashMap::new(),
        reg_fn: HashMap::new(),
        user_globals: user_globals.clone(),
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
            step(instr, span, &f.chunk.fn_defs, &mut t, table, eff, &mut e, &mut found);
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
    // The enclosing chunk's nested definitions, which is what a `LoadFn` or
    // `MakeClosure` index refers to.
    defs: &[Arc<CompiledFn>],
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
        t.reg_global.remove(d);
        t.reg_fn.remove(d);
    };

    match instr {
        // ── Sources of sharing ──────────────────────────────────────────────
        GetGlobal(d, name) => {
            clear(t, d);
            t.reg_global.insert(*d, name.clone());
            // A global holds an object the whole program can see.
            t.regs.insert(*d);
            if t.user_globals.contains(name) {
                e.reads_globals.insert(name.clone());
            }
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
            // A method reached through a receiver — `b.grow()` on an `extend`
            // block — resolves by name, because `FnTable` indexes extend methods
            // alongside ordinary functions. Without it a user method laundered
            // taint exactly the way a closure did: `extend Box { fn grow(self)
            // { self.items.push(9) } }` called from a task mutated the
            // spawner's array and the pass saw an unresolved call.
            //
            // Only ever consulted when this register is *called*, so a data
            // field that happens to share a name with a function costs nothing.
            if let Some(&u) = table.by_name.get(name) {
                t.reg_fn.insert(*d, u);
            }
        }
        LoadFn(d, idx) | MakeClosure(d, idx) => {
            clear(t, d);
            t.regs.remove(d);
            // Remember *which* function, so calling it through the register
            // inherits its effects the same way calling it by name does.
            //
            // Leaving this out let a closure launder everything: `let c = ||
            // s.push(9)` reaches the task as an ordinary value, the `Call` on it
            // resolved to no function, and its effects were never inherited —
            // so a task that ran the closure mutated the spawner's array with
            // nothing to say so. The scan that finds spawn sites already
            // resolved these; only this pass did not.
            if let Some(u) = defs.get(*idx).and_then(|f| table.idx_of(f)) {
                t.reg_fn.insert(*d, u);
            }
        }

        // ── Fresh allocations are unaliased: taint stops here ───────────────
        MakeArray(d, _) | MakeDict(d, _) => {
            clear(t, d);
            t.regs.remove(d);
        }

        // A struct literal allocates a fresh object, but what it holds may not
        // be fresh at all. A field takes a *value*, so a collection put into one
        // is the very same collection, whether it arrives as `{ items: shared }`
        // or is copied across from a `...base`. Taint travels with it either
        // way, or a task reaches a shared array through the struct wrapping it.
        //
        // The named-field half of the rule is older than copy-with and was
        // missing: this arm used to sit with `MakeArray` and `MakeDict` as an
        // unconditionally fresh allocation, so `Box { items: shared }` inside a
        // task compiled clean and pushed to the spawner's array on both engines.
        MakeStruct(d, _, fields, base) => {
            clear(t, d);
            t.regs.remove(d);
            let carries_shared = base.is_some_and(|b| t.regs.contains(&b))
                || fields.iter().any(|(_, r, _)| t.regs.contains(r));
            if carries_shared {
                t.regs.insert(*d);
            }
        }

        // ── Violations ──────────────────────────────────────────────────────
        SetGlobal(name, _) => {
            e.writes_global = true;
            found.push((format!("writes to the global `{name}`"), span));
        }
        SetIndex(o, _, _) => {
            // `o` is the *slot* of the binding being written, not a register
            // holding a copy of it: the emitter hands the instruction the
            // binding so the write lands in place (see `TStmt::IndexAssign`).
            // Slot taint lives in `slots`, so reading `regs` alone matched
            // nothing at all, and `async fn f(arr) { arr[0] = 9 }` compiled
            // clean while `arr.push(9)` next to it was correctly rejected.
            // Slots and registers are drawn from one counter, so the two sets
            // cannot borrow each other's numbers.
            if t.slots.contains(o) || t.regs.contains(o) {
                e.mutates_shared = true;
                found.push(("assigns into a shared collection".to_string(), span));
            }
        }
        SetIndexGlobal(name, _, _) => {
            // `writes_global` and not `mutates_shared`, because this *is* a
            // write to a global: the only difference from `SetGlobal` is that
            // the binding survives, which is not the part that races.
            //
            // The flag decides how the effect travels. `writes_global` reaches a
            // caller unconditionally, while `mutates_shared` reaches one only
            // when a shared value is passed in as an argument. Marked the wrong
            // way, a task calling a *zero-argument* helper that wrote a global
            // collection was not rejected, though the same write spelled inline
            // was.
            e.writes_global = true;
            found.push((format!("assigns into the global `{name}`"), span));
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
                e.reads_globals.extend(callee_eff.reads_globals.iter().cloned());
                if callee_eff.writes_global {
                    e.writes_global = true;
                    found.push((
                        format!("calls `{}`, which writes a global", table.fns[uid].chunk.name),
                        span,
                    ));
                }
                // A method reaches its receiver as `self`, not as an argument,
                // so `b.grow()` on an `extend` method has an empty `args` and
                // the shared thing is `b`. Counting the receiver is what makes
                // a user method behave like the built-in `push` above.
                let recv_shared =
                    t.getfield.get(callee).is_some_and(|(recv, _)| t.regs.contains(recv));
                if callee_eff.mutates_shared
                    && (recv_shared || args.iter().any(|a| t.regs.contains(a)))
                {
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
            // A `std/bytes` constructor is the one call whose result is not an
            // alias of anything the caller holds: it returns storage it just
            // made. Same reasoning as the `MakeArray` arm above.
            if is_bytes_constructor(t, callee) {
                t.regs.remove(d);
                return;
            }
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

/// Whether `callee` holds `bytes.zeros`, `bytes.from_ints` or `bytes.concat`
/// read off the stdlib module rather than off a user value.
///
/// `bytes.zeros(4)` and `buf.zeros(4)` compile to the same `GetGlobal` +
/// `GetField` + `Call` triple, so the receiver's origin is what decides: it has
/// to have come from `GetGlobal("bytes")`, and the program must not bind a
/// global of that name itself. That is the same test `codegen::calls` uses to
/// tell a module call from a value method, so the two passes cannot disagree
/// about what `bytes.zeros` means.
fn is_bytes_constructor(t: &Taint, callee: &Reg) -> bool {
    let Some((recv, name)) = t.getfield.get(callee) else {
        return false;
    };
    BYTES_CONSTRUCTORS.contains(&name.as_str())
        && t.reg_global.get(recv).is_some_and(|g| g == "bytes")
        && !t.user_globals.contains("bytes")
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

    // Every global name the program binds itself, whether by assigning it or by
    // importing under it. A user binding shadows a stdlib module name, so
    // `bytes.zeros(n)` stops counting as a constructor call the moment the
    // program binds `bytes` of its own.
    //
    // `ImportFile` matters as much as `SetGlobal` here, and is easier to miss:
    // `use bytes` next to a user file named `bytes.jde` binds the global with no
    // assignment anywhere. A `concat` exported from that file returning one of
    // its arguments would then have been read as a fresh allocation, handing
    // back a live alias with its taint stripped. `jade run` accepted a program
    // that mutated the spawner's array from two tasks at once.
    let mut user_globals: HashSet<String> = HashSet::new();
    for chunk in std::iter::once(top).chain(table.fns.iter().map(|f| &f.chunk)) {
        for instr in &chunk.code {
            match instr {
                Instr::SetGlobal(n, _) => {
                    user_globals.insert(n.clone());
                }
                // A *user* file only. `use std::bytes` is an `ImportFile` too,
                // with the package's own import name as its path, and counting
                // that one would make the package permanently shadow itself.
                Instr::ImportFile(path, namespace)
                    if crate::builtins::find_package(path).is_none() =>
                {
                    user_globals.insert(namespace.clone());
                }
                _ => {}
            }
        }
    }

    // Fixed point over the call graph. Effects only ever turn on, so this
    // terminates; the bound guards against a bug in that reasoning rather than
    // against a real program.
    let mut eff = vec![Effects::default(); n];
    let mut sites: Vec<Vec<(String, Span)>> = vec![Vec::new(); n];
    for _ in 0..n.max(1) + 2 {
        let mut changed = false;
        for i in 0..n {
            let (new_eff, new_sites) = analyze(&table.fns[i], &table, &eff, &user_globals);
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

    // A function value bound to a global under a name of its own. `by_name` is
    // keyed on the *definition's* name, which a closure does not have: `let c =
    // || s.push(9)` binds a global called `c` to an anonymous body, so reading
    // `c` back resolved to nothing and the closure reached a task unexamined.
    let mut global_fn: HashMap<String, usize> = HashMap::new();

    for chunk in chunks {
        let mut reg_fn: HashMap<Reg, usize> = HashMap::new();
        // Tasks spawned here and not yet awaited, and where their futures are.
        // A future is written to a register and then to a local, so both have to
        // be followed or the await is never recognised and every later
        // assignment looks like a race.
        let mut live: Vec<usize> = Vec::new();
        let mut reg_task: HashMap<Reg, usize> = HashMap::new();
        let mut slot_task: HashMap<u32, usize> = HashMap::new();
        // Values handed to a task that is still running, and how to recognise
        // them again after they have been through a local.
        let mut shared_regs: HashSet<Reg> = HashSet::new();
        let mut shared_slots: HashSet<u32> = HashSet::new();
        // …and by name, because a global is re-read into a fresh register every
        // time it is used: `read(s)` and `s.push(3)` never share a register, so
        // following registers alone sees two unrelated values.
        let mut shared_globals: HashSet<String> = HashSet::new();
        let mut reg_global: HashMap<Reg, String> = HashMap::new();
        let mut getfield: HashMap<Reg, (Reg, String)> = HashMap::new();
        let mut shared_task: usize = 0;
        for (i, instr) in chunk.code.iter().enumerate() {
            let span = chunk.spans.get(i).copied().unwrap_or(Span { line: 0, col: 0 });
            // The spawner's own half of the rule. Everything else in this pass
            // asks what a task does; this asks what the spawner does *while the
            // task runs*, which is the same race seen from the other side:
            //
            //     let k = 2
            //     async fn read() { return k }
            //     let f = read()
            //     k = 10                        // ← here
            //     print(await f)
            //
            // The two engines do not even agree on the answer: the interpreter
            // gives each task a snapshot of the globals and says 2, a compiled
            // binary shares one cell and says 10.
            if let Instr::SetGlobal(name, r) = instr
                // A function definition or a decorator rebinding one is not what
                // this is about, and both are ordinary `SetGlobal`s.
                && !reg_fn.contains_key(r)
                && let Some(&uid) =
                    live.iter().find(|&&u| eff[u].reads_globals.contains(name))
            {
                return Err(Violation {
                    task: table.fns[uid].chunk.name.clone(),
                    what: format!(
                        "reads the global `{name}`, which is assigned here while the task \
                         is still running"
                    ),
                    span,
                });
            }
            // The other half of the spawner's side: mutating a collection a
            // running task is holding. `let f = read(s)` then `s.push(3)` before
            // the await is the same race as mutating it from inside the task,
            // which the pass has always refused — it just never looked here.
            if !live.is_empty() {
                let target = match instr {
                    Instr::SetIndex(o, _, _) | Instr::SetField(o, _, _) => Some((*o, None)),
                    Instr::Call(_, callee, _) => getfield
                        .get(callee)
                        .filter(|(_, name)| MUTATING_METHODS.contains(&name.as_str()))
                        .map(|(recv, name)| (*recv, Some(name.clone()))),
                    _ => None,
                };
                if let Some((o, method)) = target
                    && shared_regs.contains(&o)
                {
                    let what = match method {
                        Some(name) => format!(
                            "is handed a collection the spawner then calls `{name}()` on, \
                             here, while the task is still running"
                        ),
                        None => "is handed a collection the spawner then assigns into, here, \
                                 while the task is still running"
                            .to_string(),
                    };
                    return Err(Violation {
                        task: table.fns[shared_task].chunk.name.clone(),
                        what,
                        span,
                    });
                }
            }

            match instr {
                Instr::Await(_, r) => {
                    if let Some(u) = reg_task.get(r) {
                        live.retain(|x| x != u);
                    }
                    if live.is_empty() {
                        shared_regs.clear();
                        shared_slots.clear();
                        shared_globals.clear();
                    }
                }
                Instr::Join(_, regs) => {
                    for r in regs {
                        if let Some(u) = reg_task.get(r) {
                            live.retain(|x| x != u);
                        }
                    }
                    if live.is_empty() {
                        shared_regs.clear();
                        shared_slots.clear();
                        shared_globals.clear();
                    }
                }
                Instr::GetField(d, o, name) => {
                    getfield.insert(*d, (*o, name.clone()));
                    // Reaching into something a task holds yields something a
                    // task holds.
                    if shared_regs.contains(o) {
                        shared_regs.insert(*d);
                    }
                }
                Instr::GetIndex(d, o, _) => {
                    if shared_regs.contains(o) {
                        shared_regs.insert(*d);
                    }
                }
                Instr::SetLocal(slot, r) => {
                    match reg_task.get(r).copied() {
                        Some(u) => {
                            slot_task.insert(*slot, u);
                        }
                        None => {
                            slot_task.remove(slot);
                        }
                    }
                    if shared_regs.contains(r) {
                        shared_slots.insert(*slot);
                    }
                }
                Instr::GetLocal(d, slot) => {
                    match slot_task.get(slot).copied() {
                        Some(u) => {
                            reg_task.insert(*d, u);
                        }
                        None => {
                            reg_task.remove(d);
                        }
                    }
                    if shared_slots.contains(slot) {
                        shared_regs.insert(*d);
                    }
                }
                _ => {}
            }
            match instr {
                Instr::SetGlobal(name, r) => match reg_fn.get(r).copied() {
                    Some(u) => {
                        global_fn.insert(name.clone(), u);
                    }
                    None => {
                        global_fn.remove(name);
                    }
                },
                Instr::GetGlobal(d, name) => {
                    match global_fn.get(name).copied().or_else(|| table.by_name.get(name).copied())
                    {
                        Some(idx) => {
                            reg_fn.insert(*d, idx);
                        }
                        None => {
                            reg_fn.remove(d);
                        }
                    }
                    reg_global.insert(*d, name.clone());
                    if shared_globals.contains(name) {
                        shared_regs.insert(*d);
                    }
                }
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
                Instr::Spawn(dest, callee, args) => {
                    // Remember it as running, so the spawner-side check above
                    // knows what is at stake until the matching await.
                    if let Some(&u) = reg_fn.get(callee) {
                        reg_task.insert(*dest, u);
                        if !live.contains(&u) {
                            live.push(u);
                        }
                        shared_regs.extend(args.iter().copied());
                        for a in args {
                            if let Some(name) = reg_global.get(a) {
                                shared_globals.insert(name.clone());
                            }
                        }
                        shared_task = u;
                    }
                    // The task body itself, and any function handed to it.
                    //
                    // A callback is the other way a task mutates shared state,
                    // and it hides from the body's own analysis: `async fn
                    // run(f) { f() }` calls a parameter, which resolves to
                    // nothing, so `let c = || s.push(9)` reached the task
                    // completely unexamined. Here the closure is still a
                    // register with a known definition, so its effects are
                    // exactly as visible as the body's.
                    let mut candidates = vec![*callee];
                    candidates.extend(args.iter().copied());
                    let Some(uid) = candidates
                        .iter()
                        .filter_map(|r| reg_fn.get(r).copied())
                        .find(|&u| !eff[u].is_clean())
                    else {
                        continue;
                    };
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

    /// The named-field half of the same rule, and older than copy-with. Putting
    /// a shared collection into a struct field hands the task the collection
    /// itself, not a copy of it. `MakeStruct` used to count as an unconditionally
    /// fresh allocation, so this compiled clean and the push reached the
    /// spawner's array on both engines.
    #[test]
    /// A callback reaches a task as a value, so the task's own body says
    /// nothing about it: `async fn run(f) { f() }` calls a parameter, which
    /// resolves to no definition at all. The closure is still a register with a
    /// known body at the *spawn*, which is where this is caught.
    #[test]
    fn a_closure_that_mutates_shared_state_is_rejected_at_the_spawn() {
        let e = rejection(
            r#"
            async fn run(f) {
                f()
                return 0
            }
            let shared = [1]
            let grow = || shared.push(9)
            await run(grow)
            "#,
        );
        assert!(e.contains("shared"), "should name the sharing: {e}");
    }

    /// The closure above is bound to a global named for the *variable*, not for
    /// any definition, so resolving it by definition name finds nothing. What
    /// the global was last assigned is what answers it.
    #[test]
    fn a_function_value_read_back_from_a_global_still_resolves() {
        let e = rejection(
            r#"
            async fn run(f) {
                f()
                return 0
            }
            let shared = { "n": 1 }
            let bump = || shared.set("n", 2)
            let alias = bump
            await run(alias)
            "#,
        );
        assert!(e.contains("shared"), "should name the sharing: {e}");
    }

    /// A method reaches its receiver as `self` rather than as an argument, so a
    /// user `extend` method mutating the receiver has an empty argument list.
    /// Counting the receiver is what makes it behave like the built-in `push`.
    #[test]
    fn a_user_method_mutating_its_receiver_is_rejected() {
        let e = rejection(
            r#"
            struct Box { items }
            extend Box {
                fn grow(self) {
                    self.items.push(9)
                }
            }
            async fn go(b) {
                b.grow()
                return 0
            }
            let shared = [1]
            await go(Box { items: shared })
            "#,
        );
        assert!(e.contains("grow"), "should name the method: {e}");
    }

    /// The spawner's own half of the rule. Every other test here asks what a
    /// task does; these two ask what the spawner does *while the task runs*,
    /// which is the same race from the other side.
    #[test]
    fn assigning_a_global_a_running_task_reads_is_rejected() {
        let e = rejection(
            r#"
            let limit = 2
            async fn read() {
                return limit
            }
            let f = read()
            limit = 10
            print(await f)
            "#,
        );
        assert!(e.contains("limit"), "should name the global: {e}");
    }

    #[test]
    fn mutating_a_collection_a_running_task_holds_is_rejected() {
        let e = rejection(
            r#"
            async fn read(a) {
                return len(a)
            }
            let readings = [1, 2]
            let f = read(readings)
            readings.push(3)
            print(await f)
            "#,
        );
        assert!(e.contains("push"), "should name the mutation: {e}");
    }

    /// The window closes at the await, so the same two programs are fine once
    /// the task has finished. Without this the check would refuse every program
    /// that ever writes a global it also reads from a task.
    #[test]
    fn the_same_writes_after_the_await_are_allowed() {
        compile(
            r#"
            let limit = 2
            async fn read(a) {
                return len(a) + limit
            }
            let readings = [1, 2]
            print(await read(readings))
            limit = 10
            readings.push(3)
            print(readings)
            "#,
        )
        .expect("writes after the await are not a race");
    }

    fn task_mutating_a_collection_it_wrapped_in_a_struct_is_rejected() {
        let e = rejection(
            r#"
            struct Box { let items = [] }
            let shared = [1, 2]
            async fn worker() {
                let b = Box { items: shared }
                b.items.push(3)
            }
            await worker()
            "#,
        );
        assert!(e.contains("shared"), "should name the sharing: {e}");
    }

    /// A copy-with literal copies field *values*, so a collection in one of them
    /// is the very same collection. Without this, a task reached a shared array
    /// by copying the struct that held it — `Box { ...shared }` looked like a
    /// fresh allocation, taint stopped, and `copy.items.push(3)` compiled clean
    /// while mutating the spawner's array on both engines.
    #[test]
    fn task_mutating_a_collection_reached_through_a_copy_with_base_is_rejected() {
        let e = rejection(
            r#"
            struct Box { let items = [] }
            let shared = Box { items: [1, 2] }
            async fn worker() {
                let copy = Box { ...shared }
                copy.items.push(3)
            }
            await worker()
            "#,
        );
        assert!(e.contains("shared"), "should name the sharing: {e}");
    }

    /// The other half of the rule: a literal with no base really is a fresh
    /// object, so taint has to stop there or every struct built inside a task
    /// would be refused.
    #[test]
    fn task_mutating_a_collection_in_a_struct_it_built_is_allowed() {
        compile(
            r#"
            struct Box { let items = [] }
            let shared = Box { items: [1, 2] }
            async fn worker() {
                let fresh = Box { items: [9] }
                fresh.items.push(3)
                return len(fresh.items)
            }
            await worker()
            "#,
        )
        .expect("a struct the task built itself is not shared");
    }

    /// The hole that made `SetIndex` a no-op check. `arr[0] = 9` on a caller's
    /// array compiled clean while `arr.push(9)` beside it was rejected, because
    /// the emitter hands `SetIndex` the *slot* of the binding and the arm was
    /// reading the register set. A real data race on both engines.
    #[test]
    fn task_assigning_into_a_passed_collection_is_rejected() {
        let e = rejection(
            r#"
            async fn f(arr) { arr[0] = 9 }
            let a = [1, 2]
            await f(a)
            "#,
        );
        assert!(e.contains("shared"), "should name the sharing: {e}");
    }

    /// The other half of the same hole: `SetIndexGlobal` had no arm at all, so
    /// a task writing into a global collection was never rejected even though
    /// rebinding that same global was.
    #[test]
    fn task_assigning_into_a_global_collection_is_rejected() {
        let e = rejection(
            r#"
            let a = [1, 2]
            async fn g() { a[0] = 9 }
            await g()
            "#,
        );
        assert!(e.contains('a'), "should name the global: {e}");
    }

    /// The bytes twin of `task_mutating_its_own_array_is_allowed`. A buffer a
    /// task allocated itself is aliased by nothing, so writing octets into it
    /// races with nothing either. Without the constructor exemption this is
    /// rejected, which would leave `std::bytes` unusable inside a task.
    #[test]
    fn task_writing_into_a_buffer_it_allocated_is_allowed() {
        compile(
            r#"
            use std::bytes
            async fn build(n) {
                let b = bytes.zeros(4)
                b[0] = n
                return b
            }
            let r = await build(7)
            "#,
        )
        .expect("a task may write into a buffer it allocated itself");
    }

    /// `concat` takes two shared inputs and returns storage neither points at,
    /// so its result is writable even when both arguments came from outside.
    #[test]
    fn task_writing_into_a_concat_result_is_allowed() {
        compile(
            r#"
            use std::bytes
            async fn join(a, b) {
                let out = bytes.concat(a, b)
                out[0] = 1
                return out
            }
            let r = await join("a".encode(), "b".encode())
            "#,
        )
        .expect("concat returns a fresh blob");
    }

    /// A caller's blob is still the caller's. The exemption is for what the
    /// constructor returns, not for every blob in sight.
    #[test]
    fn task_writing_into_a_passed_buffer_is_rejected() {
        let e = rejection(
            r#"
            use std::bytes
            async fn f(b) { b[0] = 1 }
            let buf = bytes.zeros(2)
            await f(buf)
            "#,
        );
        assert!(e.contains("shared"), "a passed buffer is not the task's: {e}");
    }

    /// The flag a violation sets decides how far it travels. `writes_global`
    /// reaches a caller unconditionally; `mutates_shared` reaches one only when
    /// a shared value is passed as an argument. So a `SetIndexGlobal` marked as
    /// shared mutation was invisible through a zero-argument helper, though the
    /// same write spelled inline was caught.
    #[test]
    fn writing_a_global_collection_through_a_helper_is_rejected() {
        let e = rejection(
            r#"
            let d = {}
            fn helper() { d["k"] = 1 }
            async fn worker() { helper() }
            await worker()
            "#,
        );
        assert!(e.contains("helper"), "should name the helper it travelled through: {e}");
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
