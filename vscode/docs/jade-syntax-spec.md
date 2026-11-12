# Jade Syntax Design Spec

Source of truth: `https://www.jadelang.org/llms.txt` (fetched 2026-07-16, docs current through
changelog entry v1.1.12). This spec inventories every syntax construct in the Jade language and
cross-references it against what `syntaxes/jade.tmLanguage.json` currently tokenizes, so grammar
work has a single checklist instead of ad hoc diffs.

Status legend:
- ✅ **Supported** — grammar already scopes this correctly.
- ⚠️ **Partial** — grammar matches some but not all forms, or mis-tokenizes an edge case.
- ❌ **Missing** — no rule matches this construct today.

> **Implementation update (2026-07-16):** items 1–7 from the §15 priority list have been
> implemented and verified by tokenizing `examples/demo.jde` through the real `vscode-textmate`
> engine (not just regex review). Tables below are updated in place; rows that changed status
> say so in their Notes column. Decorators (§13, item 8) remain unimplemented by design — the
> syntax is still unconfirmed beyond one changelog line. A companion color palette for every new
> scope was added to `package.json`'s `configurationDefaults` — cool-toned (blue/teal/green/violet),
> deliberately avoiding yellow so nothing collides with themes that use yellow for functions/types.

---

## 1. Lexical Basics

| Construct | Example | Status | Notes |
|---|---|---|---|
| Line comment | `// text` | ✅ | `comment` rule. |
| Automatic semicolon insertion | *(none written)* | N/A | Not a token; affects formatting rules (`while cond {` must stay on one line — see [language-configuration.json](../language-configuration.json) if adding smarter indent/newline handling). |
| Identifiers | `my_var`, `_x`, `Point` | ✅ (implicit) | No dedicated rule; falls through to default text color, which is correct — identifiers aren't keyworded. |

---

## 2. Literals

