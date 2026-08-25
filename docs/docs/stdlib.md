---
id: stdlib
title: Standard Library
sidebar_label: Standard Library
---

Jade ships a standard library of built-in packages. Import one with `::` notation, written `use std::<name>`, and it becomes a global variable in scope for the rest of the file.

```jade
use std::json
use std::path
use std::random

let data = json.parse('{"x": 1}')
let p = path.join("/home/user", "projects", "app")
let n = random.int(1, 100)
```

:::warning
Every import names a module, using `::` notation or a bare name. There are no quoted file paths and no `as` alias. Jade rejects both `use "std/json"` and `use "lib.jde" as lib` at compile time, with a `QuotedImport` or `ImportAlias` error. See [Imports](imports).
:::

| Import | Global name | Description |
|--------|-------------|-------------|
| `use std::math` | `math` | Numeric functions |
| `use std::string` | `string` | String utilities |
| `use std::array` | `array` | Higher-order array functions |
| `use std::dict` | `dict` | Dict utilities |
| `use std::fs` | `fs` | File system I/O |
| `use std::time` | `time` | Clocks, sleep, and calendar conversion |
| `use std::http` | `http` | HTTP client |
| `use std::uhttp` | `uhttp` | HTTP client over a Unix domain socket |
| `use std::sh` | `sh` | Shell command execution |
| `use std::json` | `json` | JSON encode / decode |
| `use std::env` | `env` | Environment variables and process info |
| `use std::path` | `path` | Path manipulation |
| `use std::random` | `random` | Random number generation |

---

## `std/math`

```jade
use std::math
```

| Function | Returns | Description |
|----------|---------|-------------|
| `math.floor(x)` | `int` | Largest integer ≤ x |
| `math.ceil(x)` | `int` | Smallest integer ≥ x |
| `math.abs(x)` | same as input | Absolute value (int or float) |
| `math.sqrt(x)` | `float` | Square root |
| `math.min(a, b)` | number | Smaller of two numbers |
| `math.max(a, b)` | number | Larger of two numbers |
| `math.pow(base, exp)` | number | base raised to exp; an int base and non-negative int exp stay an int |
| `math.round(x)` | `int` | Nearest integer; ties go away from zero, so `round(2.5)` is 3 |
| `math.trunc(x)` | `int` | Rounds toward zero. It differs from `floor` only for negative numbers |
| `math.sign(x)` | same as input | -1, 0 or 1. `sign(nan())` is NaN |
| `math.clamp(x, lo, hi)` | number | `x` held between the bounds; a reversed range answers `hi` |
| `math.ln(x)` | `float` | Natural log |
| `math.log2(x)` | `float` | Base-2 log |
| `math.log10(x)` | `float` | Base-10 log |
| `math.exp(x)` | `float` | e raised to x |
| `math.sin(x)` `math.cos(x)` `math.tan(x)` | `float` | Radians |
| `math.asin(x)` `math.acos(x)` `math.atan(x)` | `float` | Radians; NaN outside the domain |
| `math.atan2(y, x)` | `float` | Quadrant-correct angle. Note the argument order |
| `math.hypot(a, b)` | `float` | `sqrt(a² + b²)` without the intermediate overflow |
| `math.is_nan(x)` | `bool` | An int is never NaN |
| `math.is_inf(x)` | `bool` | An int is never infinite |
| `math.pi()` `math.e()` `math.tau()` | `float` | The constants |
| `math.inf()` `math.nan()` | `float` | The only way to reach these values, because the lexer caps a numeric literal |

Write the constants as calls, with the parentheses. Jade only reads a package namespace as a field that a call consumes right away, so `math.pi` on its own has no meaning yet.

```jade
use std::math

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
use std::string
```

The `std/string` package holds functions that take the target string as their first argument. The same operations are also available as *primitive methods* directly on any `str` value. The table below shows both forms.

