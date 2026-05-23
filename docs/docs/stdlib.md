---
id: stdlib
title: Standard Library
sidebar_label: Standard Library
---

Jade ships a standard library of built-in packages. Import any package with `use "std/<name>"` and it becomes a global variable in scope for the rest of the file.

```jade
use "std/json"
use "std/path"
use "std/random"

let data = json.parse('{"x": 1}')
let p = path.join("/home/user", "projects", "app")
let n = random.int(1, 100)
```

| Import string | Global name | Description |
|---------------|-------------|-------------|
| `use "std/math"` | `math` | Numeric functions |
| `use "std/string"` | `string` | String utilities |
| `use "std/array"` | `array` | Higher-order array functions |
| `use "std/dict"` | `dict` | Dict utilities |
| `use "std/fs"` | `fs` | File system I/O |
| `use "std/time"` | `time` | Clock and sleep |
| `use "std/http"` | `http` | HTTP client |
| `use "std/sh"` | `sh` | Shell command execution |
| `use "std/json"` | `json` | JSON encode / decode |
| `use "std/env"` | `env` | Environment variables and process info |
| `use "std/path"` | `path` | Path manipulation |
| `use "std/random"` | `random` | Random number generation |
| `use "llm"` | `llm` | LLM runtime configuration |

---

## `std/math`

```jade
use "std/math"
```

| Function | Returns | Description |
|----------|---------|-------------|
| `math.floor(x)` | `int` | Largest integer ≤ x |
| `math.ceil(x)` | `int` | Smallest integer ≥ x |
| `math.abs(x)` | same as input | Absolute value (int or float) |
| `math.sqrt(x)` | `float` | Square root |
| `math.min(a, b)` | number | Smaller of two numbers |
| `math.max(a, b)` | number | Larger of two numbers |
| `math.pow(base, exp)` | `float` | base raised to exp |

```jade
use "std/math"

print(math.floor(3.7))    // 3
print(math.ceil(3.2))     // 4
print(math.abs(-5))       // 5
print(math.sqrt(16.0))    // 4.0
print(math.min(3, 7))     // 3
print(math.max(3, 7))     // 7
print(math.pow(2.0, 10.0)) // 1024.0
```

---

## `std/string`

```jade
use "std/string"
```

The `std/string` package exposes functions that take the target string as the first argument. The same operations are also available as **primitive methods** directly on any `str` value — see the method form in the table below.

| Function | Method form | Returns | Description |
|----------|-------------|---------|-------------|
| `string.split(s, sep)` | `s.split(sep)` | `array` | Split `s` on `sep`; returns array of str |
| `string.upper(s)` | `s.upper()` | `str` | Uppercase |
| `string.lower(s)` | `s.lower()` | `str` | Lowercase |
| `string.trim(s)` | `s.trim()` | `str` | Strip leading and trailing whitespace |
| `string.contains(s, sub)` | `s.contains(sub)` | `bool` | True if `sub` appears in `s` |
| `string.replace(s, from, to)` | `s.replace(from, to)` | `str` | Replace first occurrence of `from` with `to` |
| `string.starts_with(s, prefix)` | `s.starts_with(prefix)` | `bool` | True if `s` starts with `prefix` |
| `string.ends_with(s, suffix)` | `s.ends_with(suffix)` | `bool` | True if `s` ends with `suffix` |

`str` also has a `len()` method (no package import needed):

```jade
let s = "Hello, Jade!"
print(s.len())                          // 12
print(s.upper())                        // HELLO, JADE!
print(s.lower())                        // hello, jade!
print(s.trim())                         // Hello, Jade!  (no change here)
print(s.split(", "))                    // ["Hello" "Jade!"]
print(s.contains("Jade"))              // true
print(s.replace("Jade", "World"))      // Hello, World!
print(s.starts_with("Hello"))          // true
print(s.ends_with("!"))                // true
```

---

## `std/array`

```jade
use "std/array"
```

The `std/array` package adds higher-order functions (`map`, `filter`) that are not available as primitive methods, plus standalone versions of `sort` and `reverse` that return new arrays.

