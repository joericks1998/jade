use super::error::Span;

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

    /// `extend TypeName { fn method(self, …) { … } … }`
    ExtendBlock {
        type_name: String,
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