| Function | Method form | Returns | Description |
|----------|-------------|---------|-------------|
| `string.split(s, sep)` | `s.split(sep)` | `array` | Split `s` on `sep`; returns array of str |
| `string.upper(s)` | `s.upper()` | `str` | Uppercase |
| `string.lower(s)` | `s.lower()` | `str` | Lowercase |
| `string.trim(s)` | `s.trim()` | `str` | Strip leading and trailing whitespace |
| `string.contains(s, sub)` | `s.contains(sub)` | `bool` | True if `sub` appears in `s` |
| `string.replace(s, from, to)` | `s.replace(from, to)` | `str` | Replace *every* occurrence of `from` with `to` |
| `string.starts_with(s, prefix)` | `s.starts_with(prefix)` | `bool` | True if `s` starts with `prefix` |
| `string.ends_with(s, suffix)` | `s.ends_with(suffix)` | `bool` | True if `s` ends with `suffix` |
| `string.trim_start(s)` | `s.trim_start()` | `str` | Strip leading whitespace only |
| `string.trim_end(s)` | `s.trim_end()` | `str` | Strip trailing whitespace only |
| `string.capitalize(s)` | `s.capitalize()` | `str` | First character upper, the rest lower |
| `string.is_empty(s)` | `s.is_empty()` | `bool` | True for the empty string |
| `string.index_of(s, sub)` | `s.index_of(sub)` | `int` | Character index of the first occurrence, or -1 |
| `string.last_index_of(s, sub)` | `s.last_index_of(sub)` | `int` | Character index of the last occurrence, or -1 |
| `string.count(s, sub)` | `s.count(sub)` | `int` | Non-overlapping occurrences; an empty `sub` is 0 |
| `string.repeat(s, n)` | `s.repeat(n)` | `str` | `n` copies; zero or negative gives `""` |
| `string.slice(s, start, end)` | `s.slice(start, end)` | `str` | Characters `[start, end)`, clamped rather than raising |
| `string.pad_start(s, width, pad)` | `s.pad_start(width, pad)` | `str` | Left-pad to `width` characters; never truncates |
| `string.pad_end(s, width, pad)` | `s.pad_end(width, pad)` | `str` | Right-pad to `width` characters |
| `string.lines(s)` | `s.lines()` | `array` | Split on newlines. A trailing newline yields no empty final element, which is the difference from `split("\n")` |

*Every index is a character index*, not a byte offset. It is the same unit `len()` counts and `s[i]` walks. So `"café!".index_of("!")` is 4.

`str` also has `len()` and `encode()`, and neither needs a package import. `encode()` gives the string's UTF-8 as a `bytes` value, and `bytes.decode()` converts back. See [Types](types#bytes).

```jade
let s = "Hello, Jade!"
print(s.len())                          // 12
print(s.encode().len())                 // 12 bytes, not characters
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
use std::array
```

The `std/array` package adds two higher-order functions, `map` and `filter`, which are not available as primitive methods. It also has standalone versions of `sort` and `reverse` that return new arrays.

| Function | Description |
|----------|-------------|
| `array.map(arr, fn)` | Apply `fn` to each element; return new array of results |
| `array.filter(arr, fn)` | Keep elements for which `fn` returns true; return new array |
| `array.sort(arr)` | Return a new sorted copy of `arr` (does not mutate) |
| `array.reverse(arr)` | Return a new reversed copy of `arr` (does not mutate) |
| `array.join(arr, sep)` | Join the elements into a `str`, separated by `sep`. Also `arr.join(sep)` |

*Primitive methods*, available with no import:

| Method | Returns | Description |
|--------|---------|-------------|
| `arr.len()` | `int` | Number of elements |
| `arr.push(x)` | `nil` | Append `x` in place |
| `arr.pop()` | value | Remove and return the last element |
| `arr.contains(x)` | `bool` | True if `x` is in the array |
| `arr.sort()` | `nil` | Sort in place |
| `arr.reverse()` | `nil` | Reverse in place |

