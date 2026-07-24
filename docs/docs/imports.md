---
id: imports
title: Imports
sidebar_label: Imports
---

Jade's `use` statement loads another `.jde` file or a standard-library package and makes its definitions available in the importing file. There are two import forms with different syntax:

- **File imports** use a quoted path and **require an alias**: `use "lib.jde" as lib`.
- **Package imports** (standard library) use **`::` notation**: `use std::math`.

## File Imports

```jade
use "<path>" as <name>
```

- `<path>` — a relative path to another `.jde` file, resolved relative to the directory of the importing file.
- `<name>` — the alias the imported module is bound to. The alias is **required**; a bare `use "lib.jde"` without `as <name>` is a compile-time error (`MissingImportAlias`).
- The `use` statement must appear at the top level (not inside a function body).
- The imported file is executed once; its definitions are reachable through the alias.

## Basic Example

**math_lib.jde** — the library:

```jade
fn add(a, b) { return a + b }
fn mul(a, b) { return a * b }
```

**main.jde** — the importer:

```jade
use "math_lib.jde" as math_lib

let x = math_lib.add(2, 3)   // 5
let y = math_lib.mul(4, 5)   // 20
```

After the `use` statement executes, the imported module's functions are reachable through the alias (`math_lib.add`, `math_lib.mul`). They can be called, passed as values, or stored in variables.

## Path Resolution

Paths in `use` are resolved relative to the directory containing the *importing* file, not the directory from which `jade` was invoked.

```jade
// If your project layout is:
//   project/
//     main.jde
//     lib/
//       utils.jde

// Inside main.jde:
use "lib/utils.jde" as utils
```

:::note
Absolute paths are not supported. Always use paths relative to the importing file's location.
:::

## Library Imports (`[lib]`)

Relative paths get awkward across a deep project tree (`use "../../shared/util.jde"`). To import a module from anywhere in a project, register a **library** in `jade.toml`: a named directory, optionally with an allowlist of its modules.

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

- **`::` notation names a module** — a stdlib package (`use std::math`) or a registered library (`use utils::math`). It binds the last segment and needs no alias. **String notation names a file path** (`use "lib/helper.jde" as h`) and still requires an alias.
- With a `files` allowlist, importing an unlisted module is a hard error in both `jade run` and `jade build`. Without one, a missing file is a normal not-found error.
- `::` notation is treated as a library reference only when its first segment names a registered library; plain relative file imports (string form) keep working unchanged.
- Library resolution is identical in the VM and the native (AOT) build.

## What Gets Imported

The imported module's top-level functions, variables, and struct definitions are reachable through the alias (`<alias>.<name>`). The imported file runs to completion before execution of the importing file continues past the `use` statement.

| Exported from `lib.jde` (imported `as lib`) | Available after `use` |
|----------------------------------------------|-----------------------|
| `fn add(a, b) { … }` | `lib.add(2, 3)` works |
| `let PI = 3.14159` | `lib.PI` is in scope |
| `struct Point { x, y }` | `lib.Point { x: 1, y: 2 }` works |

## Multiple Imports

A file may contain multiple `use` statements. Each is processed in order; each file import must have its own alias.

```jade
use "math_lib.jde" as m
use "string_lib.jde" as s

let n = m.add(1, 2)
let greeting = s.concat("hello", " world")
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
The string-literal form for stdlib packages — `use "std/math"` — is **rejected at compile time** (`StdlibStringImport`). Always use `::` notation: `use std::math`. Quoted paths are reserved for file imports, which require an alias.
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

A dependency resolves through the same `[lib]` machinery as a registered library, so it behaves identically in `jade run` and `jade build`. If a project declares both a dependency and a `[lib]` entry of the same name, the local `[lib]` wins and Jade warns. If a bare name matches both a dependency and a sibling `.jde` file, that is a hard error rather than a silent choice — rename one, or import the file explicitly with `use "name.jde" as name`.
