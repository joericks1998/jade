use super::error::Span;
use serde::{Deserialize, Serialize};

/// A decorator and its arguments, as `@name(arg, key = arg)` parses.
///
/// Each entry is `(name, args)`, and each argument is `(keyword, expr)` with the
/// keyword absent for a positional one.
///
/// Named once because it travels: the parser builds it, the per-file pass
/// rewrites inside it, the import pass renames through it, and the checker
/// reads it.
pub type DecoratorList = Vec<(String, Vec<(Option<String>, Expr)>)>;

/// A field declaration inside a `struct` body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StructFieldDef {
    /// Bare `name,` — required, no default.
    Required(String),
    /// `let name = expr` — optional, has a default value expression.
    Let { name: String, default: Expr },
    /// `prompt name = expr` — optional prompt field; default must eval to a string.
    Prompt { name: String, default: Expr },
}

impl StructFieldDef {
    pub fn name(&self) -> &str {
        match self {
            StructFieldDef::Required(name) => name,
            StructFieldDef::Let { name, .. } => name,
            StructFieldDef::Prompt { name, .. } => name,
        }
    }
}

/// A part of an interpolated f-string expression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FStrPart {
    /// A literal string segment between (or before/after) interpolation slots.
    Literal(String),
    /// An interpolated expression: the value is stringified at runtime.
    Expr(Expr),
}

/// A single arm of a `try/catch` statement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatchArm {
    /// If `Some(name)`, only catches exceptions whose struct `type_name` matches.
    /// If `None`, this is a catch-all arm that matches any raised value.
    pub catch_type: Option<String>,
    /// The variable name bound to the caught exception value inside the arm body.
    pub binding: String,
    pub body: Vec<Stmt>,
}

/// A complete Jade program: a list of statements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

/// A statement is a top-level action.
/// Purpose: This struct holds what a class body says about its members
/// beyond their definitions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClassMembers {
    /// Each decorated member, by name, with the names of its decorators in
    /// written order. `public` makes a member visible outside the class; a type
    /// name gives a field its type.
    pub decorators: Vec<(String, Vec<String>)>,
    /// Methods the body declares without a body, defined as `fn Name::m(…)`
    /// elsewhere. A declared constructor appears as `new`.
    pub declared: Vec<String>,
}

