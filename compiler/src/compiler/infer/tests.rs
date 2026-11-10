//! Tests for whole-program inference: the rules in design/v2-static-values.md,
//! each pinned by a program that should check and one that should not.

use super::*;

/// Purpose: This function runs a source string through the frontend and the checker.
///
/// Inputs:
/// - `src`: a Jade program.
///
/// Outputs: the checker's answer.
fn check(src: &str) -> Result<Typed, JadeError> {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lexes");
    let program = crate::frontend::parser::parse(tokens).expect("parses");
    let mut tp = crate::compiler::type_infer::infer(program).expect("v1 checker accepts it");
    check_members(&tp)?;
    crate::compiler::type_infer::fill_struct_literal_defaults(&mut tp).expect("defaults fill");
    check_program(&tp)
}

/// Purpose: This function asserts a program checks.
///
/// Inputs:
/// - `src`: a Jade program.
///
/// Outputs: the checker's answer.
fn ok(src: &str) -> Typed {
    match check(src) {
        Ok(t) => t,
        Err(e) => panic!("expected the program to check, got: {e}\n{src}"),
    }
}

/// Purpose: This function asserts a program is rejected with a message
/// containing `want`.
///
/// Inputs:
/// - `src`: a Jade program.
/// - `want`: part of the expected message.
///
/// Outputs: none.
fn rejects(src: &str, want: &str) {
    match check(src) {
        Ok(_) => panic!("expected a type error containing '{want}', but it checked:\n{src}"),
        Err(e) => {
            let m = e.to_string();
            assert!(m.contains(want), "expected '{want}' in the error, got: {m}");
        }
    }
}

/// Purpose: This function finds a global's resolved type.
///
/// Inputs:
/// - `t`: the checker's answer.
/// - `name`: the global.
///
/// Outputs: its type.
fn global(t: &Typed, name: &str) -> Ty {
    t.globals.get(name).cloned().unwrap_or_else(|| panic!("no global {name}"))
}

#[test]
fn literals_and_arithmetic() {
    let t = ok("let a = 1 + 2\nlet b = 1.5 * 2\nlet c = \"x\" + \"y\"\nlet d = 1 < 2\n");
    assert_eq!(global(&t, "a"), Ty::Int);
    assert_eq!(global(&t, "b"), Ty::Float);
    assert_eq!(global(&t, "c"), Ty::Str);
    assert_eq!(global(&t, "d"), Ty::Bool);
}

#[test]
fn a_function_is_checked_once_per_argument_types() {
    let t = ok(
        "fn add(a, b) { return a + b }\nlet i = add(1, 2)\nlet f = add(1.5, 2.0)\nlet s = add(\"a\", \"b\")\n",
    );
    assert_eq!(global(&t, "i"), Ty::Int);
    assert_eq!(global(&t, "f"), Ty::Float);
    assert_eq!(global(&t, "s"), Ty::Str);
    let adds = t.instances.iter().filter(|(k, _, _)| k.func == FnKey::Named("add".into())).count();
    assert_eq!(adds, 3);
}

#[test]
fn a_bad_call_is_reported_at_its_instance() {
    rejects(
        "fn add(a, b) { return a + b }\nlet x = add(1, \"b\")\n",
        "'+' cannot combine int and str",
    );
}

#[test]
fn an_empty_array_learns_its_type_from_a_push() {
    let t = ok("let xs = []\nxs.push(1)\n");
    assert_eq!(global(&t, "xs"), Ty::Array(Box::new(Ty::Int)));
}

#[test]
fn a_push_through_a_function_reaches_the_callers_array() {
    let t = ok("fn fill(a) { a.push(\"x\") }\nlet xs = []\nfill(xs)\n");
    assert_eq!(global(&t, "xs"), Ty::Array(Box::new(Ty::Str)));
}

#[test]
fn an_array_nothing_fills_holds_nil() {
    let t = ok("let xs = []\nprint(xs)\n");
    assert_eq!(global(&t, "xs"), Ty::Array(Box::new(Ty::Nil)));
}

#[test]
fn a_variable_keeps_one_type() {
    rejects("let v = \"text\"\nv = 7\n", "do not mix");
}

#[test]
fn nil_then_a_value_is_optional() {
    let t = ok("let first = nil\nfirst = 5\n");
    assert_eq!(global(&t, "first"), Ty::Opt(Box::new(Ty::Int)));
}

