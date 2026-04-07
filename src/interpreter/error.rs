/// A position in source text. Line and column are 1-based (how editors report them).
#[derive(Debug, Clone, Copy)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

/// Every error Jade can produce.
#[derive(Debug)]
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

    /// Evaluator applied an operator to an incompatible type.
    TypeError { op: String, span: Span },

    /// Lexer encountered a numeric literal that overflows its target type.
    LiteralOverflow { span: Span },

    /// Called a function with the wrong number of arguments.
    ArityMismatch { expected: usize, got: usize, span: Span },

    /// Tried to call a non-function value.
    NotCallable { span: Span },

    /// `return` used outside of a function body.
    ReturnOutsideFunction { span: Span },

    /// `fn` definition found inside another function body.
    NestedFunction { span: Span },

    /// Integer arithmetic overflowed the i64 range.
    IntegerOverflow { span: Span },

    /// Tried to access or mutate a field on a non-struct value.
    NotAStruct { span: Span },

    /// Tried to access a field that does not exist on the struct type.
    UndefinedField { type_name: String, field: String, span: Span },

    /// Struct literal used an unknown type name.
    UndefinedType { name: String, span: Span },

    /// Struct literal is missing a required field.
    MissingField { field: String, span: Span },
}

impl std::fmt::Display for JadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JadeError::UnexpectedChar { ch, span } =>
                write!(f, "[{}:{}] unexpected character '{}'", span.line, span.col, ch),
            JadeError::UnexpectedToken { expected, got, span } =>
                write!(f, "[{}:{}] expected {}, found {}", span.line, span.col, expected, got),
            JadeError::UnexpectedEof { span } =>
                write!(f, "[{}:{}] unexpected end of file", span.line, span.col),
            JadeError::UndefinedVariable { name, span } =>
                write!(f, "[{}:{}] undefined variable '{}'", span.line, span.col, name),
            JadeError::DivisionByZero { span } =>
                write!(f, "[{}:{}] division by zero", span.line, span.col),
            JadeError::RemainderByZero { span } =>
                write!(f, "[{}:{}] remainder by zero", span.line, span.col),
            JadeError::InvalidShift { amount, span } =>
                write!(f, "[{}:{}] invalid shift amount {}", span.line, span.col, amount),
            JadeError::TypeError { op, span } =>
                write!(f, "[{}:{}] type error: operator '{}' applied to incompatible types", span.line, span.col, op),
            JadeError::LiteralOverflow { span } =>
                write!(f, "[{}:{}] numeric literal overflows its type", span.line, span.col),
            JadeError::ArityMismatch { expected, got, span } =>
                write!(f, "[{}:{}] wrong number of arguments: expected {}, got {}", span.line, span.col, expected, got),
            JadeError::NotCallable { span } =>
                write!(f, "[{}:{}] value is not callable", span.line, span.col),
            JadeError::ReturnOutsideFunction { span } =>
                write!(f, "[{}:{}] 'return' used outside of a function", span.line, span.col),
            JadeError::NestedFunction { span } =>
                write!(f, "[{}:{}] function definitions cannot be nested", span.line, span.col),
            JadeError::IntegerOverflow { span } =>
                write!(f, "[{}:{}] integer overflow", span.line, span.col),
            JadeError::NotAStruct { span } =>
                write!(f, "[{}:{}] value is not a struct", span.line, span.col),
            JadeError::UndefinedField { type_name, field, span } =>
                write!(f, "[{}:{}] struct '{}' has no field '{}'", span.line, span.col, type_name, field),
            JadeError::UndefinedType { name, span } =>
                write!(f, "[{}:{}] undefined struct type '{}'", span.line, span.col, name),
            JadeError::MissingField { field, span } =>
                write!(f, "[{}:{}] missing required field '{}' in struct literal", span.line, span.col, field),
        }
    }
}

/// Shorthand so every module can write `Result<T>` instead of `Result<T, JadeError>`.
pub type Result<T> = std::result::Result<T, JadeError>;
