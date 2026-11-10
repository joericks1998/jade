//! Tests for the MIR task check, through the real pipeline: each program is
//! parsed, checked and lowered to MIR as `jade build` does.

use crate::compiler::type_infer;
use crate::frontend::{lexer, parser};

/// Purpose: This function compiles `src` to MIR and runs the task check.
///
/// Outputs: the error text when the program is refused, by the check or
/// anything before it.
fn compile(src: &str) -> Result<(), String> {
    let tokens = lexer::tokenize(src).map_err(|e| e.to_string())?;
    let program = parser::parse(tokens).map_err(|e| e.to_string())?;
    let mut tp = type_infer::infer(program).map_err(|e| e.to_string())?;
    crate::compiler::infer::check_members(&tp).map_err(|e| e.to_string())?;
    type_infer::fill_struct_literal_defaults(&mut tp).map_err(|e| e.to_string())?;
    let typed = crate::compiler::infer::check_program(&tp).map_err(|e| e.to_string())?;
    let mir = crate::mir::lower_program(&tp, &typed).map_err(|e| e.to_string())?;
    super::check(&mir).map_err(|e| e.to_string())
}

/// Purpose: This function compiles `src` and returns why it was refused.
fn rejection(src: &str) -> String {
    match compile(src) {
        Err(e) => e,
        Ok(()) => panic!("expected rejection, but the program compiled:\n{src}"),
    }
}

/// A callback reaches a task as a value, so the task's own body says
/// nothing about it: `async fn run(f) { f() }` calls a parameter, which
/// resolves to no definition at all. The function is still a register with a
/// known body at the *spawn*, which is where this is caught.
#[test]
fn a_function_value_that_mutates_shared_state_is_rejected_at_the_spawn() {
    let e = rejection(
        r#"
        async fn run(f) {
            f()
            return 0
        }
        let shared = [1]
        fn grow() { shared.push(9) }
        await run(grow)
        "#,
    );
    assert!(e.contains("shared"), "should name the sharing: {e}");
}

/// A function value copied into another global is bound to a name that is
/// not any definition's, so resolving it by definition name finds nothing.
/// What the global was last assigned is what answers it.
#[test]
fn a_function_value_read_back_from_a_global_still_resolves() {
    let e = rejection(
        r#"
        async fn run(f) {
            f()
            return 0
        }
        let shared = { "n": 1 }
        fn bump() { shared["n"] = 2 }
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
        class Box {
@public items

            @public fn grow() {
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

/// The named-field half of the same rule, and older than copy-with. Putting
/// a shared collection into a struct field hands the task the collection
/// itself, not a copy of it. `MakeStruct` used to count as an unconditionally
/// fresh allocation, so this compiled clean and the push reached the
/// spawner's array on both engines.
#[test]
fn task_mutating_a_collection_it_wrapped_in_a_struct_is_rejected() {
    let e = rejection(
        r#"
        class Box {
@public let items = []
}
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
        class Box {
@public let items = []
}
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
        class Box {
@public let items = []
}
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
        async fn glue(a, b) {
            let out = bytes.concat(a, b)
            out[0] = 1
            return out
        }
        let r = await glue("a".encode(), "b".encode())
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
        class Counter {
@public n
}
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
        class P {
@public x
}
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

/// A global collection changed through a helper the task calls with no
/// arguments. v1's bytecode pass counted a global's collection as shared only
/// through an argument, so a zero-argument helper hid the write.
#[test]
fn a_helper_pushing_to_a_global_collection_is_rejected() {
    let e = rejection(
        r#"
        let shared = [1]
        fn grow() { shared.push(9) }
        async fn worker() { grow() }
        await worker()
        "#,
    );
    assert!(e.contains("grow"), "should name the helper: {e}");
}

/// A scalar parameter cannot be shared, so building with it is fine.
#[test]
fn a_task_storing_a_scalar_parameter_is_allowed() {
    compile(
        r#"
        async fn wrap(n) {
            let out = [n]
            out.push(n + 1)
            return out
        }
        print(await wrap(1))
        "#,
    )
    .expect("an int parameter is not shared");
}