| Construct | Example | Status | Notes |
|---|---|---|---|
| Integer literal | `42`, `1000000` | ✅ | `numbers` rule. |
| Float literal | `3.14`, `0.5` | ✅ | Requires digits on both sides of `.` — grammar regex `\d+\.\d+` matches this correctly. |
| Binary integer literal | `0b1010` | ✅ | **Implemented.** `\b0b[01]+\b` in `numbers`, tried before the generic integer rule — verified `0b1010` tokenizes as one `constant.numeric.integer.jade` token, not split into `0` + `b1010`. |
| Boolean literal | `true`, `false` | ✅ | `constants` rule. |
| **`nil`** | `let a = nil` | ✅ | **Implemented.** New `constants-nil` rule, scope `constant.language.null.jade`. |
| **`None`** (alias for `nil`) | `let b = None` | ✅ | **Implemented.** Same rule as `nil`. |
| **`null`** (alias for `nil`) | `let c = null` | ✅ | **Implemented.** Same rule as `nil`. |
| Double-quoted string | `"hello"` | ✅ | `string-double` rule. |
| **Single-quoted string** | `'hello'` | ✅ | **Implemented.** New `string-single` rule, scope `string.quoted.single.jade`. Auto-closing pair also added to `language-configuration.json`. |
| Triple-quoted string (multi-line) | `"""...\n..."""` | ✅ | **Implemented & bug fixed.** New `string-triple-double` rule (begin/end on `"""`), placed *before* `string-double` in the top-level `patterns` list so the 3-char delimiter is tried first — confirmed via tokenizer that the closing `"""` no longer prematurely ends the string on its second character. |
| Triple-quoted single-quote string | `'''...\n...'''` | ✅ | **Implemented.** New `string-triple-single` rule, same fix pattern. |
| F-string (double quote) | `f"hi {name}"` | ✅ | `fstring-double` rule; interpolation body factored into shared `fstring-interpolation` fragment. |
| **F-string interpolation coloring** | `f"n = {glib.g_basename(p)}"` | ✅ | **Fixed.** The `{expr}` body previously inherited the enclosing string foreground, so an interpolation full of real code rendered as one flat orange run. `meta.embedded.line.jade` now carries a foreground rule in `configurationDefaults` that resets interpolated code to normal code coloring (deepest-scope-wins beats the theme's `string` rule), and the `{`/`}` delimiters get `punctuation.section.embedded.*` in muted slate `#8AA3B0` (shared with the `@` sigil and `self`), so they read as quiet structural punctuation and the interpolated code itself carries the contrast against the string. Nested string literals inside the interpolation still tokenize as strings. |
| **Nested braces in interpolation** | `f"pt = {Point { x: 1 }}"` | ✅ | **Fixed.** New self-recursive `fstring-nested-braces` rule — dict/struct literals inside `{...}` no longer let the first inner `}` close the interpolation early. |
| **Comments cannot leak into interpolation** | `f"{a // b}"` | ✅ | **Fixed.** Interpolations include a new `expression` fragment (the top-level pattern list minus `comment`) instead of `$self`, so a `//` inside `{...}` can't swallow the closing brace, the closing quote, and the rest of the line. |
| **F-string (single quote)** | `f'hi {name}'` | ✅ | **Implemented.** New `fstring-single` rule — verified `{expr}` interpolation still nests correctly inside it. |
| **Triple-quoted f-string** | `f"""...{expr}..."""` / `f'''...{expr}...'''` | ✅ | **Implemented.** New `fstring-triple-double` / `fstring-triple-single` rules, tried before their non-triple counterparts. |
| Escape sequences | `\n`, `\"`, `\\` | ✅ | `\\.` inside string/fstring rules. |
| String indexing | `s[0]` | ✅ (implicit) | Just bracket punctuation + identifier; no special rule needed. |
| Array literal | `[1, 2, 3]`, `[]` | ✅ (implicit) | Brackets/commas are unstyled punctuation, numbers/strings inside get their own rules — no dedicated array rule needed. |
| Dict literal | `{"name": "jade", "version": 1}` | ✅ (implicit) | Same reasoning — keys are strings (already highlighted), values recurse through existing rules. Bare-identifier dict keys (`{key: "world"}`) also fall through fine since identifiers are unstyled. |

---

## 3. Variables

| Construct | Example | Status | Notes |
|---|---|---|---|
| `let` binding | `let x = 42` | ✅ | `let` in `keywords` (declaration group). |
| Bare reassignment | `x = x + 1` | ✅ (implicit) | Just the assignment operator, already scoped. |

---

## 4. Types (as keywords in annotations)

| Construct | Example | Status | Notes |
|---|---|---|---|
| `int`, `float`, `bool`, `str` | `prompt score: int = "..."` | ✅ | `storage-types` rule. |
| `array`, `dict`, `fn`, `struct`, `nil` as *type-position* words | — | ❌ | Docs list nine runtime types total; only 4 of 9 are scoped as `storage.type.jade`. `array`/`dict`/`fn`/`struct` don't currently appear in annotation position in any documented syntax (no array/dict/fn type-annotation examples exist), so this is low priority — flagged for completeness, not urgent. `nil` should mainly be handled as the literal above, but also appears in type-annotation-like position for default params (`fn greet(name = null)`) — covered by the literal rule instead. |

---

## 5. Operators & Precedence

| Construct | Example | Status | Notes |
|---|---|---|---|
| Arithmetic `+ - * / %` | `3 + 4` | ✅ | `operators` → arithmetic. |
| Unary `- ! ~` | `-5`, `!true`, `~0` | ✅ | Covered by arithmetic/logical/bitwise rules (unary and binary forms share the same token). |
| Bitwise `& \| ^ ~ << >>` | `6 & 3` | ✅ | `operators` → bitwise. |
| `~>` (member prompt deref) | `obj~>p` | ✅ | **Implemented (2026-07-19).** Sugar for `obj.(?p)` — a prompt dereference on a member, named by analogy to C++'s pointer `->`. See §12 for the full rule; listed in `operators` too as a bare fallback so a `~>` not followed by an identifier still renders green rather than splitting into bitwise `~` + comparison `>`. |
| Logical `&& \|\| !` | `true && false` | ✅ | `operators` → logical. |
| Comparison `== != < > <= >=` | `3 == 3` | ✅ | `operators` → comparison. |
| Assignment `=` | `x = 5` | ✅ | Negative lookbehind/lookahead correctly excludes `==`, `!=`, `<=`, `>=`. |
| Pipe `\|>` | `5 \|> double` | ✅ | `operators` → pipe, and matched *before* the closure-params rule so `\|>` isn't misparsed as a closure start. |
| Arrow `->` | — | ➖ | **Removed as a syntax item (2026-07-19)** at the maintainer's direction. The dedicated `keyword.operator.arrow.jade` rule is gone; the characters now fall through to the generic arithmetic (`-`) and comparison (`>`) rules rather than forming one token. |
| **Module-path separator `::`** | `use std::math`, `from std::math use floor`, `use utils::math`, `@tools::register` | ✅ | **Implemented.** New `keyword.operator.path.jade` rule (`match: "::"`), listed first in `operators` so it's never mistaken for two single-colon punctuation tokens. Decorator usage (`@tools::register`) still won't highlight the `@name` part itself — that's §13, held. |

---

## 6. Control Flow

| Construct | Example | Status | Notes |
|---|---|---|---|
| `if` / `elif` / `else` | see docs | ✅ | `keywords` control group. |
| `while` | `while i < 5 { }` | ✅ | Same. |
| `for ... in ...` | `for n in nums { }` | ✅ | `for` and `in` both present. |
| `return` (with/without expr) | `return`, `return x` | ✅ | Same. |

---

## 7. Functions & Closures

| Construct | Example | Status | Notes |
|---|---|---|---|
| `fn name(params) { }` | `fn add(a, b) { }` | ✅ | `function-declaration` rule scopes name as `entity.name.function.jade`. |
| **Function invocation** | `factorial(5)`, `bird.is_bipedal()`, `pkg::helper(3)` | ✅ | **Implemented.** New `function-call` rule, `\b([A-Za-z_][A-Za-z0-9_]*)\s*(?=\()` → `entity.name.function.call.jade` (`#B392F0` violet, distinct from the `#5FB3B3` used at *definition* sites). A single rule covers plain calls, `extend`-block method calls, other dot-calls, and module-path calls, because `\b` matches after `.` and `:` just as after a space — the receiver and the separators fall through to their own rules and only the callee is captured. **Ordering is load-bearing:** the rule sits after `keywords`, `storage-types`, `constants` and `support-functions` in the `expression` list, since all of those match the same bare-identifier shape; placing it earlier repaints `if (x)` as a call and steals `print(`'s builtin blue. `fn name(` is claimed earlier still by `function-declaration`. Verified: `if (x)`/`while (a)` keep control coloring, `print`/`len`/`write` keep builtin blue, `@retry(3)` stays a decorator, `Animal { … }` stays a struct literal, and calls nested inside f-string interpolations highlight correctly. |
| **Known ambiguity: `int(1, 6)`** | `from std::random use int` | ⚠️ | `int` is both a primitive type name and an importable stdlib function. `storage-types` is ordered before `function-call`, so `int(1, 6)` renders as a type (`#4EC9B0`), not a call. Kept that way deliberately — flipping the order would fix the call site but mis-color nothing else of value, and a bare `int` annotation is the far more common occurrence. Same applies to `float`/`bool`/`str`. |
| Implicit return (bare final expr) | `fn double(x) { x * 2 }` | N/A | No token difference from any other expression statement — nothing to add. |
| Closure, single-expr body | `\|x\| x * 2` | ✅ | `closure-params` rule, carefully guarded against bitwise `\|` via lookbehind/lookahead. |
| Closure, block body | `\|x\| { ... }` | ✅ (implicit) | Params rule handles the `\|x\|` part; `{ }` block recurses through `$self` via top-level `patterns`. |
| Zero-param closure `\|\| expr` | `\|\| "hello"` | ⚠️ | Worth a manual check: the `closure-params` lookahead requires content starting with `[a-zA-Z_]` after the opening `\|`, so `\|\|` (immediately adjacent pipes, no param text) will **not** match `begin`, and correctly falls through to being treated as two `\|` bitwise/logical tokens rather than a parameter list — this is *visually* fine (no false-positive param highlighting) but means the delimiters get bitwise-OR coloring instead of parameter-punctuation coloring. Low priority cosmetic gap. |
| Built-ins `print`, `len` | `print(x)`, `len(arr)` | ✅ | `support-functions` rule. |
| **Built-in `write`** | `write("hello")` | ✅ | **Implemented.** Added to the `support-functions` alternation. |
| **Built-in `input`** | `input()`, `input("Name: ")` | ✅ | **Implemented.** Same rule. |
| Primitive/stdlib methods called via dot | `s.upper()`, `arr.push(x)`, `d.has(k)` | ❌ | Not scoped as functions at all today (dot-call falls through as plain identifier). Optional/stylistic — most themes don't need this, but consider a `support.function.builtin.jade` rule for method-position identifiers followed by `(` if parity with `print`/`len` styling is wanted. |

---

## 8. Async / Await

| Construct | Example | Status | Notes |
|---|---|---|---|
| `async fn name(...) { }` | `async fn ask(q) { }` | ✅ | **Implemented.** `async` added to the `keyword.declaration.jade` alternation (alongside `fn`, `struct`, etc.) — verified `async fn ask(` tokenizes as `async`→declaration, `fn`→declaration, `ask`→`entity.name.function.jade` (the existing `function-declaration` rule still fires correctly on the `fn name(` portion). |
| `await <expr>` | `return await ?p` | ✅ | **Implemented.** `await` added to the `keyword.control.jade` alternation, as planned. |

These were added exactly as planned: `await` joined `keyword.control.jade`, `async` joined
`keyword.declaration.jade` — mirroring how `fn` itself is scoped.

---

## 9. Structs & Interfaces

| Construct | Example | Status | Notes |
|---|---|---|---|
| `struct Name { field, field }` | `struct Point { x, y }` | ✅ | `type-declaration` rule. |
| `interface Name { fn sig(...) }` | `interface Describable { fn describe(self) }` | ✅ | Return-type annotations were removed from the language (2026-07-19); signatures are now bare. `interface` is in both `type-declaration`'s alternation and `keywords`' declaration group. **Caveat:** the fetched docs' page index (Quick Start → ... → Changelog) does **not** include a dedicated "Interfaces" reference page — only a one-line changelog mention (v1.0.7: "Added `interface` definitions and `extend Type: Interface` conformance checking") and the site's own summary blurb confirm the feature exists and is stable, but no full syntax reference (e.g. optional method bodies, multiple interface inheritance) was published at fetch time. Grammar support here is already ahead of the published docs; treat `examples/demo.jde` as the current best syntax reference for this feature. |
| `extend Name { fn method(self, ...) { } }` | see docs | ✅ | `type-declaration` rule. |
| `extend Name: Interface { }` | `extend Animal: Describable { }` | ✅ | Same rule, capture group 3. |
| Struct instantiation | `Point { x: 10, y: 20 }` | ✅ | **Implemented.** New `struct-literal` rule matches a capitalized identifier followed by `{` and scopes it `entity.name.type.jade`. Verified against `Animal { name: "Dog", ... }` and struct-literal-as-raise-argument (`raise ValueError { message: "..." }`) — both highlight correctly, and definitions (`struct Animal {`) still resolve via the earlier `type-declaration` rule without double-matching (it wins on start position since it begins at the keyword). |
| Field access | `p.x` | ✅ (implicit) | Dot + identifier, no rule needed. |
| Field assignment | `p.x = 99` | ✅ (implicit) | Same. |
| Method call | `c.increment()` | ✅ | **Now explicit.** The receiver and `.` stay unstyled (matching plain field access, `p.x`), and the method name is picked up by the `function-call` rule described in §7. |

---

## 10. Exceptions

| Construct | Example | Status | Notes |
|---|---|---|---|
| `raise <expr>` | `raise "oops"` | ✅ | `keywords` control group. |
| `try { } catch <binding> { }` | catch-all form | ✅ | `try`, `catch` in `keywords`. |
| `try { } catch TypeName <binding> { }` | `catch ValueError e { }` | ✅ | **Implemented.** New `exception-catch-typed` rule matches `catch` + a capitalized identifier as a pair, scoping `catch`→`keyword.control.jade` and `TypeName`→`entity.name.type.jade`. Placed before the generic `keywords` rule so it wins for the typed form. Verified both forms tokenize correctly: `catch ValueError e {` highlights `ValueError` as a type, while the catch-all `catch e {` still falls through to the plain keyword rule (the lowercase binding doesn't match `[A-Z]`, so no false positive). |

---

## 11. Imports & Modules

| Construct | Example | Status | Notes |
|---|---|---|---|
| `use` keyword | any form | ✅ | `keywords` declaration group. |
| File import w/ required alias | `use "lib.jde" as lib` | ✅ | **Implemented.** `as` added to the `keyword.declaration.jade` alternation. |
| Package import (`::` notation) | `use std::math` | ✅ | **Implemented.** `use` scoped as before; `::` now scoped via `keyword.operator.path.jade` (§5); `std`/`math` remain intentionally unstyled identifiers. |
| Library import (`[lib]` in `jade.toml`) | `use utils::math` | ✅ | Covered by the same `::` operator fix — no additional grammar work needed, as predicted. |
| **`from <pkg> use <names>`** | `from std::math use floor, ceil, sqrt` | ✅ | **Implemented.** `from` added to the `keyword.declaration.jade` alternation. Verified `from std::random use int, choice` tokenizes as `from`→declaration, `::`→path operator, `use`→declaration, with `int` picked up separately by `storage-types` (harmless coincidental overlap — `int` here is an imported function name, not a type keyword, but both render identically so it's not a visible bug). |
| Native package import | `use "native/mylib"` | ✅ (implicit) | Ordinary string, already covered by the `string` rule. |

---

## 12. LLM Integration

| Construct | Example | Status | Notes |
|---|---|---|---|
| `prompt name = "..."` | `prompt p = "..."` | ✅ | `prompt` scoped specially (`keyword.other.prompt.jade`, "jade green") in `keywords`. |
| Typed prompt binding | `prompt score: int = "..."` | ✅ | `prompt` keyword + `:` + `storage-types` all already compose correctly; no new rule needed. |
| Untyped dereference `?p` | `let r = ?p` | ✅ | `prompt-deref` rule, `?` and the name separately captured. |
| **Member dereference `obj~>p`** | `let v = critic~>verdict` | ✅ | **Implemented (2026-07-19).** Sugar for the explicit `obj.(?p)` form — dereferences prompt `p` on `obj`, named by analogy to C++'s pointer `->`. New `prompt-member-deref` rule captures `~>` as `keyword.other.prompt.jade` and the prompt name as `variable.other.jade`, so it renders identically to `?p`. Listed before `prompt-deref` and before `operators` so the name isn't left unstyled and the `~`/`>` aren't split into bitwise + comparison tokens. Verified: `critic~>verdict`, spaced `obj ~> p`, chained `a~>b~>c`, the desugared `critic.(?verdict)`, and the non-regressions `~a` (bitwise) and `a > b` (comparison) all tokenize correctly. |
| Typed dereference `?p \|> type` | `let n = ?p \|> int` | ✅ (implicit) | Composition of existing `prompt-deref` + pipe operator + `storage-types` — no new rule needed. |
| `async fn` + `await ?p` | see §8 | ✅ | Unblocked — §8's `async`/`await` keywords are implemented. |
| ~~Session variables `__tokens__`, `__model__`, `__max_retries__`, `__retry_log__`~~ | ~~`print(__tokens__)`~~ | ➖ | **Removed.** The language no longer has runtime-populated dunder globals, so the `session-variables` rule (`\b__[a-zA-Z_]+__\b` → `variable.language.jade`) and its color rule were dropped. Dunder-shaped identifiers are now just ordinary unstyled identifiers; highlighting them as language built-ins was actively misleading. |
| `use llm` runtime config (`llm.set_max_tokens`, etc.) | `llm.set_max_tokens(256)` | ✅ (implicit) | Ordinary dot-call on an identifier; no special grammar needed beyond what already exists. |

---

## 13. Decorators (undocumented in main pages — changelog-only)

| Construct | Example | Status | Notes |
|---|---|---|---|
| `@name` / `@ns::name` decorator | `@tools::register` | ✅ | **Implemented (2026-07-16), after confirming the real syntax.** The published docs still only have the one changelog line, so before writing a grammar rule the actual shape was confirmed by shallow-cloning `github.com/joericks1998/jade` and reading `src/frontend/parser.rs`'s `parse_decorators()` plus its test cases in `parser_tests.rs`. Confirmed grammar: `@name` or `@ns::name` (unlimited `::` segments, normalized internally to dot-joined), optionally followed by `(args)` where each arg is either positional (`self.parse_pipe()` — any expression) or `key = value`; decorators stack one per line via a `while` loop in the parser; valid immediately before `fn`, `async fn`, `struct`, or `extend` (not `interface`, not `let`). New `decorator` rule: `(@)(name)` begin, ending right before `(` or at the first non-identifier/`::`/whitespace character, with `::` reusing the existing path-operator scope and each name segment scoped `entity.name.function.decorator.jade`. Deliberately does **not** add special coloring for `key =` argument names — left as plain identifiers, consistent with how dict keys and struct field names are already treated elsewhere in the grammar. Verified all forms from the real test suite (`@tag`, `@tag("hello")`, `@retry(3)`, `@route(on = "action")`, `@dec("pos", key = "val")`, stacked `@a` / `@b("x")`, and `@tools::register`) tokenize correctly via `vscode-textmate`. |

---

## 14. Non-Grammar Surfaces (out of scope for `tmLanguage.json`, noted for completeness)

These aren't `.jde` syntax and wouldn't go in the grammar file, but came up in the docs and are
worth flagging if the extension's scope grows beyond syntax highlighting:

- **`jade.toml`** — project manifest (`[project]`, `[model]`, `[lib.<name>]`, `[native]` sections). Currently has zero editor support (no TOML schema, no snippets). Would need a separate `jsonValidation`/TOML-schema style contribution, not a `tmLanguage` grammar change.
- **CLI subcommands** (`jade run`, `check`, `build`, `repl`, `test`, `fmt`, `env`, `cache`, `model`, `new`/`init`, `configure`, `upgrade`) — not editor-relevant unless the extension adds a task-runner/terminal integration.

---

## 15. Priority Recommendation — Implementation Status

All items implemented and verified except the last, which stays deliberately out of scope:

1. ✅ **String literal correctness** (§2) — single-quote, triple-quote, and triple-f-string forms all added; the triple-quote mis-highlighting bug is fixed.
2. ✅ **`async` / `await`** (§8) — both keywords added, correctly composing with the existing `function-declaration` rule.
3. ✅ **`nil` / `None` / `null`** (§2) — new literal rule.
4. ✅ **`::` module-path operator** and **`from`/`as` keywords** (§5, §11) — full `use`/`from` import surface now tokenizes.
5. ✅ **Type-name highlighting at struct instantiation and typed-catch sites** (§9, §10) — both new rules added and verified not to double-match definition sites.
6. ✅ **`0b` binary integer literals** (§2) — new rule, verified against `examples/demo.jde`.
7. ✅ **`write` / `input` builtins** (§7) — added. (Session dunder variables were added alongside them and have since been removed — see §12 — after the language dropped them.)
8. ✅ **Decorators** (§13) — implemented after confirming the real syntax against `joericks1998/jade`'s parser source and tests (see §13 for the exact grammar and citations), rather than guessing from the single changelog line.

### Verification method

Every change above was checked against `examples/demo.jde` (extended with new-syntax examples)
by tokenizing it through the real `vscode-textmate` + `vscode-oniguruma` engine — not just visual
regex inspection — confirming scope names and boundaries match intent, with no regressions on
pre-existing constructs (struct definitions, catch-all exception arms, plain integers, etc.).

### Color palette

A cohesive, yellow-free palette was added to `package.json`'s `configurationDefaults` for every
new (and several pre-existing) scope, so highlighting stays consistent across editor themes
rather than depending on how each theme happens to color generic TextMate scopes:

| Scope | Color | Role |
|---|---|---|
| `keyword.other.prompt.jade` | `#00A86B` | Brand green — `prompt`, `?deref`, `~>` |
| `support.function.builtin.jade` | `#0095ff` | Blue — `print`, `len`, `write`, `input` |
| `keyword.control.jade` | `#C586C0` | Orchid — `if`/`while`/`return`/`await`/etc. (pinker than the `#B392F0` used for call sites) |
| `keyword.declaration.jade` | `#4FA6E0` | Blue — `let`/`fn`/`use`/`async`/`from`/`as`/etc. |
| `keyword.other.jade` | `#8AA3B0` | Muted slate — `self` |
| `keyword.operator.path.jade` | `#7A93A6` | Muted slate — `::` |
| `storage.type.jade` | `#4EC9B0` | Teal — `int`/`float`/`bool`/`str` |
| `constant.language.boolean.jade` | `#4FC1FF` | Azure — `true`/`false` |
| `constant.language.null.jade` | `#4FC1FF` | Azure — `nil`/`None`/`null` |
| `constant.numeric.integer.jade` / `.float.jade` | `#8FD19E` | Soft green — numeric literals |
| `entity.name.function.jade` | `#5FB3B3` | Teal-cyan — `fn` names at definition sites |
| `entity.name.function.call.jade` | `#B392F0` | Violet — callee at invocation sites (plain, method, and module-path calls). Deliberately outside the warm range: the first attempt used a soft yellow that blurred into the string salmon `#CE9178`. |
| `entity.name.type.jade` | `#4EC9B0` | Teal — struct/interface names (definitions, instantiation, typed catch) |
| `entity.other.inherited-class.jade` | `#7FCB9C` | Light green-teal — interface name in `extend X: Y` |
| `variable.parameter.jade` | `#9CDCFE` | Light blue — closure params |
| `punctuation.decorator.jade` | `#8AA3B0` | Muted slate — the `@` sigil (shares a hue with `self`) |
| `entity.name.function.decorator.jade` | `#7FD1AE` | Mint green — decorator names |
| `meta.embedded.line.jade` | `#D4D4D4` | Default foreground — resets f-string interpolation bodies to normal code coloring |
| `punctuation.section.embedded.begin.jade` / `.end.jade` | `#8AA3B0` | Muted slate — the `{`/`}` of an f-string interpolation (shares a hue with `self` and `@`) |

The whole palette stays in the blue/teal/green/violet range by design — no yellow or orange —
so it reads as one deliberate system rather than a grab-bag of theme defaults. This is a real
constraint, not a description: the f-string and call-site work initially reached for `#DCDCAA`
and `#D7BA7D`, both of which broke the rule *and* blurred into the theme's string salmon
`#CE9178`. Anything warm will collide with strings — stay in the cool range.

---

## Appendix: Full Feature List (flat reference)

Types: `int`, `float`, `bool`, `fn`, `struct`, `str`, `array`, `dict`, `nil`
Literals: integers, `0b` binaries, floats, booleans, `nil`/`None`/`null`, single/double/triple-quoted strings, f-strings (all quote/triple variants), array literals, dict literals
Bindings: `let`
Control flow: `if`/`elif`/`else`, `while`, `for … in …`, `return`
Functions: `fn`, closures `|params| body`, implicit return, recursion, first-class functions, `async fn`, `await`
Structs: `struct`, `extend`, `extend … : Interface`, `interface`, field access/assignment, method calls, struct literals
Exceptions: `raise`, `try`/`catch`, typed `catch TypeName binding`
Imports: `use "path" as alias`, `use pkg::sub`, `from pkg::sub use a, b`, library imports via `jade.toml [lib]`, native imports via `jade.toml [native]`
LLM: `prompt`, `?deref`, `?deref |> type`, `use llm` runtime config
Operators: `+ - * / %`, `& | ^ ~ << >>`, `&& || !`, `== != < > <= >=`, `=`, `|>`, `::`, `~>`
Decorators: `@name`, `@ns::name` (undocumented shape)
Built-ins: `print`, `write`, `len`, `input`
