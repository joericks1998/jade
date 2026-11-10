//! Tests for the static backend: programs built to binaries and run.
//!
//! These link with the system `cc`, as `jade build` does. Each test builds in
//! a directory of its own, named for the process and a counter, so the tests
//! can run in parallel.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Purpose: This function lowers a source program to MIR.
fn mir_of(src: &str) -> mir::Program {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let mut tir = crate::typechecker::type_infer::infer(program).expect("infer");
    crate::typechecker::type_infer::fill_struct_literal_defaults(&mut tir).expect("defaults");
    let typed = crate::typechecker::infer::check_program(&tir).expect("check");
    mir::lower_program(&tir, &typed).expect("lowers")
}

/// Purpose: This function builds a program and runs it.
///
/// Inputs:
/// - `src`: the program.
///
/// Outputs: its standard output, standard error and exit status.
fn run(src: &str) -> (String, String, i32) {
    let dir = std::env::temp_dir().join(format!(
        "jade-static-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let bin = dir.join("prog");
    compile(&mir_of(src), &bin, Emit::Binary).expect("builds");
    let out = std::process::Command::new(&bin).output().expect("runs");
    let _ = std::fs::remove_dir_all(&dir);
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn numbers_print_as_v1_printed_them() {
    let (out, _, rc) = run(
        "print(1 + 2)\nprint(48000.0)\nprint(0.1 + 0.2)\nprint(-0.0)\nprint(7 / 2)\nprint(-7 % 3)\n",
    );
    assert_eq!(rc, 0);
    assert_eq!(out, "3\n48000.0\n0.30000000000000004\n-0.0\n3\n-1\n");
}

#[test]
fn integer_overflow_stops_the_program_with_v1s_message() {
    let (out, err, rc) = run("let x = 4611686018427387903\nprint(x * 4)\nprint(\"unreached\")\n");
    assert_eq!(out, "");
    assert_eq!(err, "jade: integer overflow\n");
    assert_eq!(rc, 1);
}

#[test]
fn a_dict_passed_to_a_function_is_a_copy() {
    let (out, _, _) = run("fn w(d) { d[\"x\"] = 99 }\nlet d = {\"x\": 1}\nw(d)\nprint(d[\"x\"])\n");
    assert_eq!(out, "1\n");
}

#[test]
fn an_array_passed_to_a_function_is_shared() {
    let (out, _, _) = run("fn p(a) { a.push(2) }\nlet a = [1]\np(a)\nprint(a)\n");
    assert_eq!(out, "[1, 2]\n");
}

#[test]
fn strings_count_and_map_characters_not_bytes() {
    let (out, _, _) = run("print(len(\"café\"))\nprint(\"größe\".upper())\nprint(\"café\"[3])\n");
    assert_eq!(out, "4\nGRÖSSE\né\n");
}

#[test]
fn dicts_print_sorted_with_quoted_keys() {
    let (out, _, _) = run("let d = {\"b\": [1], \"a\": [2, 3]}\nprint(d)\n");
    assert_eq!(out, "{\"a\": [2, 3], \"b\": [1]}\n");
}

#[test]
fn a_child_struct_runs_its_own_method_through_a_parent() {
    let src = "class A {\n    @public n\n    @public fn who() { return \"A\" }\n}\n\
               class B(A) {\n    @public m\n    @public fn who() { return \"B\" }\n}\n\
               let xs = [A { n: 1 }, B { n: 2, m: 3 }]\n\
               for x in xs { print(x.who()) }\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "A\nB\n");
}

/// The point of the static backend: a compiled program calls nothing but the
/// C library. Every external symbol the IR declares must be one of these.
#[test]
fn a_program_declares_nothing_but_c_library_functions() {
    let src = "use std::time\nasync fn nap(n) {\n    time.sleep(0.01)\n    return n\n}\n\
               let d = {\"k\": \"v\"}\nlet a = [1.5, 2.5]\na.sort()\nprint(f\"{d} {a} {\"x\".upper()}\")\n\
               print(int(\"3\") + len(\"abc\".split(\"\")))\nprint(join(nap(1), nap(2)))\nprint(time.monotonic() > 0.0)\n";
    let ctx = Context::create();
    let module = build_module(&ctx, &mir_of(src)).expect("builds");
    let allowed = [
        "malloc",
        "realloc",
        "free",
        "memcmp",
        "write",
        "read",
        "exit",
        "isatty",
        "snprintf",
        "strtod",
        "qsort",
        "pthread_attr_init",
        "pthread_attr_destroy",
        "pthread_attr_setstacksize",
        "pthread_create",
        "pthread_join",
        "pthread_detach",
        "pthread_mutex_init",
        "pthread_mutex_lock",
        "pthread_mutex_unlock",
        "pthread_cond_init",
        "pthread_cond_wait",
        "pthread_cond_broadcast",
        "nanosleep",
        "clock_gettime",
        "tan",
        "asin",
        "acos",
        "atan",
        "atan2",
        "hypot",
        "fopen",
        "fread",
        "fwrite",
        "fclose",
        "ferror",
        "stat",
        "stat$INODE64",
        "unlink",
        "rmdir",
        "mkdir",
        "opendir",
        "opendir$INODE64",
        "readdir",
        "readdir$INODE64",
        "closedir",
        "rename",
        "strlen",
        "strerror",
        "__error",
        "__errno_location",
        "pipe",
        "fcntl",
        "close",
        "poll",
        "waitpid",
        "posix_spawn",
        "posix_spawn_file_actions_init",
        "posix_spawn_file_actions_destroy",
        "posix_spawn_file_actions_adddup2",
        "getenv",
        "setenv",
        "getcwd",
        "getpid",
        "posix_spawnp",
        "posix_spawn_file_actions_addopen",
        "signal",
        "kill",
        "strdup",
        "unsetenv",
        "tzset",
        "localtime_r",
        "strftime",
        "dlopen",
        "dlsym",
        "dlerror",
        "dladdr",
        "realpath",
        "strcmp",
    ];
    let mut f = module.get_first_function();
    while let Some(func) = f {
        let name = func.get_name().to_string_lossy().into_owned();
        if func.count_basic_blocks() == 0 && !name.starts_with("llvm.") {
            assert!(allowed.contains(&name.as_str()), "the program declares {name}");
        }
        f = func.get_next_function();
    }
}

#[test]
fn a_field_read_through_a_second_parent_finds_the_childs_own() {
    let src = "class W {\n    @public span\n}\nclass F {\n    @public fins\n}\nclass D(W, F) {\n    @public name\n}\n\
               fn grow(f) {\n    f.fins = f.fins + 1\n    return f.fins\n}\n\
               let d = D { span: 2, fins: 5, name: \"dot\" }\nlet fs = [F { fins: 1 }, F(d)]\n\
               for f in fs {\n    print(grow(f))\n}\nprint(d.fins)\nprint(d.span)\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "2\n6\n6\n2\n");
}

#[test]
fn destructors_run_child_first_and_globals_newest_first() {
    let src = "class R {\n    @public name\n    fn drop() { print(\"R \" + self.name) }\n}\n\
               class L(R) {\n    fn drop() { print(\"L \" + self.name) }\n}\n\
               fn f() {\n    let x = R { name: \"local\" }\n}\nf()\n\
               let a = R { name: \"a\" }\nlet b = L { name: \"b\" }\nprint(\"end\")\n";
    let (out, _, rc) = run(src);
    assert_eq!(rc, 0);
    assert_eq!(out, "R local\nend\nL b\nR b\nR a\n");
}

#[test]
fn is_tests_the_class_and_its_descendants() {
    let src = "class A {\n    @public n\n}\nclass B(A) { }\nclass C(B) { }\nclass D(A) { }\n\
               let xs = [A { n: 1 }, B { n: 2 }, C { n: 3 }, D { n: 4 }]\nfor x in xs {\n    print(x is B)\n}\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "false\ntrue\ntrue\nfalse\n");
}

#[test]
fn a_runtime_error_is_caught_and_the_program_carries_on() {
    let src = "fn f(n) {\n    let xs = [1, 2]\n    return xs[n]\n}\n\
               try {\n    print(f(5))\n} catch e {\n    print(e.message)\n}\nprint(f(1))\n";
    let (out, _, rc) = run(src);
    assert_eq!((out.as_str(), rc), ("index out of bounds\n2\n", 0));
}

#[test]
fn an_uncaught_class_reports_its_message() {
    let (out, err, rc) = run(
        "class Halt {\n    @public message\n}\nprint(1)\nraise Halt { message: \"stop\" }\nprint(2)\n",
    );
    assert_eq!((out.as_str(), err.as_str(), rc), ("1\n", "jade: stop\n", 1));
}

#[test]
fn map_and_filter_pass_extra_arguments() {
    let src = "fn scale(x, by) { x * by }\nfn above(x, floor) { x > floor }\n\
               print([1, 2, 3].map(scale, 10))\nprint([1, 5, 9].filter(above, 4))\nprint([1, 22].map(str))\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "[10, 20, 30]\n[5, 9]\n[1, 22]\n");
}

#[test]
fn tasks_join_in_order_and_settle_their_errors() {
    let src = "async fn half(n) {\n    if n % 2 == 1 {\n        raise \"odd \" + str(n)\n    }\n    return n / 2\n}\n\
               print(join(half(4), half(8)))\n\
               for r in join(half(2), half(3), settle = true) {\n    print(r.ok)\n}\n\
               try {\n    join(half(1), half(2))\n} catch e {\n    print(e.message)\n}\n\
               print(set_max_tasks(0))\nprint(await half(10))\n";
    let (out, _, code) = run(src);
    assert_eq!(out, "[2, 4]\ntrue\nfalse\nodd 1\n1\n5\n");
    assert_eq!(code, 0);
}

#[test]
fn a_commands_output_is_refused_as_a_command_however_it_is_rebuilt() {
    let src = "use std::sh\nlet t = sh.exec(\"echo hi\")\nlet again = \"\"\nfor c in t {\n    again = again + c\n}\n\
               for cmd in [t, again, t.upper(), f\"{t}\", t.split(\"\")[1]] {\n    try {\n        sh.run(cmd)\n        print(\"ran\")\n    } catch e {\n        print(\"refused\")\n    }\n}\n\
               print(sh.exec(\"echo \" + \"trusted\"))\n";
    let (out, _, code) = run(src);
    assert_eq!(out, "refused\nrefused\nrefused\nrefused\nrefused\ntrusted\n");
    assert_eq!(code, 0);
}

#[test]
fn json_parses_and_writes_with_serdes_rules() {
    let src = "use std::json\nlet j = json.parse(\"{\\\"b\\\": [1, 2.5, null], \\\"a\\\": \\\"x\\\"}\")\n\
               print(j)\nprint(json.stringify(j))\nprint(int(j[\"b\"][0]) + 1)\n\
               try {\n    json.parse(\"{} oops\")\n} catch e {\n    print(e.message)\n}\n";
    let (out, _, _) = run(src);
    assert_eq!(
        out,
        "{\"a\": x, \"b\": [1, 2.5, nil]}\n{\"a\":\"x\",\"b\":[1,2.5,null]}\n2\n\
         I/O error: json.parse: trailing characters at line 1 column 4\n"
    );
}

#[test]
fn paths_follow_rusts_components() {
    let src = "use std::path\nprint(path.basename(\"a/b/\"))\nprint(path.dirname(\"/a\"))\nprint(path.ext(\"x.tar.gz\"))\n\
               print(path.stem(\".hidden\"))\nprint(path.join(\"a\", \"/b\"))\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "b\n/\n.gz\n.hidden\n/b\n");
}