```jade
use std::array

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
`arr.sort()` and `arr.reverse()` change the array in place and return `nil`. `array.sort(arr)` and `array.reverse(arr)` return a *new* array and leave the original alone.
:::

---

## `std/dict`

```jade
use std::dict
```

The `std/dict` package adds a `merge` function, plus standalone versions of the primitive dict methods.

| Function | Description |
|----------|-------------|
| `dict.keys(d)` | Return all keys as an array |
| `dict.values(d)` | Return all values as an array |
| `dict.has(d, key)` | True if `key` is present |
| `dict.get(d, key)` | Value at `key`, or `nil` if absent |
| `dict.merge(d1, d2)` | Return new dict combining both; `d2` wins on duplicate keys |

*Primitive methods*, available with no import:

| Method | Returns | Description |
|--------|---------|-------------|
| `d.len()` | `int` | Number of key-value pairs |
| `d.keys()` | `array` | All keys |
| `d.values()` | `array` | All values |
| `d.has(key)` | `bool` | True if key exists |
| `d.get(key)` | value \| nil | Value at key, or nil if missing |

```jade
use std::dict

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
use std::fs
```

| Function | Returns | Description |
|----------|---------|-------------|
| `fs.read(path)` | `str` | Read entire file as a string |
| `fs.read_bytes(path)` | `bytes` | The file as raw octets. Unlike `read`, works on binary. Tainted. |
| `fs.write_bytes(path, b)` | `nil` | Write raw octets, truncating |
| `fs.append_bytes(path, b)` | `nil` | Append raw octets |
| `fs.read_stdin_bytes()` | `bytes` | All of stdin as raw octets. Tainted. |
| `fs.write_stdout_bytes(b)` | `nil` | Raw octets to stdout, flushed |
| `fs.write(path, content)` | `nil` | Write string to file, creating or overwriting |
| `fs.append(path, content)` | `nil` | Append string to file (creates if absent) |
| `fs.exists(path)` | `bool` | True if path exists (file or directory) |
| `fs.delete(path)` | `nil` | Delete a file |
| `fs.list_dir(path)` | `array` | List entries in a directory (names only, not full paths) |
| `fs.mkdir(path)` | `nil` | Create directory (and all parents) |
| `fs.is_file(path)` | `bool` | True for a regular file. False for a directory, and false if absent |
| `fs.is_dir(path)` | `bool` | True for a directory. False if absent |
| `fs.size(path)` | `int` | Size in bytes. Raises if the path is absent |
| `fs.copy(src, dst)` | `nil` | Copy, creating or truncating `dst` |
| `fs.rename(src, dst)` | `nil` | Rename or move |
| `fs.rmdir(path)` | `nil` | Remove an *empty* directory. Deliberately not recursive |

`is_file` and `is_dir` answer questions, so a path that does not exist gives `false` rather than an error. That matches `fs.exists`. Everything else in this package raises a catchable error when it fails, on both engines.

```jade
use std::fs

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
`fs.read` raises an `IoError` if the file does not exist. Call `fs.exists` first when the file might be missing.
:::

---

## `std/time`

```jade
use std::time
```

| Function | Returns | Description |
|----------|---------|-------------|
| `time.now()` | `int` | Current Unix timestamp in seconds |
| `time.now_ms()` | `int` | Current Unix timestamp in milliseconds |
| `time.monotonic()` | `float` | Seconds from a fixed point in this process. Never jumps; only the difference between two readings means anything. |
| `time.sleep(secs)` | `nil` | Block execution for `secs` seconds (int or float) |
| `time.local(tz)` | `str` | Formatted local time string. Pass a timezone name (e.g. `"America/Denver"`) or `nil` for the system timezone. |
| `time.utc(ts)` | `str` | A timestamp as ISO 8601 UTC, e.g. `2026-08-16T14:03:22Z` |
| `time.parts(ts)` | `dict` | A timestamp broken into UTC calendar fields |
| `time.stamp(y, mo, d[, h[, mi[, s]]])` | `int` | UTC calendar fields back to a timestamp |

### Measuring how long something took

Use `time.monotonic()` rather than `time.now_ms()`. The wall clock can move while your program runs, because NTP corrects it and a person can set it by hand. Subtracting two readings of the wall clock can therefore give you a negative duration. The monotonic clock only ever moves forward.

```jade
use std::time

let start = time.monotonic()
time.sleep(0.1)
print(f"took {time.monotonic() - start}s")   // took ~0.1s
```