impl ClassMembers {
    /// Purpose: This function says whether a member is marked `@public`.
    ///
    /// Inputs:
    /// - `name`: the member.
    ///
    /// Outputs: true when one of its decorators is `public`.
    pub fn is_public(&self, name: &str) -> bool {
        self.decorators.iter().any(|(m, ds)| m == name && ds.iter().any(|d| d == "public"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Stmt {
    /// `let name = expr`
    Let {
        name: String,
        value: Expr,
        span: Span,
    },

    /// `fn name(param, param = default, ...) { body }`
    FnDef {
        name: String,
        /// Each entry is `(param_name, default_expr)`. Required params have `None`.
        params: Vec<(String, Option<Expr>)>,
        body: Vec<Stmt>,
        /// Each entry is `(decorator_name, positional_args)`.
        decorators: DecoratorList,
        span: Span,
    },

    /// `return expr` or bare `return`
    /// `yield expr` — append a value to the stream this function produces.
    ///
    /// A function whose body contains a `yield` returns a `stream` rather than
    /// its declared value: the body runs to completion filling a buffer, and
    /// the caller reads the buffer. That is the whole model — a stream *is* a
    /// buffer, not a one-shot channel — so reading one twice gives the same
    /// values twice, and there is no rule about what a second read means.
    Yield {
        value: Expr,
        span: Span,
    },

    Return {
        value: Option<Expr>,
        span: Span,
    },

    /// `if condition { then_body } else { else_body }`
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Option<Vec<Stmt>>,
        span: Span,
    },

    /// `while condition { body }`
    While {
        condition: Expr,
        body: Vec<Stmt>,
        span: Span,
    },

    /// `for var in iterable { body }`
    For {
        var: String,
        iterable: Expr,
        body: Vec<Stmt>,
        span: Span,
    },

    /// `break` — leave the innermost loop.
    Break {
        span: Span,
    },

    /// `continue` — go on to the innermost loop's next iteration.
    Continue {
        span: Span,
    },

    /// `name = expr` — reassign an existing variable (or introduce one) in the global env
    Assign {
        name: String,
        value: Expr,
        span: Span,
    },

    /// `class Name(Parent, …) { field, … }`: the data half of a class. The
    /// parser splits a class body into this, an `ExtendBlock` of the methods
    /// written in it, and a top-level `FnDef` named `Name::new` for a
    /// constructor, so the stages after it see the shapes they always did.
    StructDef {
        name: String,
        /// The fields as the body wrote them. The per-file pass flattens the
        /// list, every inherited field first, so nothing after it has to walk
        /// a chain of parents to build a value or find a field; a parent from
        /// another file is folded in by `fill_struct_literal_defaults` once
        /// imports are merged. It also rewrites each default the way it
        /// rewrites any expression.
        fields: Vec<StructFieldDef>,
        /// Classes this one inherits, in written order. A name may be bare
        /// (`Animal`) or qualified for an imported one (`shapes.Animal`).
        /// Empty for a class that inherits nothing. The import pass renames it
        /// as it renames any type reference; it is read afterwards to work out
        /// each class's full ancestry, which a typed `catch` arm needs.
        parents: Vec<String>,
        /// What the class body says about its members beyond their definitions.
        #[serde(default)]
        members: ClassMembers,
        span: Span,
    },

    /// A class's methods: the ones written in its body, or one written outside
    /// it as `fn TypeName::method(…) { … }`. Each method's first parameter is
    /// `self`, which the parser adds.
    ExtendBlock {
        type_name: String,
        methods: Vec<Stmt>,
        decorators: DecoratorList,
        /// True for `fn TypeName::method(…)`, a definition outside the class
        /// body, which the body must declare.
        #[serde(default)]
        outside: bool,
        span: Span,
    },

    /// `object.field = expr` — mutate a field on a struct instance
    FieldAssign {
        object: String,
        field: String,
        value: Expr,
        span: Span,
    },

    /// `name[index] = expr` — mutate an element of an array
    IndexAssign {
        name: String,
        index: Expr,
        value: Expr,
        span: Span,
    },

    /// `prompt name = expr` — declare a prompt value from a string expression.
    PromptDecl {
        name: String,
        body: Expr,
        span: Span,
    },

    /// `use "path/to/file.jde"` — import all top-level definitions from another file.
    /// Also accepts `::` notation: `use std::time` → path `"std/time"`.
    /// `as_name` binds the module under that name; if absent, the stem of the path is used.
    /// stdlib packages are unaffected (they always bind under their own `global_name`).
    /// `path_is_string` is true when the user wrote `use "..."` (string literal form).
    Use {
        path: String,
        as_name: Option<String>,
        path_is_string: bool,
        span: Span,
    },

    /// `from std::time use now, sleep` — import specific names directly into scope.
    /// Path follows the same rules as `Use` (string literal or `::` notation).
    FromUse {
        path: String,
        names: Vec<String>,
        path_is_string: bool,
        span: Span,
    },

    /// `raise expr` — raise any value as an exception.
    Raise {
        value: Expr,
        span: Span,
    },

    /// `try { body } catch Type e { arm } ... catch e { arm }`
    TryCatch {
        body: Vec<Stmt>,
        arms: Vec<CatchArm>,
        span: Span,
    },

    /// `async fn name(param, param = default, ...) { body }`
    AsyncFnDef {
        name: String,
        /// Each entry is `(param_name, default_expr)`. Required params have `None`.
        params: Vec<(String, Option<Expr>)>,
        body: Vec<Stmt>,
        /// Each entry is `(decorator_name, positional_args)`.
        decorators: DecoratorList,
        span: Span,
    },

    /// A bare expression used as a statement, e.g. a method call whose return
    /// value is discarded: `obj.method(args)`.
    Expr(Expr),
}

/// An expression: what it is, and where the source wrote it.
///
/// One tree serves every stage. The parser builds it, the per-file pass in
/// `typechecker/resolve.rs` rewrites a few shapes in place (a pipe chain into
/// one `Pipe`, a converter around a dereference into a typed `PromptDeref`, a
/// class literal's defaults filled in), and the import pass, the whole-program
/// checker and MIR read the result. The checker keeps each value's type in its
/// own table, keyed by the node's address, so no node carries a type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

impl Expr {
    /// Purpose: This function builds an expression node.
    ///
    /// Inputs:
    /// - `kind`: what the expression is.
    /// - `span`: where it was written.
    ///
    /// Outputs: the node.
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Expr { kind, span }
    }
}

/// What an expression is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExprKind {
    /// `value is Class`: whether a value holds that class or a descendant.
    Is { value: Box<Expr>, class: String },

    /// An integer literal, e.g. `42`
    Integer(i64),

    /// A float literal, e.g. `3.14`
    Float(f64),

    /// A boolean literal, e.g. `true` or `false`
    Bool(bool),

    /// A string literal, e.g. `"hello"`
    Str(String),

    /// A reference to a variable or function, e.g. `add`
    Identifier(String),

    /// A function call, e.g. `add(1, 2)` or `f(x)`
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        /// Keyword arguments: `name = expr` pairs, e.g. `foo(a = 1, b = 2)`.
        #[serde(default)]
        kwargs: Vec<(String, Expr)>,
    },

    /// A binary operation, e.g. `1 + 1` or `a && b`
    BinOp { op: BinOpKind, left: Box<Expr>, right: Box<Expr> },

    /// A unary operation, e.g. `~x`, `!flag`, `-n`
    UnaryOp { op: UnaryOpKind, operand: Box<Expr> },

