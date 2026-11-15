# `src/lib/`: the std library written in Jade

## What this subtree is

Each `.jde` file here is a std package written in Jade, embedded in the compiler with `include_str!` and compiled with any program that uses it, the way the inference prelude, `src/infer/prelude.jde`, is. Since 2.0.8 every std package lives here but the value types' own, `string`, `array`, `dict` and `bytes`, which the backend writes:

| File | Package | Hooks it calls |
|---|---|---|
| `path.jde` | `std::path` | `cwd` |
| `time.jde` | `std::time`, and its `TimeParts` class | the clocks, `sleep`, `after`, `local_offset`, `local_zone`, `add_sat`, `mul_sat` and the `wrap_*` ints |
| `math.jde` | `std::math` | one per float function, `to_int`, `float_bits` |
| `env.jde` | `std::env` | `getenv`, `setenv`, `args`, `cwd` |
| `fs.jde` | `std::fs` | `read_text`, `read_file`, `write_file`, `read_all`, `write_fd`, `stat`, `unlink`, `rmdir`, `mkdir`, `rename`, `copy_file`, `read_dir`, `os_error`, `strerror` |
| `sh.jde` | `std::sh`, and its `ShOutput` class | `shell`, `drain`, `close`, `wait`, `lossy`, `os_error`, `strerror` |
| `random.jde` | `std::random` | `rand_below`, `rand_seed`, `wrap_add`, `wrap_sub` |
| `http.jde` | `std::http`, and its `HttpResponse` and `HttpBytesResponse` classes | `spawn`, `write_child`, `read_all`, `close`, `wait`, `lossy`, `keys_in_order`, `os_error`, `wrap_add`, `wrap_mul` |
| `uhttp.jde` | `std::uhttp`, over `http.jde` | `spawn`, `read_chunk`, `kill`, `close`, `wait`, `lossy`, `os_error` |

The Rust module is `crate::stdlib`, since a module named `lib` beside `lib.rs` would collide with the crate root; `lib.rs` points it at this directory.

## Why it was built

Until 2.0.8 every std function lived in four places: its name in `src/<pkg>/mod.rs`, its type in `typechecker/infer/builtins.rs`, an MIR operation in `mir/packages.rs`, and its code in `src/backend/<pkg>.rs`, written as Rust calls that build LLVM instructions one at a time. The last was the hard one to read: `math.pow` for ints is a loop, but its source read as instructions for building a loop. Part 3 of `design/v2-simplify.md` moves each package's logic into Jade, so it lives in one place and reads as what it does. The checker types a library function by reading its body, as it types any imported file's, so the name list, the type entry and the MIR entry go away.

## How a package is compiled into a program

1. *The per-file pass names the package.* `use std::path` binds `path` in `typechecker/resolve.rs` as a std package, through `builtins::std_global_name`, and `path.joinx(…)` is refused there, listing the package's public functions.
2. *The import pass loads it.* `aot/imports.rs` sees that `stdlib::find` knows the path, and `load_library` parses the source with `typechecker::resolve::resolve_library` and namespaces it under the package's reserved id, once per program: every top-level name becomes `name$<id>`, so `path.join(a, b)` is a call to `join$<id>` and no name here can meet one of the program's. A program's own class named `TimeParts` stays its own. A library module may `use` another; it lands first, and the module that uses it reaches its private functions too, so `uhttp.jde` calls `http._fetch` and returns `http.jde`'s `HttpResponse`, the same class `http.get` returns. A program reaches only the public ones.
3. *The checker types it per call.* Each library function is checked once per set of argument types, like any function. A function nobody calls is never checked or compiled, so a program pays only for what it uses.
4. *MIR and the backend compile it* like the program's own code, with two differences. A library function does not count against the recursion limit, as a built-in never did, so the optimizer inlines a one-instruction function such as `math.sqrt` down to that instruction; and what it asks of `__sys` reaches the backend as `Op::Sys`.
5. *The backend compiles each instance once per machine.* Since 2.0.9 an instance whose types are all built in, such as `path.join(str, str)` or `fs.read(str)`, is compiled in an LLVM module of its own, and its object is kept under `~/.jade/lib/<version>/<triple>/` and linked into every later program that needs it; `src/backend/libplan.rs` decides which, and `src/backend/README.md` describes the cache. An instance that touches a class, such as `time.parts`, which returns a `TimeParts`, or `random.choice` over a program's class, compiles with the program, since a class's type id is its place among the program's classes. A small instance, such as `math.sqrt` or `math.clamp`, is copied into each module that calls it, so it still inlines.