A monotonic reading has no meaning on its own, and two processes cannot compare their readings. When you want a moment in time rather than a duration, use `time.now()`.

### Calendar fields

`time.parts(ts)` splits a timestamp into a dict with eight keys, all `int`:

| Key | Range | |
|-----|-------|--|
| `year` | | |
| `month` | 1–12 | |
| `day` | 1–31 | |
| `hour` | 0–23 | |
| `minute` | 0–59 | |
| `second` | 0–59 | |
| `weekday` | 0–6 | 0 is Sunday, matching `date +%w` |
| `yearday` | 1–366 | matching `date +%j` |

```jade
use std::time

let p = time.parts(1786889002)
print(f"{p["year"]}-{p["month"]}-{p["day"]}")   // 2026-8-16
print(time.utc(1786889002))                     // 2026-08-16T14:03:22Z
```

`time.stamp` is the exact reverse of `time.parts`, so a round trip gives back what it started with. The three time-of-day arguments are optional and default to zero, so passing only three arguments means midnight.

```jade
use std::time

print(time.stamp(2026, 8, 16, 14, 3, 22))       // 1786889002
print(time.utc(time.stamp(2026, 8, 16)))        // 2026-08-16T00:00:00Z
```

A field outside its normal range *carries* into the next unit rather than failing, which is what turns date arithmetic into a single call. Month 13 is next January. Day 0 is the last day of the previous month. Adding 45 to a day crosses the end of the month on its own:

```jade
use std::time

print(time.utc(time.stamp(2026, 13, 1)))        // 2027-01-01T00:00:00Z
print(time.utc(time.stamp(2026, 3, 0)))         // 2026-02-28T00:00:00Z
print(time.utc(time.stamp(2026, 8, 16 + 45)))   // 2026-09-30T00:00:00Z
```

:::note These three are UTC, not local
`parts`, `stamp`, and `utc` all work in UTC. Converting to a local calendar needs the IANA timezone database, which Jade does not carry. `time.local(tz)` is the local-time answer, and it gives you a formatted string rather than separate fields.
:::

---

## `std/http`

```jade
use std::http
```

All HTTP functions return a `dict` with two keys:

| Key | Type | Description |
|-----|------|-------------|
| `status` | `int` | HTTP status code (e.g. `200`) |
| `body` | `str` | Response body as a string |

You can pass an optional `headers` dict as the last argument to any function. Its keys and values must be strings.

:::caution A `str` body is not safe for binary
A `str` is UTF-8 and NUL-terminated, so reading a response body as text loses two things. An invalid UTF-8 sequence becomes `�`, and *everything from the first NUL byte onward is dropped*. An image, an audio frame, and a gzip stream all hit both problems.

Use `get_bytes` or `post_bytes` for those. `.body` is then a `bytes` value, which holds any octet. Both spellings exist on `std::http` and `std::uhttp`, and both have worked under `jade run` and `jade build` since v1.2.5.
:::

| Function | Description |
|----------|-------------|
| `http.get(url, headers?)` | HTTP GET |
| `http.get_bytes(url, headers?)` | HTTP GET with an undecoded body (`.body` is `bytes`) |
| `http.post_bytes(url, body, headers?)` | HTTP POST sending raw octets |
| `http.post(url, body, headers?)` | HTTP POST with string body |
| `http.put(url, body, headers?)` | HTTP PUT with string body |
| `http.delete(url, headers?)` | HTTP DELETE |
| `http.head(url, headers?)` | HTTP HEAD (body will be empty) |

