use serde::{Deserialize, Serialize};

use crate::interpreter::{
    ast::{BinOpKind, InterfaceMethod, StructFieldDef, UnaryOpKind},
    error::Span,
};

// ── JadeType ──────────────────────────────────────────────────────────────────

/// The static type of a Jade value.
///
/// `Unknown` is a first-class type meaning "could not be determined statically".
/// It propagates through operations without triggering errors (conservative
/// checker — never a false positive). `Unknown` should not appear in a fully
/// resolved TIR; it marks gaps that Stage B does not yet handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JadeType {
    Int,
    Float,
    Bool,
    Str,
    Nil,
    Prompt,
    Array(Box<JadeType>),       // homogeneous; empty arrays are Array(Unknown)
    Dict,                       // keys and values are untyped at this stage
    Struct(String),             // named struct, e.g. Struct("Point")
    Fn {
        params: Vec<JadeType>,  // all Unknown for user fns (params are unannotated)
        ret: Box<JadeType>,
    },
    AsyncFn {
        params: Vec<JadeType>,
        ret: Box<JadeType>,
    },
    Future(Box<JadeType>),      // result of calling an AsyncFn
    Unknown,                    // type could not be statically determined
}

// ── TIR expression ────────────────────────────────────────────────────────────

/// A type-annotated expression node.
///
/// Every node carries its resolved `JadeType` and source `Span` for precise
/// error reporting in later compiler stages (bytecode emitter, LLVM codegen).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TExpr {
    pub kind: TExprKind,
    pub ty: JadeType,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TFStrPart {
    Literal(String),
    Expr(TExpr),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TExprKind {
    Integer(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Identifier(String),
    Call {
        callee: Box<TExpr>,
        args: Vec<TExpr>,
    },
    BinOp {
        op: BinOpKind,
        left: Box<TExpr>,
        right: Box<TExpr>,
    },
    UnaryOp {
        op: UnaryOpKind,
        operand: Box<TExpr>,
    },
    StructLiteral {
        type_name: String,
        /// All fields in definition order: `(name, value_expr, is_prompt)`.
        /// Includes defaults for optional fields not provided by the caller,
        /// resolved at type-inference time so the bytecode emitter sees a
        /// complete, self-contained field list.
        fields: Vec<(String, TExpr, bool)>,
    },
    FieldAccess {
        object: Box<TExpr>,
        field: String,
    },
    Index {
        object: Box<TExpr>,
        index: Box<TExpr>,
    },
    Array {
        elements: Vec<TExpr>,
    },
    FStr {
        parts: Vec<TFStrPart>,
    },
    PromptDeref {
        expr: Box<TExpr>,
        output_type: Option<String>,
    },
    Dict {
        entries: Vec<(TExpr, TExpr)>,
    },
    Closure {
        params: Vec<String>,
        body: Vec<TStmt>,
        /// Variables captured from enclosing scopes, in order.
        /// Each entry is (name, type_at_capture_site).
        captures: Vec<(String, JadeType)>,
    },
    Await {
        expr: Box<TExpr>,
    },
}

// ── TIR statements ────────────────────────────────────────────────────────────

/// A type-annotated statement node.
///
/// `StructDef` and `InterfaceDef` re-use their untyped AST equivalents directly
/// because their field/method definitions do not contain typed sub-expressions
/// that need annotation at this stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TStmt {
    Let {
        name: String,
        value: TExpr,
        span: Span,
    },
    Assign {
        name: String,
        value: TExpr,
        span: Span,
    },
    FnDef {
        name: String,
        params: Vec<String>,
        body: Vec<TStmt>,
        ret_ty: JadeType,
        span: Span,
    },
    Return {
        value: Option<TExpr>,
        span: Span,
    },
    If {
        condition: TExpr,
        then_body: Vec<TStmt>,
        else_body: Option<Vec<TStmt>>,
        span: Span,
    },
    While {
        condition: TExpr,
        body: Vec<TStmt>,
        span: Span,
    },
    For {
        var: String,
        iterable: TExpr,
        body: Vec<TStmt>,
        span: Span,
    },
    StructDef {
        name: String,
        fields: Vec<StructFieldDef>,
        span: Span,
    },
    InterfaceDef {
        name: String,
        methods: Vec<InterfaceMethod>,
        span: Span,
    },
    ExtendBlock {
        type_name: String,
        interface_name: Option<String>,
        methods: Vec<TStmt>,
        span: Span,
    },
    FieldAssign {
        object: String,
        field: String,
        value: TExpr,
        span: Span,
    },
    IndexAssign {
        name: String,
        index: TExpr,
        value: TExpr,
        span: Span,
    },
    PromptDecl {
        name: String,
        body: TExpr,
        span: Span,
    },
    /// `use "path"` — passed through unchanged from the AST; resolved at VM runtime.
    Use {
        path: String,
        span: Span,
    },
    /// `raise expr`
    Raise {
        value: TExpr,
        span: Span,
    },
    /// `try { body } catch [Type] binding { arm } …`
    TryCatch {
        body: Vec<TStmt>,
        arms: Vec<TCatchArm>,
        span: Span,
    },
    /// `async fn name(params) { body }`
    AsyncFnDef {
        name: String,
        params: Vec<String>,
        body: Vec<TStmt>,
        ret_ty: JadeType,
        span: Span,
    },
    Expr(TExpr),
}

/// A single typed catch arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TCatchArm {
    pub catch_type: Option<String>,
    pub binding: String,
    pub body: Vec<TStmt>,
}

// ── TIR program ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TProgram {
    pub stmts: Vec<TStmt>,
}
