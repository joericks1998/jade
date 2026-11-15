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
    let mut tree = crate::typechecker::resolve::resolve(program).expect("resolve");
    crate::typechecker::resolve::fill_struct_literal_defaults(&mut tree).expect("defaults");
    let typed = crate::typechecker::infer::check_program(&tree).expect("check");
    mir::lower_program(&tree, &typed).expect("lowers")
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
               print(toInt(\"3\") + len(\"abc\".split(\"\")))\nprint(join(nap(1), nap(2)))\nprint(time.monotonic() > 0.0)\n";
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
               print([1, 2, 3].map(scale, 10))\nprint([1, 5, 9].filter(above, 4))\nprint([1, 22].map(toString))\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "[10, 20, 30]\n[5, 9]\n[\"1\", \"22\"]\n");
}

#[test]
fn tasks_join_in_order_and_settle_their_errors() {
    let src = "async fn half(n) {\n    if n % 2 == 1 {\n        raise \"odd \" + toString(n)\n    }\n    return n / 2\n}\n\
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
fn sized_numbers_compute_at_their_width_and_raise_past_it() {
    let src = "@byte\nlet b = 250\nprint(b + 5)\ntry {\n    print(b + 6)\n} catch e {\n    print(e.message)\n}\n\
               @short\nlet s = -300\nprint(s / 7)\nprint(s % 7)\n\
               @uint32\nlet u = 4000000000\nprint(u / toUint32(3))\nprint(u > toUint32(1))\n\
               @ulong\nlet top = toUlong(9223372036854775807) * toUlong(2)\nprint(top)\nprint(top >> 60)\n\
               @float32\nlet f = 0.1\nprint(f)\nprint(f + f + f)\n\
               print(toByte(255) & toByte(15))\nprint(toInt(s) * 1000000)\n\
               try {\n    print(toByte(256))\n} catch e {\n    print(e.message)\n}\n";
    let (out, _, code) = run(src);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "255\nbyte overflow\n-42\n-6\n1333333333\ntrue\n18446744073709551614\n15\n\
         0.1\n0.3\n15\n-300000000\n256 does not fit a byte, which holds 0 to 255\n"
    );
}

/// A dict of strings takes every value: a string unescaped, and anything else
/// as the exact text the input wrote, a nested object or array included.
#[test]
fn to_dict_reads_a_dict_of_strings_as_the_text_written() {
    let src = r#"let d = toDict('{"s": "a\\"b\\u00e9\\ud83d\\ude00", "n": -1.50e3, "t": true, "z": null, "o": {"x": [1, {}]}, "k": 1, "k": 2}')
print(d["s"])
print(d["n"])
print(d["t"])
print(d["z"])
print(d["o"])
print(d["k"])
print(len(d))
print(toDict(d["o"]))
"#;
    let (out, err, rc) = run(src);
    assert_eq!((err.as_str(), rc), ("", 0));
    assert_eq!(
        out,
        "a\"b\u{e9}\u{1f600}\n-1.50e3\ntrue\nnull\n{\"x\": [1, {}]}\n2\n6\n{\"x\": \"[1, {}]\"}\n"
    );
}

/// Use settles the value type, and the reader builds that type: ints, floats
/// that take ints, bools, and nested dicts and arrays.
#[test]
fn to_dict_builds_the_value_type_the_program_uses() {
    let src = r#"let i = toDict('{"a": 1, "b": -9223372036854775808}')
print(i["a"] + 1)
print(i["b"])
let f = toDict('{"a": 2, "b": 0.5}')
print(f["a"] + f["b"] * 1.0)
let b = toDict('{"on": true, "off": false}')
if b["on"] == true {
    print(b)
}
let n = toDict('{"a": {"x": [1, 2]}, "b": {}}')
print(n["a"]["x"][1] + 1)
print(n)
"#;
    let (out, err, rc) = run(src);
    assert_eq!((err.as_str(), rc), ("", 0));
    assert_eq!(
        out,
        "2\n-9223372036854775808\n2.5\n{\"off\": false, \"on\": true}\n3\n{\"a\": {\"x\": [1, 2]}, \"b\": {}}\n"
    );
}

/// A value that does not fit raises with the line and column and the fix, as
/// does text that is not JSON or holds no object.
#[test]
fn to_dict_raises_where_the_text_does_not_fit() {
    let src = r#"fn try_int(t) {
    try {
        let d = toDict(t)
        print(d["a"] + 1)
    } catch e {
        print(e.message)
    }
}
try_int('{"a": 1.5}')
try_int('{"a": 99999999999999999999}')
try_int('{\n  "a": null}')
try_int('{"a": [1]}')
try_int('{"a": 1} x')
try_int('"text"')
try_int('{"a": tru}')
fn try_floats(t) {
    try {
        let d = toDict(t)
        print(d["a"] * 2.0)
    } catch e {
        print(e.message)
    }
}
try_floats('{"a": "1"}')
try_floats('{"a": 1e999}')
"#;
    let (out, _, rc) = run(src);
    assert_eq!(rc, 0);
    let want = [
        "toDict: expected an int, found a float at line 1 column 7; to read mixed values, use \
         the dict as strings and convert each with toInt()",
        "toDict: expected an int, found a number too long for 64 bits at line 1 column 7; read \
         the dict as strings to keep its digits",
        "toDict: expected an int, found null at line 2 column 8; read the dict as strings and \
         check for \"null\"",
        "toDict: expected an int, found an array at line 1 column 7; to read mixed values, use \
         the dict as strings and convert each with toInt()",
        "toDict: trailing characters at line 1 column 10",
        "toDict: the text must hold a JSON object, found a string at line 1 column 1; to read \
         another value, put it under a key, such as {\"items\": [1, 2]}",
        "toDict: expected ident at line 1 column 10",
        "toDict: expected a float, found a string at line 1 column 7; to read mixed values, use \
         the dict as strings and convert each with toFloat()",
        "toDict: number out of range at line 1 column 11",
    ];
    assert_eq!(out, want.join("\n") + "\n");
}