| Function | Description |
|----------|-------------|
| `array.map(arr, fn)` | Apply `fn` to each element; return new array of results |
| `array.filter(arr, fn)` | Keep elements for which `fn` returns true; return new array |
| `array.sort(arr)` | Return a new sorted copy of `arr` (does not mutate) |
| `array.reverse(arr)` | Return a new reversed copy of `arr` (does not mutate) |

**Primitive methods** (available without import):

| Method | Returns | Description |
|--------|---------|-------------|
| `arr.len()` | `int` | Number of elements |
| `arr.push(x)` | `nil` | Append `x` in place |
| `arr.pop()` | value | Remove and return the last element |
| `arr.contains(x)` | `bool` | True if `x` is in the array |
| `arr.sort()` | `nil` | Sort in place |
| `arr.reverse()` | `nil` | Reverse in place |

```jade
use "std/array"

let nums = [3, 1, 4, 1, 5, 9]

// Higher-order functions
let doubled  = array.map(nums, |x| x * 2)
let evens    = array.filter(nums, |x| x % 2 == 0)
let sorted   = array.sort(nums)     // new copy, nums unchanged
let rev      = array.reverse(nums)  // new copy

print(doubled)   // [6, 2, 8, 2, 10, 18]
print(evens)     // [4]
print(sorted)    // [1, 1, 3, 4, 5, 9]
print(rev)       // [9, 5, 1, 4, 1, 3]

// Primitive methods (mutate in place)
nums.push(2)
let last = nums.pop()   // 2
print(nums.contains(4)) // true
nums.sort()
print(nums)             // [1, 1, 3, 4, 5, 9]
```

:::note
`arr.sort()` and `arr.reverse()` mutate the array in place and return `nil`. `array.sort(arr)` and `array.reverse(arr)` return a **new** array and leave the original unchanged.
:::

---

## `std/dict`

```jade
use "std/dict"
```

The `std/dict` package adds a `merge` function plus standalone versions of the primitive dict methods.

| Function | Description |
|----------|-------------|
| `dict.keys(d)` | Return all keys as an array |
| `dict.values(d)` | Return all values as an array |
| `dict.has(d, key)` | True if `key` is present |
| `dict.get(d, key)` | Value at `key`, or `nil` if absent |
| `dict.merge(d1, d2)` | Return new dict combining both; `d2` wins on duplicate keys |

**Primitive methods** (available without import):

| Method | Returns | Description |
|--------|---------|-------------|
| `d.len()` | `int` | Number of key-value pairs |
| `d.keys()` | `array` | All keys |
| `d.values()` | `array` | All values |
| `d.has(key)` | `bool` | True if key exists |
| `d.get(key)` | value \| nil | Value at key, or nil if missing |

```jade
use "std/dict"

let a = {"x": 1, "y": 2}
let b = {"y": 99, "z": 3}

let merged = dict.merge(a, b)  // {"x": 1, "y": 99, "z": 3}
print(merged["y"])             // 99

// Primitive methods
let keys = a.keys()            // ["x", "y"]
print(a.has("x"))              // true
print(a.get("missing"))        // nil
```

---

## `std/fs`

```jade
use "std/fs"
```

| Function | Returns | Description |
|----------|---------|-------------|
| `fs.read(path)` | `str` | Read entire file as a string |
| `fs.write(path, content)` | `nil` | Write string to file, creating or overwriting |
| `fs.append(path, content)` | `nil` | Append string to file (creates if absent) |
| `fs.exists(path)` | `bool` | True if path exists (file or directory) |
| `fs.delete(path)` | `nil` | Delete a file |
| `fs.list_dir(path)` | `array` | List entries in a directory (names only, not full paths) |
| `fs.mkdir(path)` | `nil` | Create directory (and all parents) |

```jade
use "std/fs"

fs.write("hello.txt", "Hello, world!\n")
let content = fs.read("hello.txt")
print(content)                       // Hello, world!

print(fs.exists("hello.txt"))        // true
print(fs.exists("no_such_file.txt")) // false

let entries = fs.list_dir(".")
for entry in entries {
    print(entry)
}

fs.mkdir("output/logs")
fs.append("output/logs/run.log", "started\n")
fs.delete("hello.txt")
```

:::note
`fs.read` raises an `IoError` if the file does not exist. Use `fs.exists` to check first when the file may be absent.
:::

---

## `std/time`

```jade
use "std/time"
```

