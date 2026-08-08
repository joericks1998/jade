use super::error::Span;
use serde::{Deserialize, Serialize};

/// A method signature within an `interface` definition.
/// The type checker uses `name` for missing-method checks; `params` is parsed
/// for documentation and future type inference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceMethod {
    pub name: String,
    #[allow(dead_code)] // reserved for future type inference
    pub params: Vec<String>,
    #[allow(dead_code)] // reserved for future error reporting
    pub span: Span,
}

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Stmt {
    /// `let name = expr`
    Let {
        name: String,
        value: Expr,
        #[allow(dead_code)] // reserved for future error reporting
        span: Span,
    },

    /// `fn name(param, param = default, ...) { body }`
    FnDef {
        name: String,
        /// Each entry is `(param_name, default_expr)`. Required params have `None`.
        params: Vec<(String, Option<Expr>)>,
        body: Vec<Stmt>,
        /// Each entry is `(decorator_name, positional_args)`.
        decorators: Vec<(String, Vec<(Option<String>, Expr)>)>,
        #[allow(dead_code)] // reserved for future error reporting
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
        #[allow(dead_code)] // reserved for future error reporting
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
        #[allow(dead_code)]
        span: Span,
    },

    /// `struct Name { field, … }`
    StructDef {
        name: String,
        fields: Vec<StructFieldDef>,
        /// Each entry is `(decorator_name, positional_args)`.
        decorators: Vec<(String, Vec<(Option<String>, Expr)>)>,
        #[allow(dead_code)]
        span: Span,
    },

    /// `interface Name { fn method(self, …) -> type }`
    InterfaceDef {
        name: String,
        methods: Vec<InterfaceMethod>,
        #[allow(dead_code)]
        span: Span,
    },

    /// `extend TypeName { fn method(self, …) { … } … }`
    /// `extend TypeName: InterfaceName { fn method(self, …) { … } … }`
    ExtendBlock {
        type_name: String,
        /// The interface this extend block claims to implement, if any.
        interface_name: Option<String>,
        methods: Vec<Stmt>,
        decorators: Vec<(String, Vec<(Option<String>, Expr)>)>,
        #[allow(dead_code)]
        span: Span,
    },

    /// `object.field = expr` — mutate a field on a struct instance
    FieldAssign {
        object: String,
        field: String,
        value: Expr,
        #[allow(dead_code)]
        span: Span,
    },

    /// `name[index] = expr` — mutate an element of an array
    IndexAssign {
        name: String,
        index: Expr,
        value: Expr,
        #[allow(dead_code)]
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
        decorators: Vec<(String, Vec<(Option<String>, Expr)>)>,
        span: Span,
    },

    /// A bare expression used as a statement, e.g. a method call whose return
    /// value is discarded: `obj.method(args)`.
    Expr(Expr),
}

/// An expression produces a value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Expr {
    /// An integer literal, e.g. `42`
    Integer {
        value: i64,
        span: Span,
    },

    /// A float literal, e.g. `3.14`
    Float {
        value: f64,
        span: Span,
    },

    /// A boolean literal, e.g. `true` or `false`
    Bool {
        value: bool,
        span: Span,
    },

    /// A string literal, e.g. `"hello"`
    Str {
        value: String,
        span: Span,
    },

    /// A reference to a variable or function, e.g. `add`
    Identifier {
        name: String,
        span: Span,
    },

    /// A function call, e.g. `add(1, 2)` or `f(x)`
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        /// Keyword arguments: `name = expr` pairs, e.g. `foo(a = 1, b = 2)`.
        #[serde(default)]
        kwargs: Vec<(String, Expr)>,
        span: Span,
    },

    /// A binary operation, e.g. `1 + 1` or `a && b`
    BinOp {
        op: BinOpKind,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },

    /// A unary operation, e.g. `~x`, `!flag`, `-n`
    UnaryOp {
        op: UnaryOpKind,
        operand: Box<Expr>,
        span: Span,
    },

    /// A struct literal, e.g. `Point { x: 10, y: 20 }`
    StructLiteral {
        type_name: String,
        fields: Vec<(String, Expr)>,
        span: Span,
    },

    /// Field access on a struct, e.g. `p.x` or `obj.method`
    FieldAccess {
        object: Box<Expr>,
        field: String,
        span: Span,
    },

    /// Index into a string, e.g. `s[0]`
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },

    /// An array literal, e.g. `[1, 2, 3]` or `[]`
    Array {
        elements: Vec<Expr>,
        span: Span,
    },

    /// An interpolated string, e.g. `f"hello, {name}!"`
    FStr {
        parts: Vec<FStrPart>,
        span: Span,
    },

    /// `prompt <expr>` — produce a prompt value from a string expression.
    /// Expression form of the `prompt p = "text"` declaration; valid anywhere an
    /// expression is allowed, e.g. `let p = prompt "text"` inside a function.
    PromptLiteral {
        body: Box<Expr>,
        span: Span,
    },

    /// `?expr` — dereference a prompt, calling the LLM backend.
    /// `?expr |> TypeName`   — typed deref: coerces output to the named type with retry.
    /// `?expr |> grammar_expr` — grammar-constrained deref: expr must hold a Grammar value.
    /// `expr` must evaluate to `Value::Prompt`.  Prefix `?` covers bare prompts
    /// (`?name`); a prompt in a field uses `obj.(?field)` or `obj~>field`.
    PromptDeref {
        expr: Box<Expr>,
        /// The expression after `|>`, if any.  Resolved at type-inference time into either
        /// an output-type name (Str literal path) or a Grammar constraint expression.
        constraint: Option<Box<Expr>>,
        /// Which surface syntax produced this node.  Purely cosmetic — both forms
        /// mean the same thing and compile identically; this only exists so that
        /// anything rendering the AST back to source can reproduce what was written.
        style: DerefStyle,
        span: Span,
    },

    /// A dictionary literal, e.g. `{"key": 1, "other": 2}` or `{}`
    Dict {
        entries: Vec<(Expr, Expr)>,
        span: Span,
    },

    /// An anonymous function (closure), e.g. `|x| x * 2` or `|x, y| { x + y }`
    Closure {
        params: Vec<String>,
        body: Vec<Stmt>,
        span: Span,
    },

    /// `await expr`
    Await {
        expr: Box<Expr>,
        span: Span,
    },

    /// `value |> stage` — one pipe stage.
    ///
    /// The parser builds this for every `|>` and decides nothing about what the
    /// stage means; `compiler::type_infer::infer_pipe` classifies it. A stage is
    /// one of three things:
    ///
    /// - a **type name**, which on a prompt dereference constrains sampling with
    ///   a generated grammar and coerces the reply (`?p |> int`), and on any
    ///   other value is the ordinary type constructor (`x |> int` is `int(x)`);
    /// - a **Grammar value**, which constrains sampling (`?p |> gram`);
    /// - anything **callable**, which is applied with the piped value inserted
    ///   as the first argument (`5 |> double`, `5 |> add(3)`).
    ///
    /// Keeping the stage un-desugared is what lets a single `|>` mean all three.
    /// Until v1.2.0 the parser rewrote `value |> f` straight into `Expr::Call`
    /// and a *separate* parse path stored the stage on `PromptDeref.constraint`,
    /// so which rule applied was decided by surrounding syntax before anything
    /// knew what the names referred to. See `PromptDeref`.
    Pipe {
        value: Box<Expr>,
        stage: Box<Expr>,
        span: Span,
    },
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
