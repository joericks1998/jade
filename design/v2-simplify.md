# One tree, `toDict`, and libraries written in Jade

Status: planned, 2026-11-14, for jade 2.0.6 to 2.0.9. Joe chose the direction of every
part. Details chosen while planning are marked *default*, and the questions still open
are listed at the end of each part; both are Joe's to overrule or settle.

## Summary

Four changes, each its own release:

| Version | Change | Visible to programs |
|---|---|---|
| 2.0.6 | TIR is removed; the later stages read the parser's tree | no |
| 2.0.7 | `toDict` and `toString` replace `std::json` and the `Json` type | yes |
| 2.0.8 | the standard library is written in Jade and compiled with the program | no |
| 2.0.9 | library code that does not depend on a program's types is compiled once into a cached `.a` and linked | no |

Part 1 goes first because every later part edits code that reads the tree. Part 2 goes
before part 3 because it deletes the largest library file, `backend/json.rs`, before
anything moves.

## Part 1: one tree (2.0.6)

### The problem

The compiler builds two trees before MIR: the parser's AST and TIR. They have the same
19 expression kinds and the same 21 statement kinds. TIR adds a type to every node, and
nothing after the per-file pass reads it: `JadeType` appears nowhere in
`typechecker/infer/` or `mir/`, and `aot/imports.rs` only renames class names inside
it. The whole-program checker works out every type again in its own table, and MIR
reads that table. The per-file types are left over from v1, whose bytecode emitter
read them.

MIR stays. LLVM optimizes integers, pointers and memory and knows nothing of Jade's
rules, so something has to place a `drop` on every exit path, write the error check
after each call that can raise, make one copy of a function per set of argument types,
run the task check, and turn a pipe over a generator into a copy of the generator.
MIR is where those decisions are written down.

### The plan

```
before: AST → type_infer → TIR → imports → checker → MIR → LLVM
after:  AST → per-file pass → AST → imports → checker → MIR → LLVM
```

- `typechecker/type_infer.rs` becomes a per-file pass over the AST that keeps the jobs
  that are not types:
  - rewriting a pipe chain into one node, with its hoisted arguments and piece name;
  - folding `toInt(?p)` and its kin into a typed dereference;
  - flattening inherited fields and filling in class literal defaults;
  - the per-file errors: undefined names, unknown fields and parents, inheritance
    cycles and clashes, `yield` beside `return`, and whole-reply stages.
- The pass keeps a table of what each name is, a function, a class, a grammar or a
  value, because the pipe and dereference rewrites need that and nothing more.
- The AST gains the fields the rewrites fill in: `Pipe` gets its hoisted arguments and
  piece name, `StructLiteral` gets `given`, and `PromptDeref` gets its output type and
  grammar. The parser leaves them empty.
- The type errors the per-file pass raises from its own types, such as mismatched
  operands, move to the whole-program checker wherever it does not already raise them.
  Every `*_error.jde` fixture must still fail `jade check` with a message that names
  the fix.
- `aot/imports.rs`, `typechecker/infer/`, `infer/rewrite.rs`, `mir/` and `cli/check.rs`
  read `Expr` and `Stmt` in place of `TExpr` and `TStmt`. `typechecker/tir.rs` and
  `JadeType` are deleted.
- The cache stores one tree per file instead of two, and `CACHE_FORMAT_VERSION` goes
  up.

### What changes for programs

Nothing. Every example prints what it printed before. A few type errors may be worded
the way the whole-program checker words them.

## Part 2: `toDict` and `toString` (2.0.7)

### The problem

`json.parse` returns a `Json` value, the only value in Jade whose kind is decided while
the program runs. It exists because a JSON object usually mixes kinds and a dict holds
one kind. Reading JSON straight into a class was designed in `v2-static-values.md` but
never built, and `json.stringify` writes any value, classes included.

### Joe's rules

- There is no `Json` type and no `std::json` package. JSON is a text format for reading
  and writing data, not a type.
