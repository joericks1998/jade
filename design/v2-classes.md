# Jade 2.0.1: classes, and no closures

Status: proposed, 2026-11-05. Nothing here is built yet. Joe has decided how methods attach to a class (1), implicit `self` (2), private-by-default members and their reach (3), constructors (4), the method-conflict rule (6) and types as decorators (10). The rest are recommendations that stand unless he changes them.

## Summary

Jade's purpose is to be a compiler with LLM inference built in, not a general-purpose systems language, so its feature set should be small and easy to explain. This change reshapes it in four places.

- *A class replaces a struct and its `extend` blocks.* It is modeled on C++, with one difference: Jade infers types, so nothing is annotated unless the program means to convert or declare one. A method is defined inside the class body, or outside it as `fn A::foo(a, b)`, in any file that can see the class.
- *Members are private unless marked public.* A built-in `@public` decorator makes a field or method visible outside the class.
- *Types are decorators.* `@int let x = v` converts `v` to an int, and `@Animal let a = d` declares `a` as an `Animal`. The same decorators type a class's fields.
- *Closures are removed.* Named functions stay values that can be passed and stored, and the pipe operator and extra arguments cover what inline closures did.

```
// today
struct Dog(Animal) { breed }
extend Dog {
    fn speak(self) { return "woof" }
}
let loud = nums.map(|x| x * 2)

// 2.0.1
class Dog(Animal) {
    @public breed
    @public fn speak()
}

fn Dog::speak() { return "woof" }

fn double(x) { x * 2 }
let loud = nums.map(double)
```

## Why

*Classes.* A struct today is data, and its behavior lives in one or more `extend` blocks, which any file may add. Reading a type means finding every block that extends it. A class body that lists every member, with the code inside it or defined elsewhere by name, gives a type one place that says what it is.

*Private members.* An `extend` block can reach every field of a struct from anywhere, so nothing a type holds is its own. Private by default makes a class's public surface a choice its author writes down.

*Types as decorators.* Jade has no type annotations and should not grow a second syntax for them. A decorator already sits above a declaration and changes what it binds, so naming a type there is the smallest way to state one when inference is not what the program wants.

*Closures.* A closure captures variables from where it was written, which is a second way for a function to see state beside parameters and globals. The examples use closures in exactly one runnable file. The static backend also gets simpler: a function value becomes a plain function pointer, with no captured environment to allocate, count or free, and phase 3 of `design/v2-static-values.md` loses its largest piece.

## Decisions

### 1. `class` replaces `struct` and `extend`, and methods attach by name

*Decided.* A class declares its fields and its methods. A method's body is written inside the class, or outside it as `fn ClassName::method(...)`. A definition outside the class may live in another file, as a C++ or C# method does, and names the class the way that file names it elsewhere: `fn Point::area()` in the defining file, or `fn shapes.Point::area()` after `use shapes`.

```
class Point {
    @public x
    @public let y = 10

    @public fn to_str() { return f"({self.x}, {self.y})" }
    @public fn area()                 // defined below, or in another file
}

fn Point::area() { return self.x * self.y }
```

A method defined outside its class must be declared in the class body first, with no body: `fn area()`. That keeps the class body the one place that lists what a class has, and it is what lets an outside definition read private fields without letting any file at all add a method that does. A definition with no declaration, or a declaration with no definition, is a compile error.

*What stops compiling.* Every `struct` and every `extend` block. The compiler refuses both with a message that names the replacement, such as "`struct` is now `class`; write the methods from `extend Dog { … }` in the class body, or as `fn Dog::name(…)`".

### 2. `self` is implicit

*Decided.* Inside a method, wherever it is defined, `self` is always available and is not listed, like `this` in C++. The class association, either the class body or the `A::` prefix, is what says which class `self` is. A function that needs no instance is written at the top level of a file, so there are no static methods.

### 3. Members are private unless marked `@public`

*Decided.* A field or method is private by default. The built-in decorator `@public` makes it visible outside the class. Reading or setting a private member from outside is a compile error that names the fix:

```
class Account {
    @public owner
    balance                           // private

    @public fn deposit(n) { self.balance = self.balance + n }
}

let a = Account { owner: "ann", balance: 0 }
a.deposit(5)
print(a.owner)                        // fine
print(a.balance)                      // error: 'balance' is private to Account;
                                      // mark it @public to read it here
```

A private member is visible to the class's own methods and to the methods of every class that inherits from it, which is what C++ calls protected. Jade has two levels, private and public, rather than three.