A program with no file, such as a test's, gets its library modules through `aot::imports::namespace_program`.

## Writing a package

*Public and private names.* Every top-level `fn` whose name does not start with `_` is public: a program can call it, and the per-file pass lists it, in source order, when a program asks for a function the package does not have. A name starting with `_` is the module's own. Keep the public functions in the order the old registry listed them, since that order is what the message prints.

*Classes.* A record a function returns is a class declared here, with every field `@public`, so `t.year` and `t["year"]` both read it. It is not predeclared in every program the way `RuntimeError` is, and a program cannot name it.

*Built-in type tests.* Library code, and only library code, may test a value's built-in type: `x is int`, `x is float`, `x is str`, `x is bool`, `x is char`, `x is bytes` and `x is nil`, alone or joined by `and`, `or` and `not`. Each instance of a function has one type per variable, so the checker answers such a test per instance and checks only the branch it takes; MIR lowers only that branch. That is how `math.abs` returns an int for an int and a float for a float:

```
fn abs(x) {
    if (x is int) {
        if (x < 0) {
            return -x
        }
        return x
    } else {
        return __sys.fabs(float(x))
    }
}
```

Put each kind's code in its own branch, `else` included: code after the `if` is checked for every instance. A test on a `T?` is asked while the program runs, and the variable reads as a `T` inside the branch and as nil outside it; `path.jde`'s `_text` uses that to accept a `str?`, as the old `std::path` did.

*Checking arguments.* Start a function with the type function for what it needs, `str(p)`, `int(ts)`, `float(x)` or `bool(trust)`. A type function checks and never converts, except that `float` takes an int, so the error a program sees names the fix: `in path.join(int, str): str() needs a str, got int; convert one with toString()`. A module that defines a function named for a type function, as `random.jde` defines `int` and `float`, shadows it, since every top-level name is the module's; spell the type function `long` or `double` there. A value with no type function, such as bytes, is checked by the hook it reaches, and the message names the library function and the types, `in fs.write_bytes(str, str): type mismatch in an argument: str and bytes do not mix`, not the hook. Make sure every argument does reach a typed hook or a type function on every path an instance checks: `http.jde` tests `body is nil` rather than `body is bytes`, so a str body is refused rather than quietly dropped.

*Errors at run time.* `raise "message"` raises the same `RuntimeError` the backend's support code raised, so a program that catches one sees the same message. Integer arithmetic raises `integer overflow` as it does anywhere. Where the old code let a sum wrap or saturate instead, say so with `__sys.wrap_add` and its kin or `__sys.add_sat` and `__sys.mul_sat`; `time.jde`'s `stamp` does, and so does `random.jde`'s range, which wraps for the whole int range.

*Errors from the C library.* A hook whose C call fails returns an empty value and leaves the C library's error code in the thread's slot; `__sys.os_error()` reads it, 0 when the last such hook succeeded, and `__sys.strerror(code)` gives its text. Read it right after the hook and build the message in Jade: `fs.jde`'s `_check(op, path)` raises `I/O error: <op> '<path>': <reason> (os error <n>)`, v1's text. The slot is only written by hooks, so nothing in between can change it, and each task's thread has its own.

*Records.* A class a function returns is built in Jade, `ShOutput { stdout: …, stderr: …, code: … }`, never by a hook.

*Variadic functions.* A Jade function takes a fixed number of arguments. A package function that took any number, as `path.join` did, folds: list it in its `Library`'s `folds`, and the import pass turns `join(a, b, c)` into `join(join(a, b), c)`.