- `toDict(text)` reads JSON text into a dict, nested levels included. `toString(d)`
  writes a dict as JSON text. Both work on dicts, never on classes. They follow 2.0.4's
  rule that a `toX` function converts.
- Mixed values do not fail. A dict whose values mix becomes a dict of strings. A
  failure only comes when the program converts a value that does not fit.
- Reading the file is `fs.read`. Nothing else about JSON lives in a package.

```
let city = toDict(fs.read("city.json"))   // {"name": "Paris", "population": 2100000}
print(city["name"])                        // Paris
let pop = toInt(city["population"])        // 2100000
toInt(city["name"])                        // raises: "Paris" is not an int
```

### How the value type is chosen

The compiler fixes a dict's type before the file exists, so the file cannot decide
whether its values mix. The type comes from how the program uses the dict, as every
other type in Jade does, and it is `str` when nothing says otherwise.

| The program uses the values as | The dict is | What `toDict` does with the text |
|---|---|---|
| ints, as in `scores["ana"] + 1` | a dict of ints | raises on a value that is not an int |
| floats | a dict of floats | takes ints and floats |
| bools | a dict of bools | raises on a value that is not `true` or `false` |
| dicts, as in `d["a"]["x"]` | a dict of dicts | each nested level follows the same rule |
| arrays | a dict of arrays | each element follows the same rule |
| strings, or nothing in particular | a dict of strings | every value fits: a number or `true` as the text the file wrote, a string unescaped, a nested object or array as its JSON text |

`toInt(city["population"])` does not make the dict a dict of ints: `toInt` accepts a
string as well as a number, so the call does not settle the dict's type, the dict stays
a dict of strings, and the conversion happens at the call. A
nested object read into a dict of strings comes back as text, and `toDict` reads it
again: `toDict(city["location"])["lat"]`.

### Writing

`toString(d)` keeps today's layout, `{"ana": 3, "bo": 5}`, which is already valid JSON
except in one place: a string inside a collection prints bare, so `{"k": "v"}` prints
as `{"k": v}` and `["a"]` as `[a]`. *Default:* a string inside a dict or an array
prints quoted and escaped, everywhere, so `print(d)` and `toString(d)` both give JSON.
A string on its own still prints bare, so `print("hi")` prints `hi`. Examples whose
expected output prints strings inside a collection get new `.expected` files, among
them `strings/write` and `collections/edge_cases`.

A dict of strings writes every value quoted, so a number read into a dict of strings
is written back as `"2100000"`.

### Trust

The keys and values `toDict` reads take the trust of the text they came from, as the
strings `json.parse` returns do today.

### What goes

- `compiler/src/json/`, `std::json` in `builtins/mod.rs`, `JsonFn` and `Ty::Json` in the
  checker and MIR, and most of `backend/json.rs`. The scanner that reads JSON text
  stays, as `toDict`'s code, generated once per value type a program uses.
- `json.stringify` of a class. A class that should be written out builds a dict.
- Examples that change: `json/parse`, `uhttp/basic`, `uhttp/stream` and
  `collections/self_reference`.
- Docs: the `std/json` section of `docs/docs/stdlib.md` goes, and `toDict` joins the
  converters and the dict section of `types.md`.
- VS Code: `toDict` joins the converter names in the grammar update that 2.0.4's
  converters already need.

### Defaults chosen while building

These were open when the plan was written. Each is a default Joe can overrule.

- *`null`:* in a dict of strings it is the text the file wrote, `"null"`, like `true`.
  In a dict of any other kind it raises, with a message that says to read the dict as
  strings and check for `"null"`.
- *A top-level array:* `toDict("[1, 2]")` raises, naming `toDict`'s rule that the text
  must hold an object. There is no `toArray` for now.
- *A key written twice:* the last one wins, as most JSON readers do.

## Part 3: the library written in Jade (2.0.8)

### The problem

Each library function lives in four places:

| File | What it holds |
|---|---|
| `compiler/src/<pkg>/mod.rs` | its name |
| `typechecker/infer/builtins.rs` | its type |
| `mir/packages.rs` | an MIR entry |
| `backend/<pkg>.rs` | its code, written as Rust calls that build LLVM instructions one at a time |

The fourth is the hard one to read. `math.pow` for ints is a loop, but its source reads
as instructions for building a loop.

### The plan

- Each package is Jade source, `compiler/src/lib/<pkg>.jde`, embedded in the compiler
  with `include_str!` the way `infer/prelude.jde` is, and added to a program that uses
  it the way `infer/rewrite.rs` adds the prelude. The checker types library functions
  by reading their bodies, like any imported file, so the name list, the type entry and
  the MIR entry go away.
- Jade code cannot reach the C library, so a small hook package, `__sys`, gives library
  code typed calls into it: files, sockets, the clocks, starting a process and the
  environment. Like `__infer`, a user's program cannot name it.
- The trust checks stay inside the hooks, in Rust. The hook that runs a command refuses
  a tainted string itself, so library code cannot skip the check, and the hooks that
  bring outside data in mark it tainted.
- A type error inside library code, such as `fs.read(5)`, is reported at the user's
  call and names the library function.
- What moves: the logic of `fs`, `path`, `env`, `time`, `random`, `sh`, `http` and
  `uhttp`, and the parts of `math` that have no LLVM instruction, such as `pow` for
  ints. `path` and `time`'s calendar arithmetic become plain Jade. `http` and `uhttp`
  build `curl`'s command line in Jade and start it through a hook.
- Shared state stays behind hooks. Jade's task check refuses a task that writes a
  global, and library code is checked like any other code, so state that tasks share,
  such as the random generator's, which a lock guards today, stays in Rust and library
  code reaches it through a hook.
- What stays generated per program in `backend/`: strings, bytes, arrays, dicts,
  refcounts, printing and `toString`, `==`, freeing classes, tasks, errors, `toDict`'s
  reader for each value type, and calls that are one instruction, such as `math.sqrt`.
  Bytes stays because it is a core value type, like a string.

### What changes for programs

Nothing visible. Builds compile the library source each time, which costs little: with
a debug build of the compiler, a program that uses JSON builds in 0.11 seconds today,
with 59 of its 60 functions coming from the library.

## Part 4: the cached, precompiled library (2.0.9)

### The plan

- The compiler keeps a cache of compiled library code under
  `~/.jade/lib/<version>/<target>/`, the way it keeps Rust packages under
  `~/.jade/rust/`, and LLD links from it. A released `jade` needs no LLVM installed
  for it, since it compiles in-process.
- The cache holds one object file per instance of a library function: the function
  with one set of argument types, which is the unit MIR already compiles. A program
  that needs `fs.read(str)` links the cached object for it, or compiles it once and
  adds it to the cache. Nobody declares which functions can be precompiled: every
  instance is compiled the first time any program on the machine needs it.
- An instance is cached only when its argument and result types are all built-in
  types. One that involves a program's own class, such as `random.choice` over an
  array of `City`, depends on that program's layout, so it compiles with the program,
  as Rust's precompiled library holds only code that is not generic.
- The state both sides share gets one definition, in the program: the error slot and
  the call-depth counter. Library objects refer to them. Helpers with no state, such as
  allocating a string, may appear in more than one object, each private to it.
- The layouts of strings, arrays, dicts and the error slot must match between a program
  and the cache. Keying the cache by compiler version and target ensures that.
- A program still depends on the C library alone when it runs, since the cached code is
  linked in statically.
- Object files are written to a temporary name and renamed into place, so two builds at
  once never read a half-written file.

## Docs that change

- `design/v2-static-values.md`: the pipeline, which says "TIR (fully typed)"; decision 5
  on JSON; and the note that support code is generated per program.
- The READMEs of `typechecker/`, `aot/`, `mir/`, `backend/`, `cache/`, `builtins/` and
  every package directory touched.
- `CLAUDE.md`'s pipeline diagram and directory map.