Top-level functions and variables in a file stay visible to files that import it, as today. This decision is about class members.

### 4. Private state is set by a constructor

*Decided.* A literal written outside a class may set only its `@public` fields, and every private field it leaves out takes its default. A class whose private fields need values from outside provides a constructor.

*Recommendation for its shape.* A constructor is a member named `new`, declared in the class like any method and defined in or out of it. It is the one member with no `self`, because there is no value yet. It builds the value with a literal, which inside the class's own code may set every field, and returns it. It is called as `Account::new(…)`.

```
class Account {
    @public owner
    balance

    @public fn new(owner)
    @public fn deposit(n) { self.balance = self.balance + n }
}

fn Account::new(owner) {
    return Account { owner: owner, balance: 0 }
}

let a = Account::new("ann")
let b = Account { owner: "bob" }      // error: Account's private field 'balance'
                                      // has no default; build it with Account::new(…)
```

A child builds its parent's part with the copy-with literal that exists today: `Dog { ...Animal::new(name), breed: b }` copies the fields the parent's constructor set.

Building the value in a literal means every field has a value from the moment the object exists, so no field can be read before it is set. The alternative is C++'s shape, where the constructor receives a `self` and assigns its fields one by one. That needs a check that every field is assigned on every path before the constructor ends, and a rule for reading a field before then.

### 5. A shared ancestor is inherited once

*Recommendation.* A class inherits each ancestor once, however many paths lead to it, as C++ virtual inheritance does. A diamond has one copy of the shared ancestor's fields. Two parents that declare the same field independently are still an error, since there would be two storage slots and nothing to choose between them.

```
class Named { @public name }
class Pet(Named) { @public owner }
class Worker(Named) { @public job }
class ServiceDog(Pet, Worker) { }

let d = ServiceDog { name: "rex", owner: "ann", job: "guide" }   // one `name`
```

### 6. Two inherited methods with one name are an error

*Decided.* When two parents both supply a method of the same name, the class must define that method itself, or it does not compile, as in C++. A shared ancestor's method reaching the class by two paths is one method, not two, so a diamond alone does not trigger this.

### 7. `Parent::m()` calls a parent's version

*Recommendation.* Inside a method, `Animal::speak()` calls `Animal`'s version of `speak` on the same `self`, as C++'s `Base::method()` does. It reuses the `A::` form of decision 1, and it settles decision 6's case: a class with two parents that both have `speak` defines its own and calls whichever parent's version it wants.

```
class Puppy(Dog) {
    @public fn speak() { return Dog::speak() + "!" }
}
```

### 8. `x is Dog` tests a value's class

*Recommendation.* `x is Dog` is true when `x` holds a `Dog` or a descendant of one. Inside `if x is Dog { … }`, `x` is read as a `Dog`, so its own public fields can be used. It works like the `if x != nil` narrowing that already exists, and it closes open question 1 of `design/v2-static-values.md`.

### 9. Any parent can stand in for its child

*Recommendation.* A child's fields start with its first parent's, so a child reads as its first parent at no cost. Where a field is read through a class whose descendants keep it at different offsets, the compiled code reads the offset from a small table indexed by the value's class id. The whole program is compiled at once, so the compiler knows every class, and only programs that use a second parent pay for the table. This closes open question 2 of `design/v2-static-values.md`.

### 10. Types are decorators

*Decided.* The built-in types are decorators: `@int`, `@float`, `@str`, `@bool` and `@char`, and every class name. They decorate `let` statements and class fields.

- *On a built-in type*, the decorator converts, as `@f let x = v` already means `let x = f(v)`: `@int let n = "42"` makes `n` the int 42, and `@float let x = 1` makes it 1.0.
- *On a class*, it declares the variable's class, which may be a parent of the value's: `@Animal let a = Dog { … }` lets `a` later hold a `Cat`. A class that is not the value's own or one of its ancestors is a compile error.
- *On a field*, it gives the field a type in place of a default: `@int count`, or `@public @int count` for a public one.

```
class Inventory {
    @public @int count
    @str label
}

@float let ratio = 1
@Animal let pet = Dog { name: "rex", breed: "corgi" }
```

Nothing else needs annotating; inference covers the rest, as today.

### 11. Closures are removed

*Recommendation.* Remove `|x| expr` and `|x| { … }`. A named top-level function stays a value: it can be passed, stored and called, and so can a builtin such as `str` or `len`. The pipe operator already passes a value as a function's first argument, so `5 |> add(3)` is `add(5, 3)`. State a closure would capture becomes an argument instead.

