use super::error::Span;

/// A complete Jade program: a list of statements.
#[derive(Debug)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

/// A statement is a top-level action.
#[derive(Debug)]
#[allow(dead_code)]
pub enum Stmt {
    /// `let name = expr`
    Let {
        name: String,
        value: Expr,
        span: Span,
    },
}

/// An expression produces a value.
#[derive(Debug)]
pub enum Expr {
    /// An integer literal, e.g. `42`
    Integer {
        value: i64,
        span: Span,
    },

    /// A reference to a variable, e.g. `add`
    Identifier {
        name: String,
        span: Span,
    },

    /// A binary operation, e.g. `1 + 1` or `add * 2`
    BinOp {
        op: BinOpKind,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
}

/// The four arithmetic operators Jade supports at this stage.
#[derive(Debug)]
pub enum BinOpKind {
    Add,
    Sub,
    Mul,
    Div,
}