#[test]
fn an_optional_must_be_checked_before_use() {
    rejects("use std::env\nlet p = env.get(\"PORT\")\nprint(p.len())\n", "may be nil");
}

#[test]
fn checking_against_nil_narrows() {
    ok("use std::env\nlet p = env.get(\"PORT\")\nif p != nil {\n    print(p.len())\n}\n");
    ok("use std::env\nlet p = env.get(\"PORT\")\nif p != nil && p.len() > 0 {\n    print(p)\n}\n");
}

#[test]
fn falling_off_the_end_returns_nil() {
    let t = ok("fn maybe(x) {\n    if x > 0 {\n        return x\n    }\n}\nlet m = maybe(3)\n");
    assert_eq!(global(&t, "m"), Ty::Opt(Box::new(Ty::Int)));
}

#[test]
fn returning_two_kinds_is_an_error() {
    rejects(
        "fn pick(x) {\n    if x {\n        return \"s\"\n    }\n    return 1\n}\nprint(pick(true))\n",
        "do not mix",
    );
}

#[test]
fn a_dict_holds_one_kind_of_value() {
    let t = ok("let ages = {\"ana\": 31}\nages[\"bo\"] = 28\nlet a = ages.get(\"x\")\n");
    assert_eq!(global(&t, "ages"), Ty::Map(Box::new(Ty::Int)));
    assert_eq!(global(&t, "a"), Ty::Opt(Box::new(Ty::Int)));
    rejects("let d = {\"name\": \"jade\", \"version\": 1}\n", "use a class");
}

#[test]
fn a_dict_has_no_fields() {
    rejects("let d = {\"code\": 0}\nprint(d.code)\n", "a dict has no fields");
}

#[test]
fn mixed_arrays_are_an_error() {
    rejects("let m = [1, \"a\"]\n", "do not mix");
}

#[test]
fn structs_join_to_their_common_parent() {
    let src = "class Frame {}\nclass Token(Frame) {\n    @public text\n}\nclass Done(Frame) {\n    @public reason\n}\n\
               let frames = [Token { text: \"hi\" }, Done { reason: \"stop\" }]\n";
    let t = ok(src);
    assert_eq!(global(&t, "frames"), Ty::Array(Box::new(Ty::Struct("Frame".into()))));
    rejects(
        "class A {\n    @public x\n}\nclass B {\n    @public y\n}\nlet m = [A { x: 1 }, B { y: 2 }]\n",
        "shared parent",
    );
}

#[test]
fn a_method_dispatches_on_the_actual_struct() {
    let src = "class Animal {\n    @public name\n}\n\
               class Dog(Animal) {\n    @public fn speak() { return \"woof\" }\n}\n\
               class Cat(Animal) {\n    @public fn speak() { return \"meow\" }\n}\n\
               let pets = [Dog { name: \"rex\" }, Cat { name: \"tom\" }]\n\
               for p in pets {\n    print(p.speak())\n}\n";
    ok(src);
}

#[test]
fn a_struct_field_is_typed_by_every_literal() {
    rejects("class P {\n    @public let x = 0\n}\nlet p = P { x: \"s\" }\n", "do not mix");
}

#[test]
fn recursion_settles() {
    let t = ok(
        "fn fact(n) {\n    if n <= 1 {\n        return 1\n    }\n    return n * fact(n - 1)\n}\nlet f = fact(5)\n",
    );
    assert_eq!(global(&t, "f"), Ty::Int);
}

#[test]
fn recursion_that_changes_its_own_types_is_refused() {
    rejects("fn f(x) {\n    return f([x])\n}\nprint(f(1))\n", "different type each time");
}

#[test]
fn a_function_passed_as_a_value_takes_its_types_from_the_call() {
    let t =
        ok("fn apply(f, x) { return f(x) }\nfn square(x) { x * x }\nlet sq = apply(square, 6)\n");
    assert_eq!(global(&t, "sq"), Ty::Int);
}

#[test]
fn map_and_filter_are_typed() {
    let t = ok(
        "fn double(x) { x * 2 }\nfn big(x) { x > 1 }\nlet xs = [1, 2, 3]\nlet ys = xs.map(double)\nlet zs = xs.filter(big)\nlet ss = xs.map(str)\n",
    );
    assert_eq!(global(&t, "ys"), Ty::Array(Box::new(Ty::Int)));
    assert_eq!(global(&t, "zs"), Ty::Array(Box::new(Ty::Int)));
    assert_eq!(global(&t, "ss"), Ty::Array(Box::new(Ty::Str)));
}