```
// today
let multiplier = 3
let triple = |x| x * multiplier
print(triple(5))

// 2.0.1
fn scale(x, by) { x * by }
print(5 |> scale(3))
```

*What stops compiling.* `examples/closures/closures.jde`, and `examples/async/shared_state/callback_state_error.jde`, which tests a closure handed to a task. That fixture is rewritten to hand a named function to the task, so the task-safety check it pins stays covered.

### 12. `map` and `filter` pass extra arguments along

*Recommendation.* Extra arguments to `map` and `filter` are passed to the function after each element, in the order the pipe uses.

```
fn scale(x, by) { x * by }
print([1, 2, 3].map(scale, 10))      // [10, 20, 30]
```

### 13. A method is called, not passed

*Recommendation.* Remove bound methods as values. `let bump = c.bump` holds a receiver to pass as `self` later, which is a closure over one variable. A function that needs to call a method later is handed the object.

*What stops compiling.* `examples/structs/bound_method/bound_method.jde`, which exists to test bound methods, and one line of `examples/functions/values/values.jde`.

### 14. New behavior lands in the static backend only

*Recommendation.* The tagged backend and both runtimes are deleted at the end of v2, so new behavior is built only in the static backend. Some of this change works on both backends anyway:

- the class syntax, `A::` definitions and implicit `self`, which the parser rewrites into today's definitions;
- visibility, which is a compile-time check;
- conversion decorators, which are calls;
- removing closures and bound methods, which only removes code.

The rest is static only: a shared ancestor once, `Parent::m()`, `is`, second-parent field tables, class decorators and extra arguments to `map`. Examples that use those are listed for `golden.sh --backend=static` and skipped by the default gate with the reason "static backend only".

## How the compiler changes

- *Lexer and parser.* `class` becomes a keyword. The parser reads a class body: fields, method definitions and method declarations, each with decorators. An `fn A::m(...)` definition is matched to its declaration, and every method gets `self` as its first parameter. The result is the same definition nodes a struct and its `extend` blocks produce today, plus each member's visibility. `struct`, `extend` and `|x|` are refused with messages naming the replacement. `Parent::m()` and `x is T` are new expressions.
- *Imports.* An `fn shapes.Point::m()` definition in one file attaches to the class another file declares.
- *`type_infer.rs`.* Flattening inherits a shared ancestor once and finds conflicting methods.
- *`compiler/infer/`.* It checks visibility on every field read, field write, method call and literal, and that a literal outside its class sets only public fields. It resolves `Parent::m()` to that parent's method, and narrows on `is` the way it does on `!= nil`. It types a variable or field from its type decorator. Closures and bound methods leave `FnKey` and `FuncRef`.
- *`mir/` and `backend/`.* `is` becomes a type-id test, `Parent::m()` a direct call, and second-parent field reads use the offset table. `map` and `filter` with function arguments land here as part of phase 3.
- *Everywhere a message says "struct"*, it says "class". Internal names in code the switchover deletes are left alone.
- *`jade fmt`* prints classes.

## What happens to the examples

- *40 files declare structs and 17 have `extend` blocks.* Each is rewritten as classes, with `@public` on what other code reads. The recorded output does not change.
- *`closures/closures.jde`* is rewritten to named functions and pipes, with the same output.
- *`structs/bound_method`* is removed, and `functions/values` loses its bound-method line.
- *`async/shared_state/callback_state_error.jde`* hands the task a named function.
- *New examples* cover `A::` definitions across files, private access errors, constructors, type decorators, diamonds, method conflicts, `Parent::m()`, `is`, second parents and extra arguments to `map`.
- *User docs:* the struct, closure, decorator and inheritance pages in `docs/docs/` are rewritten.

## Order of work

1. *Remove closures and bound methods* from the parser, the checkers and both backends, and rewrite the three examples.
2. *Class syntax*: class bodies, `A::` definitions across files, implicit `self`, `jade fmt`, the messages, every example and the docs.
3. *Visibility and constructors*: private by default, `@public`, the checks, and `new`.
4. *Type decorators* on `let` statements and fields.
5. *Inheritance rules*: a shared ancestor once, and method conflicts.
6. *`Parent::m()` and `is`*, in the checker and the static backend.
7. *Second-parent field tables* in the static backend.

Then phase 3 of `design/v2-static-values.md` continues, smaller than before: exceptions, function values as plain pointers, `map` and `filter`, and streams.

## Open questions

None. Decisions 5, 7, 8, 9 and 11 to 14 are recommendations Joe has not overruled, and the constructor's shape in decision 4 is one.
