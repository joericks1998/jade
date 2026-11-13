# Jade syntax, and how the grammar colors it

This lists every construct in Jade 2.0.2 and the scope `syntaxes/jade.tmLanguage.json` gives it. The source of truth is the compiler: `compiler/src/frontend/lexer.rs` for words and symbols, `compiler/src/frontend/parser.rs` for constructs, and `compiler/src/frontend/error.rs` for syntax that was removed. When the language changes, update this list, the grammar and `test/cases.js` together.

`test/` checks each row: `cases.js` holds one case per construct, and `node grammar.js test` also tokenizes every `.jde` file in the repo. The colors for each scope are in `package.json`, under `configurationDefaults`.

## Words and literals

| construct | example | scope |
|---|---|---|
| line comment, the only kind | `// note` | `comment.line.double-slash.jade` |
| control keywords | `if elif else while for in return break continue yield try catch raise await` | `keyword.control.jade` |
| declaration keywords | `let fn class use from async` | `keyword.declaration.jade` |
| word operators, read as `&& \|\| !`, and the class test | `and or not is` | `keyword.operator.word.jade` |
| `self` | `self.count` | `keyword.other.jade` |
| booleans | `true false` | `constant.language.boolean.jade` |
| the nil value, three spellings | `nil None null` | `constant.language.null.jade` |
| integers and floats, decimal only | `42`, `3.14` | `constant.numeric.integer.jade`, `constant.numeric.float.jade` |
| built-in types | `int float bool str char` | `storage.type.jade` |
| globals with no import, from `compiler/src/core/mod.rs`, plus `join` | `print len write input wait route join cancelled max_tasks set_max_tasks max_tokens set_max_tokens` | `support.function.builtin.jade` |
| the built-in class | `Grammar.new(pattern)` | `support.class.builtin.jade` |

## Strings

Every string may span lines, in single or double quotes, and the triple-quoted forms spare escaping the quote. The lexer accepts `\\`, `\n`, `\t`, `\r` and the string's own quote; an f-string also accepts `\{` and `\}`. Any other escape is a compile error, and the grammar marks it `invalid.illegal.escape.jade`.

| construct | example | scope |
|---|---|---|
| strings | `"a"`, `'a'`, `"""a"""`, `'''a'''` | `string.quoted.double.jade`, `.single`, `.triple` |
| escapes | `\n`, `\"` | `constant.character.escape.jade` |
| f-strings | `f"x = {x}"`, `f'…'`, `f"""…"""` | the string scope, with `meta.embedded.line.jade` over the code in braces |
| braces in an f-string | `f"{Point { x: 1 }}"` | balanced, as the lexer counts them |

## Declarations

| construct | example | scope |
|---|---|---|
| function | `fn add(a, b = 2)` | name `entity.name.function.jade`, parameters `variable.parameter.jade` |
| async function | `async fn fetch(url)` | the same |
| method defined outside its class | `fn Dog::speak()`, `fn shapes.Point::area()` | class `entity.name.type.jade`, method `entity.name.function.jade` |
| class, with parents | `class Dog(Animal, shapes.Pet)` | name `entity.name.type.jade`, parents `entity.other.inherited-class.jade` |
| class members | `name`, `let legs = 4`, `prompt system = "…"`, `fn speak()` | fields plain, methods as functions |
| visibility | `@public name` | `storage.modifier.visibility.jade` |
| type decorator | `@int let n = "42"`, `@Animal let a = d`, `@public @int count` | `storage.type.jade` or `entity.name.type.jade` |
| other decorators | `@tools::register`, `@retry(3)`, `@route(on = "x")` | `entity.name.function.decorator.jade`, with `punctuation.decorator.jade` on `@` |
| imports | `use std::math`, `use sub::helper`, `from mod use a, b` | keywords, with `::` as `keyword.operator.path.jade` |

A decorator ends at its name, so it may share a line with what it decorates.

## Expressions

| construct | example | scope |
|---|---|---|
| call | `add(1, b = 3)`, `bird.speak()` | `entity.name.function.call.jade` |
| class literal | `Point { x: 1 }`, `shapes.Point { x: 1 }` | `entity.name.type.jade` |
| copy-with base | `Config { ...c, port: 9090 }` | `keyword.operator.spread.jade` |
| class member | `Account::new(…)`, `Parent::m()` | class `entity.name.type.jade` |
| class view | `Animal(d)` | `entity.name.type.jade` |
| class test | `x is Dog`, `x is shapes.Dog` | `keyword.operator.word.jade`, class `entity.name.type.jade` |
| implicit self | `.count = .count + 1` | plain |
| typed catch | `catch ValueError e { }` | class `entity.name.type.jade` |
| membership | `x in xs`, `x not in xs` | `in` control, `not` word operator |
| pipe | `a \|> f \|> g` | `keyword.operator.pipe.jade` |
| logical, bitwise, comparison, arithmetic, assignment | `&& \|\| !`, `& \| ^ ~ << >>`, `== != < > <= >=`, `+ - * / %`, `=` | `keyword.operator.*.jade` |

A capitalized name before `{`, `(` or `::` is read as a class. Jade spells classes that way by convention, and the compiler does not require it, so a capitalized function name is colored as a class too.

## Prompts

All of these are jade green, `keyword.other.prompt.jade`, and the name after `?` or `~>` is `variable.other.jade`.

| construct | example |
|---|---|
| declaration | `prompt p = "Name a city."` |
| prompt literal | `let p = prompt "text"`, `?prompt "text"` |
| dereference | `?p`, `?(a + b)`, `?items[0]` |
| typed read | `?p \|> int` |
| member dereference | `obj~>verdict`, `obj.(?verdict)` |
| prompt field | `@public prompt system = "…"` in a class |

## Removed syntax

The lexer still reads these, so the compiler can name what replaced them. The grammar marks each `invalid.deprecated.removed.jade`, drawn struck through in red, rather than coloring it as working code.

| removed | replaced by |
|---|---|
| `struct`, `extend` | `class`, with methods in the body or defined as `fn Type::method` |
| `interface` | a shared parent class |
| `use "file.jde"`, `from "file.jde" use …` | `use name`, `use sub::name` |
| `use x as y` | an import binds its last path segment |
| closures, `\|x\| x * 2` | a named function; the grammar no longer colors their parameters |

Binary literals such as `0b1010` were never in the lexer and are not colored as numbers.