```jade
use std::http
use std::json

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
An HTTP error status, meaning anything outside the 200s, does *not* raise. Check `resp["status"]` yourself. Only a transport failure raises an `IoError`, which covers a DNS failure, a refused connection, a failed TLS handshake, and a timeout.

`std/http` runs requests through the `curl` binary, which has to be on your `PATH`. A missing `curl` is reported as a transport failure. Two things follow. There is no overall request timeout beyond curl's own defaults, and *redirects are not followed*, so a 301 comes back as `status` 301 with the redirect page as its body.
:::

---

## `std/uhttp`

```jade
use std::uhttp
```

This package speaks HTTP/1.1 over a *Unix domain socket* rather than a TCP host. Use it to talk to a local daemon, such as the Docker Engine API at `/var/run/docker.sock`, or another socket-backed operating system service. The API mirrors `std/http`. Every function returns the same `{status, body}` dict, and you can pass an optional `headers` dict as the last argument.

The target is a single pseudo-URL string of the form:

```text
unix://<socket-path>:<request-path>
```

The socket path runs up to the *first* `:` after the `unix://` scheme. Everything after that colon is the request path, so colons inside a query string survive. With no request path given, it defaults to `/`.

| Function | Description |
|----------|-------------|
| `uhttp.get(url, headers?)` | HTTP GET |
| `uhttp.get_bytes(url, headers?)` | HTTP GET with an undecoded body (`.body` is `bytes`) |
| `uhttp.post_bytes(url, body, headers?)` | HTTP POST sending raw octets |
| `uhttp.post(url, body, headers?)` | HTTP POST with string body |
| `uhttp.put(url, body, headers?)` | HTTP PUT with string body |
| `uhttp.delete(url, headers?)` | HTTP DELETE |
| `uhttp.head(url, headers?)` | HTTP HEAD (body will be empty) |
| `uhttp.stream(url, handler, headers?)` | Stream a long-lived response, calling `handler(line)` per line |

```jade
use std::uhttp
use std::json

// Query the Docker Engine API over its socket
let resp = uhttp.get("unix:///var/run/docker.sock:/v1.43/containers/json")
print(resp["status"])   // 200
let containers = json.parse(resp["body"])
```

### Streaming endpoints

`uhttp.stream` reads a response that stays open indefinitely, one line at a time. Docker's `/events`, its `/logs?follow=1`, and its image-pull progress are all this shape. It calls `handler(line)` for each newline-delimited line of the body, with the newline stripped, and returns the HTTP status code once the stream ends. It decodes the framing as it goes, so a `Transfer-Encoding: chunked` response works.

A handler that returns `false` *stops* the stream early and closes the socket. Any other return value continues it.

```jade
use std::uhttp
use std::json

// Follow the Docker event stream, printing each event's action
fn on_event(line) {
    let event = json.parse(line)
    print(event["Action"])
    // return false here to stop after the first event
}

uhttp.stream("unix:///var/run/docker.sock:/v1.43/events", on_event)
```

:::note
A one-shot request, such as `get` or `post`, times out after 30 seconds and sends `Connection: close`. `stream` keeps the connection open with no read timeout, because events can be sparse. Response framing honors `Content-Length` and `Transfer-Encoding: chunked`, which it de-chunks, and it reads to end of file when the connection closes. An HTTP error status does *not* raise, so check `resp["status"]` yourself. A missing socket, a malformed pseudo-URL, or a connection failure raises an `IoError`.
:::

---

## `std/sh`

```jade
use std::sh
```

All three functions run commands through `sh -c`, so shell features such as pipes, redirection, and globbing work.

| Function | Returns | Description |
|----------|---------|-------------|
| `sh.exec(cmd)` | `str` | Run command, return stdout (trailing newline stripped). Raises if exit code is non-zero. |
| `sh.run(cmd)` | `int` | Run command with inherited stdio. Returns exit code. Never raises. |
| `sh.output(cmd)` | `dict` | Capture all output. Returns `{stdout: str, stderr: str, code: int}`. Never raises. |

```jade
use std::sh

// exec is best for capturing the output of a simple command
let branch = sh.exec("git rev-parse --abbrev-ref HEAD")
print(f"current branch: {branch}")

// run sends the command's output straight to the terminal
let code = sh.run("npm test")
if code != 0 {
    raise "tests failed"
}

// output gives you full control
let result = sh.output("ls -la nonexistent 2>&1")
print(result["code"])    // 1 or 2
print(result["stderr"])  // ls: cannot access...
```

:::warning
`sh.exec` raises an `IoError` if the command exits with a non-zero code. Use `sh.run` or `sh.output` when you expect a failure, or when you need to read the exit code.
:::

### Untrusted strings cannot become commands

