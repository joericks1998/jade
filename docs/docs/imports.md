---
id: imports
title: Imports
sidebar_label: Imports
---

The `use` statement loads another `.jde` file or a standard-library package, and makes its definitions available in the file doing the importing. There is only *one import form*. A `use` statement names a *module*, either with `::` notation or as a bare name, and the import binds under the module's last path segment. There are no quoted file paths, and there is no `as` alias.

```jade
use utils          // a sibling ./utils.jde        → binds `utils`
use sub::helper    // ./sub/helper.jde             → binds `helper`
use std::math      // a standard-library package   → binds `math`
use mylib::shapes  // a registered [lib] module     → binds `shapes`
use fastmath       // an installed dependency        → binds `fastmath`
```

- A `use` statement must appear at the top level, never inside a function body.
- Jade runs the imported file once, and its definitions become reachable through the bound name.
- The bound name is always the *last segment* of the path, so `sub::helper` binds `helper`. To bind a different name, rename the file.

## Local files by name

A bare name resolves to a *sibling `.jde` file*. A `::` path descends into subdirectories. Both are resolved relative to the directory holding the *importing* file.

Here is `math_lib.jde`, a sibling of the importer:

```jade
fn add(a, b) { return a + b }
fn mul(a, b) { return a * b }
```

And here is `main.jde`, the importer:

```jade
use math_lib

let x = math_lib.add(2, 3)   // 5
let y = math_lib.mul(4, 5)   // 20
```

```jade
// project/
//   main.jde
//   lib/
//     utils.jde

// Inside main.jde:
use lib::utils      // -> ./lib/utils.jde, binds `utils`
```

:::note
`::` descends into subdirectories only. You cannot write a module path that reaches a *parent* directory or a sibling directory, so `../shared/util.jde` has no module-path spelling. Register those directories as a `[lib]` instead, described below, which anchors resolution at the project root. Absolute paths are never supported.
:::

## Library Imports (`[lib]`)

Bare names and `::` paths only reach *down* from the importing file. So a module shared across a deep tree, such as a `src/utils/` used from several directories, has no plain module-path spelling. To import a module from anywhere in a project, register a *library* in `jade.toml`. A library is a named directory, and you can optionally list which of its modules may be imported.

```toml
# jade.toml
[project]
name = "myapp"

[lib.utils]
path  = "src/utils"             # directory, relative to the project root
files = ["math.jde", "io.jde"]  # optional: allowlist of importable filenames
```

`files` is optional:

- *Omit it* to make every recognized file in the directory importable.
- *List filenames*, with their extensions, to allow only those.

A module's *file extension decides how it loads*:

- `.jde` is a Jade source module.
- `.dylib` and `.so` are *native* C-ABI shared libraries, such as a Rust crate built as a `cdylib`. Jade loads one over the `jade_pkg_init` FFI and binds it as a dict of functions.

```toml
[lib.ext]
path  = "lib"
files = ["math.jde", "fastmath.dylib"]   # one Jade module, one native library
```

```jade
use ext::math       // -> lib/math.jde     (Jade)
use ext::fastmath   // -> lib/fastmath.dylib (native), then fastmath.some_fn(...)
```

Native libraries work in both engines. The interpreter loads one with `dlopen`. A compiled binary also loads it with `dlopen` at startup, then calls through `jrt_native_call`. The same `.so` file serves both.

Then import the module with `::` notation from *any* file in the project. Jade resolves the path against the library's directory, anchored at the project root, rather than against the importing file. The import binds to its last segment automatically, with no `as` needed:

```jade
use utils::math               // -> <root>/src/utils/math.jde, from anywhere
print(math.square(5))        // binds as `math` (the last segment)
```

Rules:

- A `use` path is a *library reference* when its first segment names a registered library. Otherwise it resolves as a relative module, meaning a sibling file or a subdirectory. Either way it binds the last segment, and there is no alias.
- With a `files` list in place, importing a module you did not list is a hard error in both `jade run` and `jade build`. Without the list, a missing file gives an ordinary not-found error.
- Library resolution works the same way in the VM and in a compiled binary.

## What Gets Imported

The imported module's top-level functions, variables, and struct definitions are all reachable through the bound name, written `<name>.<member>`. The imported file runs all the way through before the importing file continues past the `use` statement.

| Exported from `mathlib.jde` (`use mathlib`) | Available after `use` |
|----------------------------------------------|-----------------------|
| `fn add(a, b) { … }` | `mathlib.add(2, 3)` works |
| `let PI = 3.14159` | `mathlib.PI` is in scope |
| `struct Point { x, y }` | `mathlib.Point { x: 1, y: 2 }` works |

## Multiple Imports