*Keep classes to the functions that return one.* An instance that holds a class value, or calls an instance that does, cannot be cached, and compiles with every program that uses it; so does any function that calls it. `time.jde`'s calendar arithmetic, `_days_from_civil`, takes and returns ints and is cached, while `utc` and `parts`, which build a `TimeParts`, are not. Doing the arithmetic in functions of built-in types and building the class at the end keeps most of a package cached. `raise "text"` is the exception: the `RuntimeError` it builds does not stop an instance from being cached.

*What library code may not do.* It may not write a Jade global: a task may call it, and the task check in `mir/taskcheck.rs` checks library code like any other. State that tasks share, such as the lock around `TZ` and the random generator, stays behind a hook. So does every trust check: a hook that runs a command or reads a path refuses tainted text itself, naming the library function it was given, `__sys.shell(cmd, "sh.exec", capture)` as `sh.exec(cmd)`, and a hook that brings outside text in marks it tainted, so library code can neither skip nor launder one. Jade's string operations carry trust on their own: a slice keeps its string's trust and a concatenation takes the most tainted of its parts, and so do `bytes.concat`, a bytes slice and `__sys.lossy`. Where a result keeps none of a path's text, `path.jde` adds an empty slice of the path, `s.slice(0, 0)`, to carry its trust.

## Hooks: the `__sys` package

What Jade cannot do on its own, a library module asks of `__sys`, with `use std::__sys` at its top. Each hook is one operation the C library or LLVM provides: a clock read, a sleep, an LLVM intrinsic, the local time zone's offset at an instant, a file read, starting a process, a read from a pipe, a draw from the random generator. A few are operations on a value Jade has no spelling for: `lossy`, which decodes bytes with bad sequences replaced, and `keys_in_order`, which gives a dict's keys in the order they were added, since a `for` loop sorts them. A hook is typed exactly, so a float parameter refuses an int and library code writes `float(x)`. A process is an `[int]` of its pid and descriptors, which `close`, `wait` and `kill` take back.

Only the four hooks that refuse tainted text raise, `read_text`, `read_file`, `shell` and `spawn`; `SysFn::may_raise` says so, and MIR checks for an error after those alone. Every other failure goes to the error slot above.

A program cannot name `__sys`: its `use std::__sys` is refused as an undefined name, `undefined variable '__sys'`, as `__infer` is, and so is a bare `__sys.now()`.

To add a hook:

1. Add a variant to `SysFn` in `src/mir/mod.rs`, to `SysFn::ALL`, and its name in `SysFn::name`. Give its argument types in `SysFn::params` and its result in `SysFn::result`; the checker and MIR both read them, so nothing else types it. If it refuses tainted text, add it to `SysFn::may_raise`.
2. Emit it in `Cx::sys_op` in `src/backend/sys.rs`, with its code there or in `sys_fs.rs`, `sys_proc.rs` or `sys_rand.rs`. A fallible one calls `F::os_ok` and `F::os_fail` or `F::os_result` right after its C call. A C library function it calls goes in `Cx::libc`'s table, and in the pinned list in `backend/tests.rs`.
3. Call it from the package's `.jde`.

## Adding a package

1. Write `src/lib/<pkg>.jde`, beginning with `use std::__sys` if it calls a hook.
2. Add a `Library` entry at the end of `LIBRARIES` in `mod.rs`, with its `include_str!` and any `folds`. A package's index there gives its namespace id, so a new one goes last.
3. If the package was written in the backend before, delete its `src/<pkg>/` directory and its `PACKAGES` entry in `src/builtins/`, its typing in `typechecker/infer/builtins.rs`, its MIR operations in `mir/` and `mir/packages.rs`, and its backend code, keeping what becomes a hook. A record it returned moves from `builtin_structs` in `typechecker/infer/builtins.rs` into the `.jde` as a class.
4. Run the old and new compilers over the same programs and compare what they print, error messages included. `examples/path/components/`, `examples/time/calendar_edges/` and `examples/numbers/math_edges/` were checked that way, and in phase B `examples/fs/edges/`, `fs/stdio/`, `env/vars/`, `sh/capture/`, `random/seeded/`, `http/local/` and `uhttp/local/`, whose `.expected` files the backend versions recorded.