Jade tracks where a string came from. Anything read from outside the program is *tainted*, which covers a model reply, an HTTP body, a file, and stdin. The taint spreads through concatenation, f-strings, indexing, `.encode()`, and `.decode()`.

`sh.exec` and `sh.run` refuse a tainted command rather than running it:

```jade
use std::sh
use std::fs

let cmd = fs.read("command.txt")
sh.exec(cmd)
// raises: refused tainted string in sh.exec(cmd) — value derived from an
// untrusted source (LLM, network, file, stdin) and cannot flow to a
// code-execution sink
```

Either catch the error, or build the command from string literals and interpolate only the parts you have checked yourself.

The output of a shell command is itself tainted, so you cannot clean a value by running it through `sh` and feeding the result back in.

All three functions check, because all three reach the same `sh -c`. Until v1.3.3, `sh.output` did not check. That never limited what an untrusted command could do. It only meant the command had to be written as `sh.output(x).stdout`.

---

## `std/json`

```jade
use std::json
```

| Function | Returns | Description |
|----------|---------|-------------|
| `json.parse(s)` | value | Parse a JSON string into a Jade value |
| `json.stringify(val)` | `str` | Serialize a Jade value to compact JSON |
| `json.stringify_pretty(val)` | `str` | Serialize to indented (pretty-printed) JSON |

*Type mapping:*

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
use std::json

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
`json.parse` raises an `IoError` if the input is not valid JSON. A number with a decimal point becomes a `float`, and a number without one becomes an `int`.
:::

---

## `std/env`

```jade
use std::env
```

| Function | Returns | Description |
|----------|---------|-------------|
| `env.get(name)` | `str` \| `nil` | Value of environment variable `name`, or `nil` if unset |
| `env.set(name, value)` | `nil` | Set environment variable `name` to `value` |
| `env.args()` | `array` | Command-line arguments (including the program name as `args[0]`) |
| `env.cwd()` | `str` | Current working directory as an absolute path |

```jade
use std::env

// Read an env var with a fallback
let key = env.get("API_KEY")
if key == nil {
    raise "API_KEY is not set"
}

// Set a variable, which child processes started by std/sh will see
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

:::note What `args[0]` is
`env.args()` gives the *process's* argument list, so what you see depends on how the program was started.

A compiled binary run as `./app one two` gives `["./app", "one", "two"]`, which is the shape you would expect.

Under the interpreter, the process is `jade`, so the list starts with the jade binary and includes the subcommand. `jade run app.jde` gives `["…/jade", "run", "app.jde"]`. `jade run` also takes no arguments of its own beyond the file, so there is no way to pass any through it. The old shorthand does accept them, and `jade app.jde one two` gives `["…/jade", "app.jde", "one", "two"]`, but the positions still differ from a built program's.

Build the program when argument positions have to be stable.
:::

---

## `std/path`

```jade
use std::path
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
use std::path

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
`path.join` accepts two or more arguments. If any component is an absolute path, it resets the result, which is how Python's `os.path.join` behaves too. `path.abs` does not follow symlinks, and it does not require the path to exist.
:::

---

## `std/random`

```jade
use std::random
```

Jade uses one global random number generator, seeded from operating system entropy the first time you use it. Calling `random.seed` replaces that with a seed of your own, which makes the sequence repeatable.

| Function | Returns | Description |
|----------|---------|-------------|
| `random.int(min, max)` | `int` | Uniformly random integer in `[min, max]` inclusive |
| `random.float()` | `float` | Uniformly random float in `[0.0, 1.0)` |
| `random.choice(arr)` | value | Random element from an array. Raises if empty. |
| `random.shuffle(arr)` | `nil` | Shuffle array in place (Fisher-Yates) |
| `random.seed(n)` | `nil` | Reseed the global RNG with integer `n` for reproducible output |

