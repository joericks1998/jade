---
id: imports
title: Imports
sidebar_label: Imports
---

Jade's `use` statement loads another `.jde` file or a standard-library package and makes its definitions available in the importing file. There is **one import form**: a `use` statement names a **module** with `::` notation (or a bare name), and the import binds under the module's last path segment. There are no quoted file paths and no `as` alias.

```jade
use utils          // a sibling ./utils.jde        → binds `utils`
use sub::helper    // ./sub/helper.jde             → binds `helper`
use std::math      // a standard-library package   → binds `math`
use mylib::shapes  // a registered [lib] module     → binds `shapes`
use fastmath       // an installed dependency        → binds `fastmath`
```

- The `use` statement must appear at the top level (not inside a function body).
- The imported file is executed once; its definitions are reachable through the bound name.
- The bound name is always the **last segment** of the path (`sub::helper` → `helper`). To bind a different name, rename the file.

## Local files by name

A bare name resolves to a **sibling `.jde` file**; a `::` path descends into subdirectories. Resolution is always relative to the directory of the *importing* file.

**math_lib.jde** — a sibling of the importer:

```jade
fn add(a, b) { return a + b }
fn mul(a, b) { return a * b }
```

**main.jde** — the importer:

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
`::` descends into subdirectories only. A **parent** or cross-directory import (`../shared/util.jde`) is not expressible as a module path — register those directories as a **`[lib]`** (below), which anchors resolution at the project root. Absolute paths are never supported.
:::

## Library Imports (`[lib]`)

Bare/`::` names only reach *down* from the importing file, so sharing a module across a deep tree (a `src/utils/` used from several directories) isn't expressible as a plain module path. To import a module from anywhere in a project, register a **library** in `jade.toml`: a named directory, optionally with an allowlist of its modules.

```toml
# jade.toml
[project]
name = "myapp"

[lib.utils]
path  = "src/utils"             # directory, relative to the project root
files = ["math.jde", "io.jde"]  # optional: allowlist of importable filenames
```

`files` is optional:

- **Omit it** to make every recognized file in the directory importable.
- **List filenames** (with extension) to restrict imports to that allowlist.

A module's **file extension decides how it loads**:

- `.jde` → a Jade source module.
- `.dylib` / `.so` → a **native** C-ABI shared library (e.g. a Rust crate built as `cdylib`), loaded over the `jade_pkg_init` FFI and bound as a dict of functions.

```toml
[lib.ext]
path  = "lib"
files = ["math.jde", "fastmath.dylib"]   # one Jade module, one native library
```

```jade
use ext::math       // -> lib/math.jde     (Jade)
use ext::fastmath   // -> lib/fastmath.dylib (native), then fastmath.some_fn(...)
```

Native libraries work in both backends: the interpreter loads them with `dlopen`, and an AOT binary `dlopen`s them at startup and dispatches through `jrt_native_call`. The same `.so` serves both.

Then import the module with **`::` notation** from **any** file in the project — the path is resolved against the library's directory anchored at the project root, not the importing file. The import binds to its last segment automatically (no `as` needed):

```jade
use utils::math               // -> <root>/src/utils/math.jde, from anywhere
print(math.square(5))        // binds as `math` (the last segment)
```

Rules:

- A `use` path is a **library reference** when its first segment names a registered library; otherwise it resolves as a relative module (sibling file / subdirectory). Both bind the last segment — no alias.
- With a `files` allowlist, importing an unlisted module is a hard error in both `jade run` and `jade build`. Without one, a missing file is a normal not-found error.
- Library resolution is identical in the VM and the native (AOT) build.

## What Gets Imported

The imported module's top-level functions, variables, and struct definitions are reachable through the bound name (`<name>.<member>`). The imported file runs to completion before execution of the importing file continues past the `use` statement.

| Exported from `mathlib.jde` (`use mathlib`) | Available after `use` |
|----------------------------------------------|-----------------------|
| `fn add(a, b) { … }` | `mathlib.add(2, 3)` works |
| `let PI = 3.14159` | `mathlib.PI` is in scope |
| `struct Point { x, y }` | `mathlib.Point { x: 1, y: 2 }` works |

## Multiple Imports

A file may contain multiple `use` statements, processed in order.

```jade
use mathlib
use stringlib

let n = mathlib.add(1, 2)
let greeting = stringlib.concat("hello", " world")
```

## No Re-export

Imports are not re-exported. If `a.jde` uses `b.jde`, a third file that uses `a.jde` does *not* automatically get access to what `b.jde` defined. Each file must import the libraries it needs directly.

---

## Standard Library Packages

Jade's standard library is imported with **`::` notation** — `use std::json`, not a quoted path. These packages are always available; no installation required.

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

Importing a package binds it as a global variable named after the package (`math`, `json`, `path`, etc.). The table below lists all available packages.

:::warning
Quoted-string imports of any kind — `use "std/math"`, `use "lib.jde" as lib` — are **rejected at compile time** (`QuotedImport`), as is the `as` alias (`ImportAlias`). Always name a module with `::` notation: `use std::math`, `use utils`.
:::

| Import | Global | Summary |
|--------|--------|---------|
| `use std::math` | `math` | `floor`, `ceil`, `abs`, `sqrt`, `min`, `max`, `pow` |
| `use std::string` | `string` | `split`, `upper`, `lower`, `trim`, `contains`, `replace`, `starts_with`, `ends_with` |
| `use std::array` | `array` | `map`, `filter`, `sort`, `reverse` (higher-order; non-mutating) |
| `use std::dict` | `dict` | `keys`, `values`, `has`, `get`, `merge` |
| `use std::fs` | `fs` | `read`, `write`, `append`, `exists`, `delete`, `list_dir`, `mkdir` |
| `use std::time` | `time` | `now`, `now_ms`, `sleep`, `local` |
| `use std::http` | `http` | `get`, `post`, `put`, `delete`, `head` |
| `use std::sh` | `sh` | `exec`, `run`, `output` |
| `use std::json` | `json` | `parse`, `stringify`, `stringify_pretty` |
| `use std::env` | `env` | `get`, `set`, `args`, `cwd` |
| `use std::path` | `path` | `join`, `basename`, `dirname`, `ext`, `stem`, `abs`, `is_abs` |
| `use std::random` | `random` | `int`, `float`, `choice`, `shuffle`, `seed` |

See the [Standard Library](stdlib) reference for full API documentation.

There is no `llm` import — running inference is language syntax (`?p`,
`?p |> Type`), not a package. See [LLM Integration](llm).

## Selective Imports (`from … use`)

The `from <package> use <names>` form imports specific names from a package directly into scope, without the package prefix. It uses the same `::` notation as `use`.

```jade
from std::math use floor, ceil, sqrt

let a = floor(3.7)   // 3
let b = sqrt(16.0)   // 4.0
```

As with `use`, the string-literal form (`from "std/math" use floor`) is a compile-time error — use `::` notation.

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

A dependency resolves through the same `[lib]` machinery as a registered library, so it behaves identically in `jade run` and `jade build`. If a project declares both a dependency and a `[lib]` entry of the same name, the local `[lib]` wins and Jade warns. If a bare name matches both a dependency and a sibling `.jde` file, that is a hard error rather than a silent choice — rename one of them.