    /// A class literal, `Point { x: 1, y: 2 }`, or a copy of an existing value
    /// with some fields replaced: `Point { ...p, x: 99 }`.
    StructLiteral {
        type_name: String,
        /// The `...expr` of a copy-with literal, if there is one. At most one,
        /// and it comes first.
        ///
        /// It is carried through to MIR rather than expanded, so the expression
        /// is evaluated exactly once. Which fields it supplies depends on its
        /// class, which only the whole-program checker knows, so a field the
        /// literal leaves out is read from the base when the base's class has
        /// it and takes its declared default when it does not.
        base: Option<Box<Expr>>,
        /// Fields as `(name, value, is_prompt)`.
        ///
        /// The parser writes the fields the source named, each with
        /// `is_prompt` false. The per-file pass puts a literal of a class its
        /// file defines in definition order, marks prompt fields, and with no
        /// `base` adds the default of every field left out, so the list is
        /// complete; `fill_struct_literal_defaults` does the same for a class
        /// from another file once imports are merged. With a `base` it holds
        /// only the fields the source named.
        fields: Vec<(String, Expr, bool)>,
        /// The fields the source wrote, before any default was filled in. The
        /// per-file pass sets it; the parser leaves it empty. The class member
        /// rules read it: a literal outside a class may set only public fields,
        /// which `fields` alone cannot tell once defaults are in.
        #[serde(default)]
        given: Vec<String>,
    },

    /// Field access on a class value, e.g. `p.x` or `obj.method`
    FieldAccess { object: Box<Expr>, field: String },

    /// Indexing, e.g. `s[0]` or `d["k"]`
    Index { object: Box<Expr>, index: Box<Expr> },

    /// An array literal, e.g. `[1, 2, 3]` or `[]`
    Array { elements: Vec<Expr> },

    /// An interpolated string, e.g. `f"hello, {name}!"`
    FStr { parts: Vec<FStrPart> },

    /// `prompt <expr>` — produce a prompt value from a string expression.
    /// Expression form of the `prompt p = "text"` declaration; valid anywhere an
    /// expression is allowed, e.g. `let p = prompt "text"` inside a function.
    PromptLiteral { body: Box<Expr> },

    /// `?expr`: dereference a prompt, which runs the model. Called with a
    /// whole dereference as its one argument, a converter, a class or a
    /// grammar asks the model for that shape: `toInt(?p)`, `City(?p)`, `g(?p)`.
    /// The per-file pass folds such a call into this node's `output_type` or
    /// `grammar_expr`; the parser leaves both empty. Piped, `?p |> f` calls
    /// `f` once per piece of the reply as it arrives.
    PromptDeref {
        expr: Box<Expr>,
        /// The type a converter or class around the dereference names, `int`
        /// for `toInt(?p)` or `City` for `City(?p)`.
        output_type: Option<String>,
        /// The Grammar value a call around the dereference names, `g` for
        /// `g(?p)`, which constrains sampling.
        grammar_expr: Option<Box<Expr>>,
        /// Which surface syntax produced this node. Purely cosmetic: every
        /// form means the same thing and compiles identically; this only
        /// exists so that anything rendering the tree back to source can
        /// reproduce what was written.
        style: DerefStyle,
    },

    /// A dictionary literal, e.g. `{"key": 1, "other": 2}` or `{}`
    Dict { entries: Vec<(Expr, Expr)> },

    /// `await expr`
    Await { expr: Box<Expr> },

    /// `source |> f |> g(a)`, a whole chain: `body` runs once per piece of
    /// `source` as each piece arrives, with `piece` naming the piece in hand.
    ///
    /// `body` is the stages applied to the piece, `g(f(piece), a)`. A stage
    /// argument that is not a literal or a name is worked out once, before the
    /// first piece: it is in `hoisted`, bound to the name `body` uses. A source
    /// whose type is a stream gives its values as pieces, a call to a generator
    /// at the moment each is yielded; any other source is one piece. The
    /// per-file pass builds this from the parser's `RawPipe`. See
    /// `design/v2-pipes.md`.
    Pipe { source: Box<Expr>, hoisted: Vec<(String, Expr)>, piece: String, body: Box<Expr> },

    /// `value |> stage`, one pipe stage as the parser reads it: `a |> f |> g`
    /// nests as `RawPipe(RawPipe(a, f), g)`.
    ///
    /// Parser-only. The parser cannot build the flat `Pipe`, because the
    /// per-file pass checks each stage against what its name is first: a
    /// converter piped after a dereference, or a stage that is plainly not a
    /// function, is refused there with a message naming the fix. The per-file
    /// pass replaces every `RawPipe` with a `Pipe`, so no later stage sees
    /// one; a stage that does reports an internal error at its span.
    RawPipe { value: Box<Expr>, stage: Box<Expr> },
}

/// All binary operators Jade supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BinOpKind {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    // Bitwise
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    // Logical
    And,
    Or,
    // Comparison
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    // Membership
    In,
    NotIn,
}

/// Which surface form a `PromptDeref` was written in.  All three spellings parse
/// to the same node; this records only which one the source used, so anything
/// rendering the AST back to source can reproduce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DerefStyle {
    /// Prefix: `?p` — bare prompts only; never a field access.
    Prefix,
    /// Postfix: `obj.(?field)`
    DotParen,
    /// Postfix: `obj~>field`
    Squiggly,
}

/// All unary operators Jade supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UnaryOpKind {
    BitNot,
    Not,
    Neg,
}