#[test]
fn different_functions_in_one_array_share_a_function_type() {
    let t = ok("fn double(x) { return x * 2 }\nfn square(x) { return x * x }\n\
                let fs = [double, square]\nfor g in fs {\n    print(g(3))\n}\n");
    match global(&t, "fs") {
        Ty::Array(inner) => assert!(matches!(*inner, Ty::Fn(_)), "got {inner}"),
        other => panic!("expected an array, got {other}"),
    }
}

#[test]
fn raise_takes_a_struct_or_a_string() {
    ok(
        "class Oops {\n    @public code\n}\ntry {\n    raise Oops { code: 1 }\n} catch Oops e {\n    print(e.code)\n} catch e {\n    print(e.message)\n}\n",
    );
    rejects("raise 5\n", "cannot raise int");
}

#[test]
fn a_catch_all_only_has_a_message() {
    rejects("try {\n    raise \"x\"\n} catch e {\n    print(e.code)\n}\n", "only has 'message'");
}

#[test]
fn async_functions_give_futures() {
    let t = ok(
        "async fn work(x) { return x + 1 }\nlet f = work(1)\nlet r = await f\nlet all = join(work(1), work(2))\n",
    );
    assert_eq!(global(&t, "f"), Ty::Future(Box::new(Ty::Int)));
    assert_eq!(global(&t, "r"), Ty::Int);
    assert_eq!(global(&t, "all"), Ty::Array(Box::new(Ty::Int)));
}

#[test]
fn generators_give_streams() {
    let t = ok("fn nums() {\n    yield 1\n    yield 2\n}\nlet s = nums()\n");
    assert_eq!(global(&t, "s"), Ty::Stream(Box::new(Ty::Int)));
    rejects("fn mixed() {\n    yield 1\n    yield \"two\"\n}\nprint(mixed())\n", "do not mix");
}

#[test]
fn std_results_are_structs_that_read_both_ways() {
    ok("use std::sh\nlet r = sh.output(\"true\")\nprint(r.code)\nprint(r[\"stdout\"])\n");
}

#[test]
fn json_is_its_own_type() {
    let t =
        ok("use std::json\nlet v = json.parse(\"{}\")\nlet a = v[\"Action\"]\nlet s = str(a)\n");
    assert_eq!(global(&t, "v"), Ty::Json);
    assert_eq!(global(&t, "s"), Ty::Str);
}

#[test]
fn the_v1_soundness_holes_are_closed() {
    // An assignment in one branch.
    rejects("let x = \"s\"\nif true {\n    x = 1\n}\nprint(x)\n", "do not mix");
    // A push of another kind.
    rejects("let a = [1]\na.push(\"s\")\n", "do not mix");
}

#[test]
fn an_array_cannot_contain_itself() {
    rejects("let a = []\na.push(a)\n", "cannot contain itself");
}

#[test]
fn an_early_exit_on_nil_narrows_the_rest_of_the_block() {
    ok(
        "use std::env\nfn port() {\n    let p = env.get(\"PORT\")\n    if p == nil {\n        return \"none\"\n    }\n    return p\n}\nprint(port())\n",
    );
}

#[test]
fn a_trailing_expression_is_the_return_value() {
    let t = ok("fn double(x) {\n    x * 2\n}\nlet d = double(5) + 1\n");
    assert_eq!(global(&t, "d"), Ty::Int);
}

#[test]
fn a_method_used_without_a_call_is_refused() {
    rejects(
        "class C {\n    @public n\n\n    @public fn get() { return self.n }\n}\nlet c = C { n: 1 }\nlet g = c.get\n",
        "a method is called, not passed",
    );
    rejects("let s = \"a\"\nlet u = s.upper\n", "a method is called, not passed");
}

// ── classes: visibility, constructors, outside definitions ───────────────────

#[test]
fn a_private_field_is_refused_outside_its_class() {
    rejects(
        "class A {\n    @public n\n    let secret = 1\n}\nlet a = A { n: 1 }\nprint(a.secret)\n",
        "field 'secret' is private to A",
    );
    rejects(
        "class A {\n    @public n\n    let secret = 1\n}\nlet a = A { n: 1 }\na.secret = 2\n",
        "field 'secret' is private to A",
    );
}