| Function | Returns | Description |
|----------|---------|-------------|
| `time.now()` | `int` | Current Unix timestamp in seconds |
| `time.now_ms()` | `int` | Current Unix timestamp in milliseconds |
| `time.sleep(secs)` | `nil` | Block execution for `secs` seconds (int or float) |

```jade
use "std/time"

let start = time.now_ms()
time.sleep(0.1)
let elapsed = time.now_ms() - start
print(f"elapsed: {elapsed}ms")   // elapsed: ~100ms
```

---

## `std/http`

```jade
use "std/http"
```

All HTTP functions return a `dict` with two keys:

| Key | Type | Description |
|-----|------|-------------|
| `status` | `int` | HTTP status code (e.g. `200`) |
| `body` | `str` | Response body as a string |

An optional `headers` dict may be passed as the last argument to any function. Keys and values must be strings.

| Function | Description |
|----------|-------------|
| `http.get(url, headers?)` | HTTP GET |
| `http.post(url, body, headers?)` | HTTP POST with string body |
| `http.put(url, body, headers?)` | HTTP PUT with string body |
| `http.delete(url, headers?)` | HTTP DELETE |
| `http.head(url, headers?)` | HTTP HEAD (body will be empty) |

```jade
use "std/http"
use "std/json"

// Simple GET
let resp = http.get("https://api.example.com/status")
print(resp["status"])   // 200
print(resp["body"])

// POST with JSON body and headers
let payload = json.stringify({"name": "jade"})
let resp2 = http.post(
    "https://api.example.com/items",
    payload,
    {"Content-Type": "application/json", "Authorization": "Bearer sk-..."}
)
let result = json.parse(resp2["body"])
```

:::note
Requests time out after 30 seconds. HTTP errors (non-2xx status) do **not** raise — check `resp["status"]` yourself. Network errors (DNS failure, connection refused, etc.) raise an `IoError`.
:::

---

## `std/sh`

```jade
use "std/sh"
```

All three functions run commands through `sh -c`, so shell features like pipes, redirection, and globbing work.

| Function | Returns | Description |
|----------|---------|-------------|
| `sh.exec(cmd)` | `str` | Run command, return stdout (trailing newline stripped). Raises if exit code is non-zero. |
| `sh.run(cmd)` | `int` | Run command with inherited stdio. Returns exit code. Never raises. |
| `sh.output(cmd)` | `dict` | Capture all output. Returns `{stdout: str, stderr: str, code: int}`. Never raises. |

```jade
use "std/sh"

// exec — great for capturing output of simple commands
let branch = sh.exec("git rev-parse --abbrev-ref HEAD")
print(f"current branch: {branch}")

// run — when you want the command's output to go directly to the terminal
let code = sh.run("npm test")
if code != 0 {
    raise "tests failed"
}

// output — when you need full control
let result = sh.output("ls -la nonexistent 2>&1")
print(result["code"])    // 1 or 2
print(result["stderr"])  // ls: cannot access...
```

:::warning
`sh.exec` raises an `IoError` if the command exits with a non-zero code. Use `sh.run` or `sh.output` when you expect failure or need to inspect the exit code.
:::

---

## `std/json`

```jade
use "std/json"
```

| Function | Returns | Description |
|----------|---------|-------------|
| `json.parse(s)` | value | Parse a JSON string into a Jade value |
| `json.stringify(val)` | `str` | Serialize a Jade value to compact JSON |
| `json.stringify_pretty(val)` | `str` | Serialize to indented (pretty-printed) JSON |

**Type mapping:**

| JSON type | Jade type |
|-----------|-----------|
| `null` | `nil` |
| `true` / `false` | `bool` |
| integer number | `int` |
| floating-point number | `float` |
| string | `str` |
| array | `array` |
| object | `dict` |

```jade
use "std/json"

// Parse
let data = json.parse('{"name": "jade", "version": 1, "stable": true}')
print(data["name"])     // jade
print(data["version"])  // 1

// Serialize
let compact = json.stringify(data)
print(compact)          // {"name":"jade","stable":true,"version":1}

let pretty = json.stringify_pretty(data)
print(pretty)
// {
//   "name": "jade",
//   "stable": true,
//   "version": 1
// }

// Round-trip
let arr = [1, 2, {"x": 3}]
let back = json.parse(json.stringify(arr))
print(back[2]["x"])     // 3
```