## What each file does

- *`mod.rs`* holds `Library`, the `LIBRARIES` table, and the lookups the import pass, the per-file pass, the checker and MIR use: `find` by the path a `use` names, `by_name`, `by_id` and `owner` by a mangled name, `display`, which names a library function as a program calls it, `folds`, `public_fns`, which reads a module's public functions from its source once, `statements`, which parses a module in library mode, and `dedupe`.
- *`path.jde`* treats a path as text, following Rust's `std::path::Path` on Unix as v1 did. Only `abs` asks the system anything, for the working directory.
- *`time.jde`* holds the two clocks, `sleep` and `after`, `local`, which formats now as `date` does from the zone's offset and name, two hooks because a hook returns one typed value, and the calendar arithmetic: Howard Hinnant's `days_from_civil` and `civil_from_days`, `utc`, `parts` and `stamp`.
- *`math.jde`* holds `std::math`. A one-instruction function calls its hook; `pow` of two ints, `clamp`, `sign`, `min`, `max`, `abs` and the rounding functions are written out, with v1's results: halves round away from zero, a float becomes an int by saturating, NaN gives way in `min` and `max`, and the sign of zero is kept.
- *`env.jde`* holds `std::env`, one hook a function: a variable's value tainted, the arguments and the working directory trusted.
- *`fs.jde`* holds `std::fs`: each function calls its hook and turns a failure into v1's `I/O error` message; `mkdir` makes each parent in turn and reports the error v1 read, after asking whether a directory was there; `list_dir` drops `.` and `..`; `exists`, `is_file` and `is_dir` read `stat`'s mode and never raise; `read` raises v1's `stream did not contain valid UTF-8` for octets that are not text.
- *`sh.jde`* holds `std::sh`: each function starts the shell through `shell`, which refuses a tainted command; `exec` drops trailing newlines, and only those, and raises with standard error trimmed of Unicode white space at both ends, as v1's `trim_start` then `trim_end` did; `output` returns an `ShOutput`.
- *`random.jde`* holds `std::random` over two hooks. `int` draws below the range's size, which wraps to 0 for the whole int range, and adds the draw back with wrapping; `float` scales a draw's top 53 bits; `choice` and `shuffle`, Fisher-Yates from the last element down, are Jade. Each draws exactly what the backend's version drew, so a seeded program's sequence is unchanged.
- *`http.jde`* holds `std::http` and the transport `uhttp.jde` shares: `_fetch` builds curl's command line, headers in the order the program wrote them, writes the body, and reads the reply; `_status_at` finds the last status mark, so a body cannot fake it, `_text` cuts a text body at its first NUL, `_unix_target` reads a uhttp target, and `_fail` words a transport failure as v1 did.
- *`uhttp.jde`* holds `std::uhttp`: the requests, through `http.jde`, and `stream`, which reads curl's output a chunk at a time, calls the handler once a line, the last one without a newline included, and stops curl when the handler returns false or raises.
- *`tests.rs`* checks that each module compiles on its own, the public functions and their order, the moved packages' functions in the order the backend's registry listed them, that only library code reaches a module's private functions, the namespace ids, `dedupe`, and that every hook's name maps back to it.

## Who uses it

*Used by:* `aot/imports.rs`, which loads each module a program uses; `backend/libplan.rs` and `backend/libcache.rs`, which compile its instances once per machine and cache them; `typechecker/resolve.rs` and `builtins/`, through `builtins::is_std_package`, `std_global_name` and `std_functions`; `project/imports.rs`, whose import walk treats these packages as built in; `typechecker/infer/`, which recognizes library code with `is_library_fn` and names its functions with `display` in messages; and `mir/`, which marks library functions with `Func::library`.

*Depends on:* `frontend/` to parse a module and `typechecker::resolve::resolve_library` for the per-file pass.

## Building and testing

```sh
cargo test stdlib::
./target/debug/jade run examples/path/components/components.jde
./target/debug/jade run examples/uhttp/local/local.jde                            # a stand-in for curl, no network
./target/debug/jade build examples/numbers/math_edges/math_edges.jde --emit mir   # the library's instances
```