```jade
use std::random

// Reproducible output
random.seed(42)

let n = random.int(1, 6)          // a simulated die roll, 1 to 6
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

## `std/bytes`

```jade
use std::bytes
```

Building a blob from nothing. Everything else that produces one reads it from somewhere: `str.encode()` converts text you already have, and `fs.read_bytes`, `http.get_bytes` and `uhttp.get_bytes` all read from outside the program. This package is how a program makes octets of its own, which is what you need to hand a pixel buffer, a header, or a mask to a C library.

`str.encode()` is not a substitute. A Jade string is UTF-8 and NUL-terminated, so a zero byte cuts it short and any value above 127 encodes as two octets rather than one. Neither is a defect; both are what a string *is*.

| Function | Returns | Description |
|----------|---------|-------------|
| `bytes.zeros(n)` | `bytes` | A buffer of `n` zeroed octets. Raises if `n` is negative. |
| `bytes.from_ints(arr)` | `bytes` | A blob from an array of ints. Each must be an octet, 0 to 255, or it raises and names the position. |
| `bytes.concat(a, b)` | `bytes` | A new blob holding `a` then `b`. Neither input changes. |

```jade
use std::bytes

let buf = bytes.zeros(4)
print(buf)                          // b"\x00\x00\x00\x00"

let px = bytes.from_ints([255, 0, 128, 64])
print(len(px))                      // 4
print(px[2])                        // 128

let whole = bytes.concat(px, buf)
print(len(whole))                   // 8
```

### Writing an octet

A blob is a buffer you can write into. Reading one octet was always spelled `b[i]`, so writing one is spelled `b[i] = v`, the same way an array works.

```jade
use std::bytes

let buf = bytes.zeros(4)
buf[0] = 255
buf[3] = 1
print(buf)                          // b"\xff\x00\x00\x01"
```

The value is an int from 0 to 255. Anything else raises, and so does an index past the end.

A blob is *reference-semantic*, like an array and unlike a dict. Two names for one buffer see the same write, and a function that writes into its argument changes what the caller still holds.

```jade
use std::bytes

fn fill(dst, value) {
    let i = 0
    while i < len(dst) {
        dst[i] = value
        i = i + 1
    }
    return dst
}

let buf = bytes.zeros(3)
fill(buf, 7)
print(buf)                          // b"\x07\x07\x07". fill wrote into the caller's buffer.
```

`slice` copies, so writing into a slice leaves the original alone.

### Trust

A blob the program built is trusted, because nothing about it came from outside. `bytes.concat` takes the more restrictive of its two inputs: joining anything to a blob read off a disk gives a tainted result, so `sh.exec` still refuses it. See [the trust rules](stdlib#untrusted-strings-cannot-become-commands).

One edge is worth knowing about. An int carries no trust anywhere in Jade, so a program that walks a tainted blob out into an int array and back through `bytes.from_ints` gets a trusted blob holding the same octets. That is deliberate rather than an oversight: trust follows values that hold it, and a number is not one. If you need the original marker, keep the blob rather than its octet values.

### Tasks

A task may write into a buffer it allocated itself, and may not write into one it was handed. The rule is the same one that covers arrays: tasks run on a shared heap, so writing through a value the caller still holds is a data race and does not compile.

```jade
use std::bytes

async fn render(n) {
    let buf = bytes.zeros(n)        // allocated here, so writing is fine
    buf[0] = 255
    return buf
}
```

"Allocated itself" means one of the three functions above. A blob from `str.encode()` or from `b.slice()` is just as fresh, but the checker cannot tell those apart from a method of the same name on some other type, so it refuses rather than guess. Inside a task, join the blob to an empty one first, which gives you a buffer the checker knows is yours:

```jade
async fn patch(text) {
    let buf = bytes.concat(text.encode(), bytes.zeros(0))
    buf[0] = 65
    return buf
}
```

None of this applies outside a task. An ordinary function may write into any blob it can reach.

---

## LLM inference

There is no `llm` package. Running inference is language *syntax* rather than a package: you declare a prompt and dereference it, written `?p` or `?p |> Type`. The provider package now owns everything a program used to reach for through `use llm`, including the model, the token budget and accounting, anchor handling, retries, health, model profiles, and tool-call parsing. See [LLM Integration](llm).

```jade
prompt p = "Write a one-sentence summary of Jade."
let summary = ?p
print(summary)
```