/// Nesting past serde's limit is refused, in a dict of strings too, which reads
/// past nested text rather than building it.
#[test]
fn to_dict_refuses_nesting_past_the_limit() {
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    let src = format!(
        "try {{\n    let d = toDict('{{\"a\": {deep}}}')\n    print(d[\"a\"])\n}} catch e {{\n    print(e.message)\n}}\n"
    );
    let (out, _, _) = run(&src);
    assert_eq!(out, "toDict: recursion limit exceeded at line 1 column 133\n");
}

/// Keys and values take the trust of the text they came from, so a command's
/// output read into a dict cannot reach `sh.run` by either.
#[test]
fn to_dict_keys_and_values_keep_the_texts_trust() {
    let src = "use std::sh\nlet d = toDict(sh.exec(\"printf '{\\\"echo hi\\\": \\\"echo hi\\\"}'\"))\n\
               for k in d.keys() {\n    for cmd in [k, d[k]] {\n        try {\n            sh.run(cmd)\n            print(\"ran\")\n        } catch e {\n            print(\"refused\")\n        }\n    }\n}\n\
               let mine = toDict(\"{\\\"c\\\": \\\"echo trusted\\\"}\")\nprint(sh.exec(mine[\"c\"]))\n";
    let (out, _, code) = run(src);
    assert_eq!(out, "refused\nrefused\ntrusted\n");
    assert_eq!(code, 0);
}

/// A string inside an array or a dict prints quoted and escaped as JSON, so
/// `toString` of a dict is JSON; a string on its own prints bare.
#[test]
fn strings_inside_collections_print_as_json() {
    let src = "print(\"a\\\"b\")\nprint([\"a\\\"b\", \"c\\\\d\", \"e\\nf\\t\\r\"])\n\
               print({\"k\\\"\": \"v\"})\nlet m = [nil, \"x\"]\nprint(m)\n\
               let ys = [\"y\"]\nprint(f\"{ys}\")\nprint(toString({\"caf\u{e9}\": [\"\u{e9}\"]}))\n\
               print([\"a\", \"b\"].join(\",\"))\n";
    let (out, _, _) = run(src);
    assert_eq!(
        out,
        "a\"b\n[\"a\\\"b\", \"c\\\\d\", \"e\\nf\\t\\r\"]\n{\"k\\\"\": \"v\"}\n[nil, \"x\"]\n[\"y\"]\n\
         {\"caf\u{e9}\": [\"\u{e9}\"]}\na,b\n"
    );
}

#[test]
fn paths_follow_rusts_components() {
    let src = "use std::path\nprint(path.basename(\"a/b/\"))\nprint(path.dirname(\"/a\"))\nprint(path.ext(\"x.tar.gz\"))\n\
               print(path.stem(\".hidden\"))\nprint(path.join(\"a\", \"/b\"))\n";
    let (out, _, _) = run(src);
    assert_eq!(out, "b\n/\n.gz\n.hidden\n/b\n");
}

/// A pipe over a call to a generator runs each value through its stages as it
/// is yielded, before the generator makes the next. A stage's other arguments
/// are worked out once, a raise in a stage passes the generator's own `try`,
/// and a generator that pipes itself compiles and terminates.
#[test]
fn a_pipe_calls_its_stages_as_each_value_is_yielded() {
    let (out, err, rc) = run(concat!(
        "fn two() {\n    print(\"a\")\n    yield 1\n    print(\"b\")\n    yield 2\n}\n",
        "fn add(x, y) { return x + y }\n",
        "let n = 0\nfn once() {\n    n = n + 1\n    return n * 10\n}\n",
        "two() |> add(once()) |> print\n",
        "print(n)\n",
        "print(two() |> add(1))\n",
        "let held = two()\n",
        "print(held |> add(5))\n",
        "fn guarded() {\n    try {\n        yield 1\n        yield 2\n    } catch e {\n        print(\"inner\")\n    }\n}\n",
        "fn no_two(x) {\n    if x == 2 {\n        raise \"two\"\n    }\n    return x\n}\n",
        "try {\n    guarded() |> no_two |> print\n} catch e {\n    print(f\"outer {e}\")\n}\n",
        "fn down(k) {\n    if k > 0 {\n        down(k - 1) |> print\n    }\n    yield k\n}\n",
        "print(down(2))\n",
    ));
    assert_eq!(err, "");
    assert_eq!(rc, 0);
    assert_eq!(out, "a\n11\nb\n12\n1\na\nb\n[2, 3]\na\nb\n[6, 7]\n1\nouter two\n0\n1\n[2]\n");
}