#[test]
fn a_private_method_is_refused_outside_its_class() {
    rejects(
        "class A {\n    @public n\n    fn hidden() { return 1 }\n}\nlet a = A { n: 1 }\nprint(a.hidden())\n",
        "method 'hidden' is private to A",
    );
}

#[test]
fn a_child_sees_its_parents_private_members() {
    ok("class A {\n    let secret = 1\n    fn hidden() { return self.secret }\n}\n\
        class B(A) {\n    @public fn peek() { return self.secret + self.hidden() }\n}\n\
        let b = B {}\nprint(b.peek())\n");
}

#[test]
fn a_literal_outside_the_class_sets_only_public_fields() {
    rejects(
        "class A {\n    @public n\n    let secret = 1\n}\nlet a = A { n: 1, secret: 2 }\n",
        "field 'secret' is private to A, so a literal outside the class cannot set it",
    );
    rejects(
        "class A {\n    @public n\n    secret\n}\nlet a = A { n: 1 }\n",
        "A's private field 'secret' has no default",
    );
}

#[test]
fn a_constructor_builds_private_state_from_inside() {
    let t = ok(
        "class A {\n    @public n\n    secret\n    @public fn new(n)\n    @public fn read() { return self.secret }\n}\n\
                fn A::new(n) { return A { n: n, secret: n * 2 } }\n\
                let a = A::new(3)\nlet r = a.read()\n",
    );
    assert_eq!(global(&t, "r"), Ty::Int);
}

#[test]
fn an_outside_definition_needs_a_declaration() {
    rejects("class A {\n    @public n\n}\nfn A::area() { return self.n }\n", "does not declare");
    ok(
        "class A {\n    @public n\n    @public fn area()\n}\nfn A::area() { return self.n }\nprint(A { n: 2 }.area())\n",
    );
}

#[test]
fn a_declared_method_is_refused_only_where_it_is_called() {
    ok("class A {\n    @public n\n    @public fn area()\n}\nprint(A { n: 2 }.n)\n");
    rejects(
        "class A {\n    @public n\n    @public fn area()\n}\nprint(A { n: 2 }.area())\n",
        "declares `fn area` but nothing defines it",
    );
}

#[test]
fn a_method_defined_twice_is_refused() {
    rejects(
        "class A {\n    @public n\n    @public fn area() { return 1 }\n}\nfn A::area() { return 2 }\n",
        "does not declare",
    );
    rejects(
        "class A {\n    @public n\n    @public fn area()\n}\nfn A::area() { return 1 }\nfn A::area() { return 2 }\n",
        "defined more than once",
    );
}

/// Purpose: This function asserts a program is refused by any stage before
/// code generation, with a message containing `want`.
///
/// Inputs:
/// - `src`: a Jade program.
/// - `want`: part of the expected message.
///
/// Outputs: none.
fn refused(src: &str, want: &str) {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lexes");
    let program = match crate::frontend::parser::parse(tokens) {
        Ok(p) => p,
        Err(e) => {
            let m = e.to_string();
            assert!(m.contains(want), "expected '{want}', got: {m}");
            return;
        }
    };
    let result = crate::compiler::type_infer::infer(program).and_then(|mut tp| {
        check_members(&tp)?;
        crate::compiler::type_infer::fill_struct_literal_defaults(&mut tp)?;
        check_program(&tp).map(|_| ())
    });
    match result {
        Ok(()) => panic!("expected an error containing '{want}', but it compiled:\n{src}"),
        Err(e) => {
            let m = e.to_string();
            assert!(m.contains(want), "expected '{want}', got: {m}");
        }
    }
}

const ANIMALS: &str = "class Animal {\n    @public name\n    @public fn speak() { return \"...\" }\n}\n\
                       class Dog(Animal) {\n    @public breed\n    @public fn speak() { return Animal::speak() + \"woof\" }\n}\n\
                       class Cat(Animal) {\n    @public fn speak() { return \"meow\" }\n}\n";

#[test]
fn a_class_decorator_declares_a_parent_class() {
    let t = ok(&format!("{ANIMALS}@Animal\nlet pet = Dog {{ name: \"rex\", breed: \"x\" }}\n"));
    assert_eq!(global(&t, "pet"), Ty::Struct("Animal".into()));
    refused(
        &format!("{ANIMALS}@Cat\nlet pet = Dog {{ name: \"rex\", breed: \"x\" }}\n"),
        "views a value as a Cat",
    );
}

