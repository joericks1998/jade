// One case per construct in compiler/src/frontend/lexer.rs and parser.rs, as of
// Jade 2.0.2: [source, [[token text, scope it must have], ...]]. A scope starting
// with `!` must be absent from every token with that text.
module.exports = [
  // Comments
  ['let x = 1 // note', [['// note', 'comment.line.double-slash.jade']]],

  // Keywords
  ['if a { } elif b { } else { }', [['if', 'keyword.control.jade'], ['elif', 'keyword.control.jade'], ['else', 'keyword.control.jade']]],
  ['while a { break }', [['while', 'keyword.control.jade'], ['break', 'keyword.control.jade']]],
  ['for x in xs { continue }', [['for', 'keyword.control.jade'], ['in', 'keyword.control.jade'], ['continue', 'keyword.control.jade']]],
  ['yield x', [['yield', 'keyword.control.jade']]],
  ['return x', [['return', 'keyword.control.jade']]],
  ['raise ValueError { message: "x" }', [['raise', 'keyword.control.jade'], ['ValueError', 'entity.name.type.jade']]],
  ['let v = await f', [['let', 'keyword.declaration.jade'], ['await', 'keyword.control.jade']]],
  ['if a and b or not c { }', [['and', 'keyword.operator.word.jade'], ['or', 'keyword.operator.word.jade'], ['not', 'keyword.operator.word.jade']]],
  ['if x not in xs { }', [['not', 'keyword.operator.word.jade'], ['in', 'keyword.control.jade']]],
  ['let v = self.name', [['self', 'keyword.other.jade']]],

  // Removed in 2.0.1: still words the lexer knows, refused by the parser
  ['struct Point { x }', [['struct', 'invalid.deprecated.removed.jade'], ['struct', '!keyword.declaration']]],
  ['extend Point { }', [['extend', 'invalid.deprecated.removed.jade']]],
  ['interface Shape { }', [['interface', 'invalid.deprecated.removed.jade']]],
  ['let f = |x| x * 2', [['x', '!variable.parameter']]],

  // Constants and numbers
  ['let t = true', [['true', 'constant.language.boolean.jade']]],
  ['let f = false', [['false', 'constant.language.boolean.jade']]],
  ['let a = nil', [['nil', 'constant.language.null.jade']]],
  ['let a = None', [['None', 'constant.language.null.jade']]],
  ['let a = null', [['null', 'constant.language.null.jade']]],
  ['let n = 42', [['42', 'constant.numeric.integer.jade']]],
  ['let r = 3.14', [['3.14', 'constant.numeric.float.jade']]],
  ['let m = -9223372036854775808', [['9223372036854775808', 'constant.numeric.integer.jade']]],
  ['let b = 0b1010', [['0b1010', '!constant.numeric']]],

  // Strings and escapes
  ['let s = "a\\n\\t\\r\\\\\\"b"', [['\\n', 'constant.character.escape.jade'], ['\\"', 'constant.character.escape.jade']]],
  ["let s = 'it\\'s'", [["\\'", 'constant.character.escape.jade']]],
  ['let s = "bad \\q"', [['\\q', 'invalid.illegal.escape.jade']]],
  ['let s = "no \\\' here"', [["\\'", 'invalid.illegal.escape.jade']]],
  ['let s = """\nmulti\n"""', [['multi', 'string.quoted.triple.jade']]],
  ["let s = '''\nmulti\n'''", [['multi', 'string.quoted.triple.jade']]],
  ['let s = "spans\nlines"', [['lines', 'string.quoted.double.jade']]],
  ['let s = f"x = {add(1, 2)}"', [['add', 'entity.name.function.call.jade'], ['{', 'punctuation.section.embedded.begin.jade']]],
  ['let s = f"\\{literal\\}"', [['\\{', 'constant.character.escape.jade'], ['\\}', 'constant.character.escape.jade']]],
  ['let s = "\\{"', [['\\{', 'invalid.illegal.escape.jade']]],
  ["let s = f'{a}'", [['a', 'meta.embedded.line.jade']]],
  ['let s = f"""\n{a}\n"""', [['a', 'meta.embedded.line.jade']]],
  ['let s = f"{Point { x: 1 }}"', [['Point', 'entity.name.type.jade']]],
  ['let buf = elf"x"', [['elf', '!string']]],

  // Functions and calls
  ['fn add(a, b = 2) { }', [['fn', 'keyword.declaration.jade'], ['add', 'entity.name.function.jade']]],
  ['async fn fetch(url) { }', [['async', 'keyword.declaration.jade'], ['fetch', 'entity.name.function.jade']]],
  ['let v = add(1, b = 3)', [['add', 'entity.name.function.call.jade']]],
  ['let v = list.join(",")', [['join', 'entity.name.function.call.jade'], ['join', '!support.function']]],
  ['let v = math.sqrt(2.0)', [['sqrt', 'entity.name.function.call.jade']]],

  // Built-in globals, from compiler/src/core/mod.rs
  ['print(len(xs))', [['print', 'support.function.builtin.jade'], ['len', 'support.function.builtin.jade']]],
  ['write(input("> "))', [['write', 'support.function.builtin.jade'], ['input', 'support.function.builtin.jade']]],
  ['wait(f)', [['wait', 'support.function.builtin.jade']]],
  ['route(x)', [['route', 'support.function.builtin.jade']]],
  ['let r = join(f1, f2, settle = true)', [['join', 'support.function.builtin.jade']]],
  ['if cancelled() { }', [['cancelled', 'support.function.builtin.jade']]],
  ['set_max_tasks(max_tasks() * 2)', [['set_max_tasks', 'support.function.builtin.jade'], ['max_tasks', 'support.function.builtin.jade']]],
  ['set_max_tokens(max_tokens())', [['set_max_tokens', 'support.function.builtin.jade'], ['max_tokens', 'support.function.builtin.jade']]],
  ['let g = Grammar.new("[0-9]+")', [['Grammar', 'support.class.builtin.jade']]],

  // Primitive types: conversions, pipe targets and decorators
  ['let n = int("4")', [['int', 'storage.type.jade']]],
  ['let c = char(65)', [['char', 'storage.type.jade']]],
  ['let v = ?p |> float', [['float', 'storage.type.jade']]],

  // Classes
  ['class Dog(Animal, shapes.Pet) {', [['class', 'keyword.declaration.jade'], ['Dog', 'entity.name.type.jade'], ['Animal', 'entity.other.inherited-class.jade'], ['Pet', 'entity.other.inherited-class.jade']]],
  ['class Empty { }', [['class', 'keyword.declaration.jade'], ['Empty', 'entity.name.type.jade']]],
  ['class Dog(\n    Animal,\n    Pet\n) {', [['Animal', 'entity.other.inherited-class.jade'], ['Pet', 'entity.other.inherited-class.jade']]],
  ['    @public name', [['@', 'punctuation.decorator.jade'], ['public', 'storage.modifier.visibility.jade'], ['name', '!decorator']]],
  ['    @public @int count', [['public', 'storage.modifier.visibility.jade'], ['int', 'storage.type.jade'], ['count', '!decorator']]],
  ['    @public let legs = 4', [['let', 'keyword.declaration.jade'], ['legs', '!decorator']]],
  ['    @public fn speak()', [['speak', 'entity.name.function.jade']]],
  ['    fn new(name) { }', [['new', 'entity.name.function.jade']]],
  ['fn Dog::speak() { }', [['Dog', 'entity.name.type.jade'], ['::', 'keyword.operator.path.jade'], ['speak', 'entity.name.function.jade']]],
  ['fn shapes.Point::area() { }', [['Point', 'entity.name.type.jade'], ['area', 'entity.name.function.jade']]],
  ['let a = Account::new("x")', [['Account', 'entity.name.type.jade'], ['new', 'entity.name.function.call.jade']]],
  ['let a = shapes.Account::new("x")', [['Account', 'entity.name.type.jade']]],
  ['    return Animal::speak()', [['Animal', 'entity.name.type.jade'], ['speak', 'entity.name.function.call.jade']]],
  ['let a = Animal(d)', [['Animal', 'entity.name.type.jade']]],
  ['if pet is Dog { }', [['is', 'keyword.operator.word.jade'], ['Dog', 'entity.name.type.jade']]],
  ['if pet is shapes.Dog { }', [['Dog', 'entity.name.type.jade']]],
  ['let p = Point { x: 1, y: 2 }', [['Point', 'entity.name.type.jade']]],
  ['let p = shapes.Point { x: 1 }', [['Point', 'entity.name.type.jade']]],
  ['let q = Config { ...c, port: 9090 }', [['...', 'keyword.operator.spread.jade']]],
  ['    .count = .count + 1', [['=', 'keyword.operator.assignment.jade'], ['+', 'keyword.operator.arithmetic.jade']]],

  // Decorators
  ['@int\nlet n = "42"', [['int', 'storage.type.jade'], ['let', 'keyword.declaration.jade']]],
  ['@int let n = "42"', [['int', 'storage.type.jade'], ['n', '!decorator']]],
  ['@Animal let pet = Dog { name: "rex" }', [['Animal', 'entity.name.type.jade'], ['pet', '!decorator']]],
  ['@shapes::Animal let pet = d', [['shapes', 'entity.name.function.decorator.jade'], ['Animal', 'entity.name.type.jade']]],
  ['@tools::register', [['tools', 'entity.name.function.decorator.jade'], ['::', 'keyword.operator.path.jade'], ['register', 'entity.name.function.decorator.jade']]],
  ['@retry(3)', [['retry', 'entity.name.function.decorator.jade'], ['3', 'constant.numeric.integer.jade']]],
  ['@route(on = "action")', [['route', 'entity.name.function.decorator.jade'], ['=', 'keyword.operator.assignment.jade']]],
  ['@tagged prompt p = "x"', [['tagged', 'entity.name.function.decorator.jade'], ['prompt', 'keyword.other.prompt.jade']]],

  // Prompts
  ['prompt p = "Name a city."', [['prompt', 'keyword.other.prompt.jade']]],
  ['let p = prompt "text"', [['prompt', 'keyword.other.prompt.jade']]],
  ['let v = ?p', [['?', 'keyword.other.prompt.jade'], ['p', 'variable.other.jade']]],
  ['let v = ?p |> int', [['|>', 'keyword.operator.pipe.jade'], ['int', 'storage.type.jade']]],
  ['let v = ?prompt "x"', [['?', 'keyword.other.prompt.jade'], ['prompt', 'keyword.other.prompt.jade'], ['prompt', '!variable.other']]],
  ['let v = ?(a + b)', [['?', 'keyword.other.prompt.jade']]],
  ['let v = ?items[0]', [['?', 'keyword.other.prompt.jade']]],
  ['let v = obj~>verdict', [['~>', 'keyword.other.prompt.jade'], ['verdict', 'variable.other.jade']]],
  ['let v = obj.(?verdict)', [['?', 'keyword.other.prompt.jade'], ['verdict', 'variable.other.jade']]],

  // Parameters
  ['fn add(a, b = 2) { }', [['a', 'variable.parameter.jade'], ['b', 'variable.parameter.jade'], ['2', 'constant.numeric.integer.jade']]],
  ['fn f(a = g(1, c), d = [1, e]) { }', [['a', 'variable.parameter.jade'], ['d', 'variable.parameter.jade'], [', c', '!variable.parameter'], [', e', '!variable.parameter'], ['g', 'entity.name.function.call.jade']]],
  ['fn Dog::rename(to) { }', [['to', 'variable.parameter.jade']]],
  ['fn f(\n    a,\n    b\n) { }', [['a', 'variable.parameter.jade'], ['b', 'variable.parameter.jade']]],
  ['fn f(a) { return a }', [['return', 'keyword.control.jade']]],

  // Imports
  ['use std::time', [['use', 'keyword.declaration.jade'], ['::', 'keyword.operator.path.jade']]],
  ['use sub::helper', [['::', 'keyword.operator.path.jade']]],
  ['use std::time as t', [['as', 'invalid.deprecated.removed.jade']]],
  ['use "lib/shapes.jde"', [['use', 'keyword.declaration.jade'], ['"lib/shapes.jde"', 'invalid.deprecated.removed.jade']]],
  ["from 'lib.jde' use a", [["'lib.jde'", 'invalid.deprecated.removed.jade'], ['use', 'keyword.declaration.jade']]],
  ['from std::math use floor, sqrt', [['from', 'keyword.declaration.jade'], ['use', 'keyword.declaration.jade']]],

  // Exceptions
  ['try { } catch ValueError e { } catch e { }', [['try', 'keyword.control.jade'], ['catch', 'keyword.control.jade'], ['ValueError', 'entity.name.type.jade']]],

  // Operators
  ['let v = a |> f |> g', [['|>', 'keyword.operator.pipe.jade']]],
  ['let v = a && b || !c', [['&&', 'keyword.operator.logical.jade'], ['||', 'keyword.operator.logical.jade'], ['!', 'keyword.operator.logical.jade']]],
  ['let v = (a << 2) >> 1 & 3 | 4 ^ ~5', [['<<', 'keyword.operator.bitwise.jade'], ['>>', 'keyword.operator.bitwise.jade'], ['~', 'keyword.operator.bitwise.jade']]],
  ['let v = a == b != c <= d >= e < f > g', [['==', 'keyword.operator.comparison.jade'], ['<=', 'keyword.operator.comparison.jade']]],
  ['let v = a + b - c * d / e % f', [['%', 'keyword.operator.arithmetic.jade']]],
  ['x = 1', [['=', 'keyword.operator.assignment.jade']]],
];
