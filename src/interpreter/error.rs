/// A position in source text. Line and column are 1-based (how editors report them).
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

/// Every error Jade can produce.
#[derive(Debug)]
#[allow(dead_code)]
pub enum JadeError {
    /// Lexer found a character it doesn't recognize.
    UnexpectedChar { ch: char, span: Span },

    /// Parser expected one thing but got something else.
    UnexpectedToken { expected: String, got: String, span: Span },

    /// Parser hit the end of the token stream unexpectedly.
    UnexpectedEof { span: Span },

    /// Evaluator tried to look up a name that was never declared.
    UndefinedVariable { name: String, span: Span },

    /// Evaluator hit a divide-by-zero.
    DivisionByZero { span: Span },

    /// Evaluator hit a remainder-by-zero.
    RemainderByZero { span: Span },

    /// Evaluator received a negative or out-of-range shift amount.
    InvalidShift { amount: i64, span: Span },
}

/// Shorthand so every module can write `Result<T>` instead of `Result<T, JadeError>`.
pub type Result<T> = std::result::Result<T, JadeError>;