#[test]
fn a_type_decorator_types_a_field() {
    let t = ok("class K {\n    @public @int capacity\n}\nlet c = K { capacity: 2 }.capacity\n");
    assert_eq!(global(&t, "c"), Ty::Int);
    refused(
        "class K {\n    @public @int capacity\n}\nlet c = K { capacity: \"two\" }\n",
        "do not mix",
    );
    refused(
        "class K {\n    @public @cached capacity\n}\nlet c = K { capacity: 2 }\n",
        "is not @public or a type",
    );
    refused("class K {\n    @int fn size() { return 1 }\n}\n", "a method takes only @public");
}

#[test]
fn a_diamond_inherits_its_shared_ancestor_once() {
    let t = ok(
        "class N {\n    @public name\n}\nclass P(N) {\n    @public owner\n}\nclass W(N) {\n    @public job\n}\n\
                class S(P, W) { }\nlet d = S { name: \"rex\", owner: \"a\", job: \"g\" }\nlet n = d.name\n",
    );
    assert_eq!(global(&t, "n"), Ty::Str);
}

#[test]
fn two_parents_methods_need_the_class_to_settle_them() {
    let parents = "class P {\n    @public fn kind() { return \"p\" }\n}\nclass W {\n    @public fn kind() { return \"w\" }\n}\n";
    refused(
        &format!("{parents}class S(P, W) {{ }}\nprint(S {{}}.kind())\n"),
        "define `kind` in the class itself",
    );
    ok(&format!(
        "{parents}class S(P, W) {{\n    @public fn kind() {{ return P::kind() + W::kind() }}\n}}\nprint(S {{}}.kind())\n"
    ));
}

#[test]
fn a_parent_call_runs_the_parents_version() {
    let t = ok(&format!("{ANIMALS}let s = Dog {{ name: \"rex\", breed: \"x\" }}.speak()\n"));
    assert_eq!(global(&t, "s"), Ty::Str);
    refused(&format!("{ANIMALS}print(Animal::speak())\n"), "works only inside a method");
}

#[test]
fn is_narrows_to_the_class_it_tests() {
    ok(&format!(
        "{ANIMALS}let xs = [Dog {{ name: \"r\", breed: \"x\" }}, Cat {{ name: \"t\" }}]\n\
         for x in xs {{\n    if x is Dog {{\n        print(x.breed)\n    }}\n}}\n"
    ));
    refused(
        &format!("{ANIMALS}let d = Dog {{ name: \"r\", breed: \"x\" }}\nprint(d is Cat)\n"),
        "can never be a Cat",
    );
}

#[test]
fn a_destructor_chain_is_recorded_for_each_class_and_never_called() {
    let src = "class R {\n    @public name\n    fn drop() { print(self.name) }\n}\nclass L(R) {\n    fn drop() { print(\"l\") }\n}\n\
               let a = R { name: \"a\" }\nlet b = L { name: \"b\" }\n";
    let t = ok(src);
    let chain: Vec<String> = t.drops["L"].iter().map(|k| format!("{:?}", k.func)).collect();
    assert_eq!(
        chain,
        vec!["Method(\"L\", \"drop\")".to_string(), "Method(\"R\", \"drop\")".to_string()]
    );
    refused(
        "class R {\n    @public name\n    fn drop() { print(1) }\n}\nR { name: \"a\" }.drop()\n",
        "cannot be called",
    );
    refused(
        "class R {\n    @public name\n    fn drop(why) { print(why) }\n}\n",
        "takes no arguments",
    );
}

#[test]
fn a_comparison_with_a_string_waits_for_the_call_that_settles_its_side() {
    // `first` is checked after the top level reads `c == "a"`, and returns a
    // char, which compares with a string.
    ok(
        "fn first(text) {\n    for c in text {\n        return c\n    }\n    raise \"empty\"\n}\nlet c = first(\"x\")\nprint(c == \"a\")\n",
    );
    // A side nothing else settles still becomes the other side's type.
    ok("let xs = []\nprint(len(xs) == 0)\nfn g(v) {\n    return v == \"q\"\n}\nprint(g(\"q\"))\n");
}
