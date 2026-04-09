use super::error::Span;

/// A method signature within an `interface` definition.
/// The tree-walk evaluator uses the name only (for missing-method checks);
/// `params`/`return_type` are parsed for documentation and future type inference.
#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    pub name: String,
    #[allow(dead_code)] // reserved for future type inference
    pub params: Vec<String>,
    /// Return type annotation, e.g. `str` in `fn to_str(self) -> str`.
    #[allow(dead_code)] // reserved for future type inference
    pub return_type: Option<String>,
    #[allow(dead_code)] // reserved for future error reporting
    pub span: Span,
}

/// A part of an interpolated f-string expression.
#[derive(Debug, Clone)]
pub enum FStrPart {
    /// A literal string segment between (or before/after) interpolation slots.
    Literal(String),
    /// An interpolated expression: the value is stringified at runtime.
    Expr(Expr),
}

/// A complete Jade program: a list of statements.
#[derive(Debug, Clone)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

/// A statement is a top-level action.
#[derive(Debug, Clone)]
pub enum Stmt {
    /// `let name = expr`
    Let {
        name: String,
        value: Expr,
        #[allow(dead_code)] // reserved for future error reporting
        span: Span,
    },

    /// `fn name(param, ...) { body }`
    FnDef {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
        #[allow(dead_code)] // reserved for future error reporting
        span: Span,
    },

    /// `return expr` or bare `return`
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
        fields: Vec<String>,
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

    /// A bare expression used as a statement, e.g. a method call whose return
    /// value is discarded: `obj.method(args)`.
    Expr(Expr),
}

/// An expression produces a value.
#[derive(Debug, Clone)]
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

    /// `?name` — dereference a prompt variable, calling the LLM backend.
    /// `?name |> type` — typed dereference: coerces the LLM output to `type` with retry.
    PromptDeref {
        name: String,
        output_type: Option<String>,
        span: Span,
    },

    /// A dictionary literal, e.g. `{"key": 1, "other": 2}` or `{}`
    Dict {
        entries: Vec<(Expr, Expr)>,
        span: Span,
    },
}

/// All binary operators Jade supports.
#[derive(Debug, Clone)]
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
}

/// All unary operators Jade supports.
#[derive(Debug, Clone)]
pub enum UnaryOpKind {
    BitNot,
    Not,
    Neg,
}
