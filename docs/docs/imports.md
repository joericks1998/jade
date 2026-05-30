---
id: imports
title: Imports
sidebar_label: Imports
---

Jade's `use` statement loads another `.jde` file and makes its top-level function and variable definitions available in the importing file.

## Syntax

```jade
use "<path>"
```

- `<path>` — a relative path to another `.jde` file, resolved relative to the directory of the importing file.
- The `use` statement must appear at the top level (not inside a function body).
- The imported file is executed once; all top-level bindings it produces become available in the importing scope.

## Basic Example

**math_lib.jde** — the library:

```jade
fn add(a, b) { return a + b }
fn mul(a, b) { return a * b }
```

**main.jde** — the importer:

```jade
use "math_lib.jde"

let x = add(2, 3)   // 5
let y = mul(4, 5)   // 20
```

After the `use` statement executes, `add` and `mul` are defined in the importing scope exactly as if they had been written inline. They can be called, passed as values, or stored in variables.

## Path Resolution

Paths in `use` are resolved relative to the directory containing the *importing* file, not the directory from which `jade` was invoked.

```jade
// If your project layout is:
//   project/
//     main.jde
//     lib/
//       utils.jde

// Inside main.jde:
use "lib/utils.jde"
```

:::note
Absolute paths are not supported. Always use paths relative to the importing file's location.
:::

## Library Imports (`[lib]`)

Relative paths get awkward across a deep project tree (`use "../../shared/util.jde"`). To import a module from anywhere in a project, register a **library** in `jade.toml`: a named directory plus an explicit allowlist of its modules.

```toml
# jade.toml
[project]
name = "myapp"

[lib.utils]
path  = "src/utils"          # directory, relative to the project root
files = ["math", "strings"]  # importable module stems (no .jde extension)
```

Then import `<lib>/<module>` from **any** file in the project — the path is resolved against the library's directory anchored at the project root, not the importing file:

```jade
use "utils/math" as m        // -> <root>/src/utils/math.jde, from anywhere
print(m.square(5))
```

Rules:

- Only modules listed in `files` are importable. `use "utils/secret"` (unregistered) is a hard error in both `jade run` and `jade build`.
- A `use` path is treated as a library reference only when its first segment names a registered library; everything else still resolves as a normal relative path (so existing relative imports keep working).
- Library resolution is identical in the VM and the native (AOT) build.

## What Gets Imported

All top-level bindings in the imported file are brought into scope — including functions, variables, and struct definitions. The imported file runs to completion before execution of the importing file continues past the `use` statement.

| Exported from lib | Available after `use` |
|-------------------|-----------------------|
| `fn add(a, b) { … }` | `add(2, 3)` works |
| `let PI = 3.14159` | `PI` is in scope |
| `struct Point { x, y }` | `Point { x: 1, y: 2 }` works |

## Multiple Imports

A file may contain multiple `use` statements. Each is processed in order. If two imported files define the same name, the later import wins.

```jade
use "math_lib.jde"
use "string_lib.jde"

let n = add(1, 2)
let s = concat("hello", " world")
```

## No Re-export

Imports are not re-exported. If `a.jde` uses `b.jde`, a third file that uses `a.jde` does *not* automatically get access to what `b.jde` defined. Each file must import the libraries it needs directly.

---

## Standard Library Packages

Jade's standard library is also imported with `use`, but with a built-in string like `"std/json"` instead of a file path. These packages are always available — no installation required.

```jade
use "std/math"
use "std/json"
use "std/path"
use "std/random"

let n = math.sqrt(144.0)          // 12.0
let data = json.parse('{"x": 1}')
let p = path.join("src", "main.jde")
let roll = random.int(1, 6)
```

Importing a stdlib package binds it as a global variable named after the package (`math`, `json`, `path`, etc.). The table below lists all available packages.

| Import string | Global | Summary |
|---------------|--------|---------|
| `use "std/math"` | `math` | `floor`, `ceil`, `abs`, `sqrt`, `min`, `max`, `pow` |
| `use "std/string"` | `string` | `split`, `upper`, `lower`, `trim`, `contains`, `replace`, `starts_with`, `ends_with` |
| `use "std/array"` | `array` | `map`, `filter`, `sort`, `reverse` (higher-order; non-mutating) |
| `use "std/dict"` | `dict` | `keys`, `values`, `has`, `get`, `merge` |
| `use "std/fs"` | `fs` | `read`, `write`, `append`, `exists`, `delete`, `list_dir`, `mkdir` |
| `use "std/time"` | `time` | `now`, `now_ms`, `sleep` |
| `use "std/http"` | `http` | `get`, `post`, `put`, `delete`, `head` |
| `use "std/sh"` | `sh` | `exec`, `run`, `output` |
| `use "std/json"` | `json` | `parse`, `stringify`, `stringify_pretty` |
| `use "std/env"` | `env` | `get`, `set`, `args`, `cwd` |
| `use "std/path"` | `path` | `join`, `basename`, `dirname`, `ext`, `stem`, `abs`, `is_abs` |
| `use "std/random"` | `random` | `int`, `float`, `choice`, `shuffle`, `seed` |
| `use "llm"` | `llm` | `set_max_tokens` |

See the [Standard Library](stdlib) reference for full API documentation.