A file may contain several `use` statements. Jade processes them in order.

```jade
use mathlib
use stringlib

let n = mathlib.add(1, 2)
let greeting = stringlib.concat("hello", " world")
```

## No Re-export

Imports are not passed along. If `a.jde` uses `b.jde`, then a third file that uses `a.jde` does *not* get access to what `b.jde` defined. Every file imports what it needs directly.

The same rule decides what a *package* exposes. A package built from several files compiles all of them into one artifact, but only the entry module's own top-level functions become bindings. Everything its imports defined stays internal. To publish one of those, forward it:

```jade
// mathlib.jde is the entry module, so it is the package's API
use geometry

fn area(w, h) { return geometry.area(w, h) }
```

So adding a helper to `geometry.jde` never quietly widens what users of `mathlib` can call. See [Packages](packages#a-package-of-several-files).

---

## Standard Library Packages

Import a standard-library package with `::` notation, such as `use std::json`, never with a quoted path. These packages are always available and need no installation.

```jade
use std::math
use std::json
use std::path
use std::random

let n = math.sqrt(144.0)          // 12.0
let data = json.parse('{"x": 1}')
let p = path.join("src", "main.jde")
let roll = random.int(1, 6)
```

Importing a package binds it as a global variable named after the package, such as `math`, `json`, or `path`. The table below lists every available package.

:::warning
Jade rejects quoted-string imports of every kind at compile time, with a `QuotedImport` error. That covers both `use "std/math"` and `use "lib.jde" as lib`. The `as` alias is rejected too, with `ImportAlias`. Always name a module with `::` notation, such as `use std::math` or `use utils`.
:::

| Import | Global | Summary |
|--------|--------|---------|
| `use std::math` | `math` | `floor`, `ceil`, `abs`, `sqrt`, `min`, `max`, `pow` |
| `use std::string` | `string` | `split`, `upper`, `lower`, `trim`, `contains`, `replace`, `starts_with`, `ends_with` |
| `use std::array` | `array` | `map`, `filter`, `sort`, `reverse`. All take a function and none mutate |
| `use std::dict` | `dict` | `keys`, `values`, `has`, `get`, `merge` |
| `use std::fs` | `fs` | `read`, `write`, `append`, `exists`, `delete`, `list_dir`, `mkdir`, plus the `_bytes` forms |
| `use std::time` | `time` | `now`, `now_ms`, `sleep`, `local` |
| `use std::http` | `http` | `get`, `post`, `put`, `delete`, `head`, `get_bytes`, `post_bytes` |
| `use std::uhttp` | `uhttp` | The same API over a Unix domain socket, plus `stream` |
| `use std::sh` | `sh` | `exec`, `run`, `output` |
| `use std::json` | `json` | `parse`, `stringify`, `stringify_pretty` |
| `use std::env` | `env` | `get`, `set`, `args`, `cwd` |
| `use std::path` | `path` | `join`, `basename`, `dirname`, `ext`, `stem`, `abs`, `is_abs` |
| `use std::random` | `random` | `int`, `float`, `choice`, `shuffle`, `seed` |

See the [Standard Library](stdlib) reference for full API documentation.

There is no `llm` import. Running inference is language syntax, written `?p` or `?p |> Type`, rather than a package. See [LLM Integration](llm).

## Selective Imports (`from … use`)

The `from <package> use <names>` form brings specific names from a package straight into scope, with no package prefix. It uses the same `::` notation that `use` does.

```jade
from std::math use floor, ceil, sqrt

let a = floor(3.7)   // 3
let b = sqrt(16.0)   // 4.0
```

As with `use`, the string-literal form is a compile-time error, so `from "std/math" use floor` is rejected. Use `::` notation.

## Dependencies

External packages are declared in `jade.toml` and imported by their bare name. See [Packages](packages) for the full workflow.

```toml
[dependencies.fastmath]
version = "1.2.0"
url     = "https://example.com/fastmath-{platform}.so"
```

```jade
use fastmath

print(fastmath.triple(14))
```

A dependency resolves through the same machinery as a registered library, so it behaves the same way in `jade run` and `jade build`. If a project declares both a dependency and a `[lib]` entry under one name, the local `[lib]` wins and Jade warns you. If a bare name matches both a dependency and a sibling `.jde` file, that is a hard error rather than a silent choice. Rename one of them.

A dependency is always *one name binding one artifact*, no matter how many files went into building it. `use fastmath` reaches the package's exported functions and nothing more. There is no `fastmath::submodule`, because the package's internal structure did not survive compilation. That is the difference between a dependency and a `[lib]`. A `[lib]` registers a *directory* whose modules you address one at a time, while a package is a single compiled unit.