:::note
`json.parse` raises an `IoError` if the input is not valid JSON. Numbers with a decimal point become `float`; numbers without one become `int`.
:::

---

## `std/env`

```jade
use "std/env"
```

| Function | Returns | Description |
|----------|---------|-------------|
| `env.get(name)` | `str` \| `nil` | Value of environment variable `name`, or `nil` if unset |
| `env.set(name, value)` | `nil` | Set environment variable `name` to `value` |
| `env.args()` | `array` | Command-line arguments (including the program name as `args[0]`) |
| `env.cwd()` | `str` | Current working directory as an absolute path |

```jade
use "std/env"

// Read an env var with a fallback
let key = env.get("API_KEY")
if key == nil {
    raise "API_KEY is not set"
}

// Set a variable (visible to child processes spawned via std/sh)
env.set("DEBUG", "1")

// Inspect command-line arguments
let args = env.args()
print(f"running: {args[0]}")
if args.len() > 1 {
    print(f"first arg: {args[1]}")
}

// Working directory
print(env.cwd())   // /home/user/myproject
```

---

## `std/path`

```jade
use "std/path"
```

| Function | Returns | Description |
|----------|---------|-------------|
| `path.join(base, part, ...)` | `str` | Join two or more path segments with the OS separator |
| `path.basename(p)` | `str` | Filename with extension (last path component) |
| `path.dirname(p)` | `str` | Parent directory; `"."` for a bare filename |
| `path.ext(p)` | `str` \| `nil` | File extension including the dot (e.g. `".rs"`), or `nil` if none |
| `path.stem(p)` | `str` | Filename without extension |
| `path.abs(p)` | `str` | Absolute path (resolves relative to cwd; path need not exist) |
| `path.is_abs(p)` | `bool` | True if the path is absolute |

```jade
use "std/path"

let p = path.join("/home/user", "projects", "app", "main.jde")
print(p)                    // /home/user/projects/app/main.jde

print(path.basename(p))     // main.jde
print(path.dirname(p))      // /home/user/projects/app
print(path.ext(p))          // .jde
print(path.stem(p))         // main
print(path.is_abs(p))       // true

let rel = "src/main.jde"
print(path.dirname(rel))    // src
print(path.is_abs(rel))     // false
print(path.abs(rel))        // /current/working/dir/src/main.jde
```

:::note
`path.join` accepts two or more arguments. If any component is an absolute path it resets the result (same behaviour as Python's `os.path.join`). `path.abs` does not resolve symlinks and does not require the path to exist.
:::

---

## `std/random`

```jade
use "std/random"
```

Jade uses a single global RNG (seeded from OS entropy at first use). Calling `random.seed` replaces it with a deterministic seed.

| Function | Returns | Description |
|----------|---------|-------------|
| `random.int(min, max)` | `int` | Uniformly random integer in `[min, max]` inclusive |
| `random.float()` | `float` | Uniformly random float in `[0.0, 1.0)` |
| `random.choice(arr)` | value | Random element from an array. Raises if empty. |
| `random.shuffle(arr)` | `nil` | Shuffle array in place (Fisher-Yates) |
| `random.seed(n)` | `nil` | Reseed the global RNG with integer `n` for reproducible output |

```jade
use "std/random"

// Reproducible output
random.seed(42)

let n = random.int(1, 6)          // simulated die roll, 1–6
print(n)

let f = random.float()            // 0.0 ≤ f < 1.0
print(f)

let items = ["rock", "paper", "scissors"]
let pick = random.choice(items)   // random element
print(pick)

let deck = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
random.shuffle(deck)              // in-place shuffle
print(deck)
```

---

## `llm` (LLM configuration)

```jade
use "llm"
```

The `llm` package exposes runtime controls for LLM inference. See [LLM Integration](llm) for the full reference on prompts, typed dereferences, and configuration.

| Function | Returns | Description |
|----------|---------|-------------|
| `llm.set_max_tokens(n)` | `nil` | Cap model responses at `n` tokens for all subsequent `?` calls |

```jade
use "llm"

llm.set_max_tokens(128)

prompt p = "Write a one-sentence summary of Jade."
let summary = ?p
print(summary)
```
