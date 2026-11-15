//! Tests for the library cache: what is cached, and that builds share it
//! safely. Each test uses a cache root of its own under the system's temp
//! directory, never `~/.jade`, and removes it when it ends.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::super::libplan::{Kind, LibPlan};
use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Purpose: This struct holds a scratch directory a test removes when done.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "jade-libcache-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Purpose: This function lowers a source program to MIR, loading the std
/// packages it uses the way a build does.
fn mir_of(src: &str) -> mir::Program {
    let tokens = crate::frontend::lexer::tokenize(src).expect("lex");
    let program = crate::frontend::parser::parse(tokens).expect("parse");
    let mut tree = crate::typechecker::resolve::resolve(program).expect("resolve");
    tree.stmts = crate::aot::imports::namespace_program(tree.stmts).expect("imports");
    crate::typechecker::resolve::fill_struct_literal_defaults(&mut tree).expect("defaults");
    let typed = crate::typechecker::infer::check_program(&tree).expect("check");
    mir::lower_program(&tree, &typed).expect("lowers")
}

/// Purpose: This function runs a built program.
///
/// Outputs: its standard output.
fn run(bin: &Path) -> String {
    let out = std::process::Command::new(bin).output().expect("runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Purpose: This function lists the object files in a cache root.
fn objects_in(root: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

/// Purpose: This function counts the objects a build compiled and wrote into
/// the cache.
fn written(r: &Report) -> usize {
    r.objects.iter().filter(|o| !o.hit && !o.scratch).count()
}

/// Purpose: This function asserts a build used the cache as planned.
fn assert_clean(r: &Report) {
    assert_eq!(r.fallback, None, "the build fell back to one module");
    assert!(r.demoted.is_empty(), "library modules depended on the program: {:?}", r.demoted);
    assert_eq!(r.relinked, None, "the cached objects did not link");
}

const PATHS: &str = "use std::path\nuse std::math\nuse std::env\n\
    print(path.join(\"a\", \"b\", \"c.txt\"))\nprint(path.stem(\"x/y.tar.gz\"))\n\
    print(path.dirname(\"/usr/lib/x\"))\nprint(math.clamp(7, 1, 5))\nprint(math.sqrt(16.0))\n\
    print(len(env.args()) > 0)\n";

#[test]
fn a_second_build_writes_no_object_and_prints_the_same() {
    let s = Scratch::new("second");
    let root = s.path("lib");
    let prog = mir_of(PATHS);
    let first = compile(&prog, &s.path("one"), &root).expect("first build");
    assert_clean(&first);
    assert!(written(&first) > 0, "a cold build compiles library objects: {first:?}");
    assert_eq!(first.hits(), 0);
    let files = objects_in(&root);
    assert_eq!(files.len(), first.objects.len());

    let second = compile(&prog, &s.path("two"), &root).expect("second build");
    assert_clean(&second);
    assert_eq!(written(&second), 0, "a warm build writes no object: {second:?}");
    assert_eq!(second.hits(), first.objects.len());
    assert_eq!(objects_in(&root), files, "the cache did not change");
    let out = run(&s.path("one"));
    assert_eq!(out, "a/b/c.txt\ny.tar\n/usr/lib\n5\n4.0\ntrue\n");
    assert_eq!(run(&s.path("two")), out);
}

#[test]
fn two_type_instances_of_one_function_get_different_objects() {
    let s = Scratch::new("instances");
    let prog = mir_of(
        "use std::random\nrandom.seed(3)\nprint(random.choice([1, 2, 3]))\n\
         print(random.choice([\"a\", \"b\", \"c\"]))\n",
    );
    let r = compile(&prog, &s.path("prog"), &s.path("lib")).expect("builds");
    assert_clean(&r);
    let choices: Vec<&LibObject> =
        r.objects.iter().filter(|o| o.label.starts_with("choice$")).collect();
    assert_eq!(choices.len(), 2, "{:?}", r.objects);
    assert_ne!(choices[0].path, choices[1].path);
    let symbol = |o: &LibObject| super::super::libplan::symbol(&o.label);
    assert_ne!(symbol(choices[0]), symbol(choices[1]));
    // The same draws as a build with no cache.
    super::super::compile_whole(&prog, &s.path("whole")).expect("builds whole");
    assert_eq!(run(&s.path("prog")), run(&s.path("whole")));
}

#[test]
fn a_class_typed_instance_is_not_cached() {
    let src = "use std::random\nuse std::time\nclass Card {\n    @public n\n}\nrandom.seed(7)\n\
               let c = random.choice([Card { n: 1 }, Card { n: 2 }])\nprint(c.n > 0)\n\
               print(random.choice([1, 2]) > 0)\nprint(time.parts(0).year)\n";
    let prog = mir_of(src);
    let plan = LibPlan::new(&prog);
    let kind = |label: &str| {
        let i = prog.funcs.iter().position(|f| f.name == label).expect(label);
        plan.kind(i)
    };
    assert_eq!(kind("choice$4294966277([Card])"), Kind::Program);
    assert_eq!(kind("choice$4294966277([int])"), Kind::Object);
    // `time.parts` returns a class the library declares, whose type id is
    // the program's.
    assert_eq!(kind("parts$4294966272(int)"), Kind::Program);
    let s = Scratch::new("class");
    let r = compile(&prog, &s.path("prog"), &s.path("lib")).expect("builds");
    assert_clean(&r);
    assert!(r.objects.iter().all(|o| !o.label.contains("Card") && !o.label.starts_with("parts")));
    assert_eq!(run(&s.path("prog")), "true\ntrue\n1970\n");
}

#[test]
fn a_small_instance_is_copied_and_others_get_objects() {
    let prog = mir_of(PATHS);
    let plan = LibPlan::new(&prog);
    let kind = |label: &str| {
        let i = prog.funcs.iter().position(|f| f.name == label).expect(label);
        plan.kind(i)
    };
    assert_eq!(kind("sqrt$4294966273(float)"), Kind::Copied);
    // `clamp` calls `max` and `min`, which are copied with it.
    assert_eq!(kind("clamp$4294966273(int, int, int)"), Kind::Copied);
    assert_eq!(kind("max$4294966273(int, int)"), Kind::Copied);
    assert_eq!(kind("join$4294966271(str, str)"), Kind::Object);
    assert_eq!(kind("main"), Kind::Program);
}

#[test]
fn an_unwritable_cache_root_still_builds() {
    let s = Scratch::new("unwritable");
    // A file where the cache's directory would go: no user, root included,
    // can make a directory under it.
    std::fs::write(s.path("blocker"), b"not a directory").expect("file");
    let root = s.path("blocker").join("lib");
    let prog = mir_of(PATHS);
    let r = compile(&prog, &s.path("prog"), &root).expect("builds without the cache");
    assert_clean(&r);
    assert_eq!(r.dir, None);
    assert!(!r.objects.is_empty() && r.objects.iter().all(|o| o.scratch && !o.hit));
    assert!(r.objects.iter().all(|o| !o.path.exists()), "scratch objects are removed");
    assert!(run(&s.path("prog")).starts_with("a/b/c.txt\n"));
}

#[test]
fn two_builds_at_once_both_link() {
    let s = Scratch::new("concurrent");
    let root = s.path("lib");
    let prog = mir_of(PATHS);
    let outs = [s.path("a"), s.path("b")];
    let reports: Vec<Result<Report, String>> = std::thread::scope(|sc| {
        let hs: Vec<_> = outs.iter().map(|o| sc.spawn(|| compile(&prog, o, &root))).collect();
        hs.into_iter().map(|h| h.join().expect("thread")).collect()
    });
    for r in reports {
        assert_clean(&r.expect("both build"));
    }
    for o in &outs {
        assert!(run(o).starts_with("a/b/c.txt\n"));
    }
    let files = objects_in(&root);
    assert!(files.iter().all(|f| f.extension().is_some_and(|e| e == "o")), "{files:?}");
}

#[test]
fn a_damaged_cached_object_is_rebuilt_or_linked_around() {
    let s = Scratch::new("damaged");
    let root = s.path("lib");
    let prog = mir_of(PATHS);
    let first = compile(&prog, &s.path("one"), &root).expect("first build");
    assert_clean(&first);
    // A file that is not an object at all is a miss, and is written again.
    let victim = &first.objects[0].path;
    std::fs::write(victim, b"garbage").expect("overwrite");
    let second = compile(&prog, &s.path("two"), &root).expect("second build");
    assert_clean(&second);
    assert_eq!(written(&second), 1);
    // One with the right magic number but nothing behind it fails to link,
    // and the build links without the cache instead.
    let magic: &[u8] = if cfg!(target_os = "macos") { b"\xcf\xfa\xed\xfe" } else { b"\x7fELF" };
    std::fs::write(victim, [magic, &[0u8; 64][..]].concat()).expect("overwrite");
    let third = compile(&prog, &s.path("three"), &root).expect("third build");
    assert!(third.relinked.is_some(), "{third:?}");
    assert!(run(&s.path("three")).starts_with("a/b/c.txt\n"));
}

/// A program that reaches most of the library, after the one `backend/tests.rs`
/// uses to pin the C functions a program declares.
const BIG: &str = "use std::time\nuse std::fs\nuse std::sh\nuse std::env\nuse std::random\nuse std::uhttp\n\
               use std::path\nuse std::math\nuse std::http\n\
               print(time.monotonic() > 0.0)\nprint(time.utc(0))\nprint(time.stamp(2020, 1, 2, 3, 4, 5))\n\
               fs.mkdir(\"/x\")\nfs.copy(\"/x\", \"/y\")\nfs.rename(\"/x\", \"/y\")\nfs.delete(\"/x\")\n\
               print(fs.read(\"/x\") + toString(fs.size(\"/x\")) + toString(fs.list_dir(\"/x\")))\n\
               print(fs.exists(\"/x\"))\nprint(sh.output(\"true\").code + sh.run(\"true\"))\nprint(sh.exec(\"true\"))\n\
               print(env.get(\"X\"))\nenv.set(\"X\", \"y\")\nprint(env.args())\nprint(env.cwd())\n\
               random.seed(1)\nprint(random.float())\nprint(random.int(1, 6))\nlet a = [1, 2]\nrandom.shuffle(a)\n\
               print(uhttp.get(\"unix:///s:/\").status)\nprint(http.get(\"http://x\").status)\n\
               print(path.abs(\"x\"))\nprint(path.basename(env.get(\"HOME\")))\nprint(math.pow(2, 10))\n";

/// None of `BIG`'s cached instances may ask for anything only the program
/// knows.
#[test]
fn no_library_module_depends_on_the_program() {
    let prog = mir_of(BIG);
    let plan = LibPlan::new(&prog);
    assert!(plan.objects().len() > 20, "{}", plan.objects().len());
    for id in plan.objects() {
        let ctx = Context::create();
        let (_, dependent) = library_module(&ctx, &prog, &plan, id).expect("compiles alone");
        assert!(!dependent, "{} depends on the program", prog.funcs[id].name);
    }
}

/// A library module defines no mutable state: every global it has is a
/// private constant, such as a string literal, a declaration of the C
/// library's, or a declaration of the program's state it recorded for the
/// program to define.
#[test]
fn library_modules_only_declare_shared_state() {
    let prog = mir_of(BIG);
    let plan = LibPlan::new(&prog);
    let mut declared = 0;
    for id in plan.objects() {
        let ctx = Context::create();
        let (cx, _) = library_module(&ctx, &prog, &plan, id).expect("compiles alone");
        let refs = cx.take_refs();
        let mut g = cx.module.get_first_global();
        while let Some(global) = g {
            let name = global.get_name().to_string_lossy().into_owned();
            let linkage = global.get_linkage();
            if global.is_declaration() {
                if name.starts_with("jade.") {
                    assert!(refs.contains_key(&name), "{name} is declared but not recorded");
                    declared += 1;
                }
            } else {
                assert!(global.is_constant(), "{} defines {name}", prog.funcs[id].name);
                assert!(
                    matches!(
                        linkage,
                        inkwell::module::Linkage::Private | inkwell::module::Linkage::Internal
                    ),
                    "{name} is {linkage:?}"
                );
            }
            g = global.get_next_global();
        }
    }
    assert!(declared > 0, "the error slot, at least, is the program's");
}

/// A cached instance's IR is a function of the instance alone: lowering the
/// program again, with new hash maps throughout, gives every library module
/// the same text, so the same key.
#[test]
fn a_library_modules_key_is_stable_and_covers_its_inputs() {
    let ir_of = |prog: &mir::Program, plan: &LibPlan, id: usize| {
        let ctx = Context::create();
        let (cx, _) = library_module(&ctx, prog, plan, id).expect("compiles alone");
        cx.module.print_to_string().to_string()
    };
    let (one, two) = (mir_of(BIG), mir_of(BIG));
    let (p1, p2) = (LibPlan::new(&one), LibPlan::new(&two));
    assert_eq!(p1.objects(), p2.objects());
    for id in p1.objects() {
        assert_eq!(ir_of(&one, &p1, id), ir_of(&two, &p2, id), "{}", one.funcs[id].name);
    }
    let a = ir_of(&one, &p1, p1.objects()[0]);
    let t = "arm64-apple-darwin";
    assert_eq!(object_key(&a, t), object_key(&a.clone(), t));
    assert_ne!(object_key(&a, t), object_key(&format!("{a} "), t));
    assert_ne!(object_key(&a, t), object_key(&a, "x86_64-unknown-linux-gnu"));
    assert_eq!(object_key(&a, t).len(), 64);
}

#[test]
fn a_program_without_library_code_builds_whole() {
    let s = Scratch::new("none");
    let r = compile(&mir_of("print(1 + 2)\n"), &s.path("prog"), &s.path("lib")).expect("builds");
    assert!(r.objects.is_empty());
    assert!(!s.path("lib").exists(), "nothing to cache, so no directory");
    assert_eq!(run(&s.path("prog")), "3\n");
}

/// A module that turns out to depend on the program sends its instance, and
/// every cached instance that calls it, back to the program.
#[test]
fn demoting_an_instance_demotes_its_callers() {
    let prog = mir_of(PATHS);
    let mut plan = LibPlan::new(&prog);
    let objects = plan.objects();
    let callers_of = |id: usize| -> Vec<usize> {
        objects
            .iter()
            .copied()
            .filter(|c| super::super::libplan::callees(&prog.funcs[*c]).contains(&id))
            .collect()
    };
    let victim = objects
        .iter()
        .copied()
        .find(|id| !callers_of(*id).is_empty())
        .expect("a cached instance another one calls");
    let callers = callers_of(victim);
    plan.demote(&prog, &[victim]);
    assert_eq!(plan.kind(victim), Kind::Program);
    assert_eq!(plan.symbol(victim), None);
    for c in callers {
        assert_eq!(plan.kind(c), Kind::Program, "{} calls the demoted one", prog.funcs[c].name);
    }
}
