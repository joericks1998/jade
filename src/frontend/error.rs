/// A position in source text. Line and column are 1-based (how editors report them).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
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

    /// Lexer hit the end of the file without finding a closing `"`.
    UnterminatedString { span: Span },

    /// String index is out of range.
    IndexOutOfBounds { index: i64, len: usize, span: Span },

    /// `extend Type: Interface` names an interface that was never defined.
    UndefinedInterface { name: String, span: Span },

    /// `extend Type: Interface` is missing a method required by the interface.
    MissingInterfaceMethod { type_name: String, interface_name: String, method: String, span: Span },

    /// LLM inference call failed (network error, HTTP error, or unexpected API response).
    InferenceError { message: String, span: Span },

    /// No API key or backend is configured when a `?` dereference is attempted.
    MissingApiKey { span: Span },

    /// `?` applied to a variable that holds a non-prompt value.
    NotAPrompt { name: String, span: Span },

    /// Typed dereference `?p |> Type` exhausted its retry budget without producing a valid value.
    PromptOverflow { name: String, attempts: usize, span: Span },

    /// `?p |> Type` was used inside `print(...)` where streaming output is expected.
    StreamingWithType { span: Span },

    /// Dict lookup used a key that does not exist.
    KeyNotFound { key: String, span: Span },

    /// A `prompt` field's default or provided value is not a string.
    PromptFieldNotStr { field: String, span: Span },

    /// A struct literal supplies the same field name more than once.
    DuplicateField { field: String, span: Span },

    /// Type checker: an operator or construct received incompatible types.
    TypeMismatch { expected: String, got: String, span: Span },

    /// Type checker: an array literal contains elements of different concrete types.
    HeterogeneousArray { first: String, got: String, span: Span },

    /// `use "path"` could not find the referenced file.
    ImportNotFound { path: String, span: Span },

    /// `use "path"` would create a cycle: `a` imports `b` which imports `a`.
    CircularImport { path: String, span: Span },

    /// An exception raised by `raise` that was not caught by any enclosing `try/catch`.
    /// `message` is the string representation of the raised value, captured at raise-site.
    Exception { message: String, span: Span },

    /// `await` applied to a value that is not a Future.
    NotAFuture { span: Span },

    /// The same Future was awaited more than once.
    DoubleAwait { span: Span },

    /// A spawned async task panicked (tokio JoinError).
    AsyncPanic { message: String, span: Span },

    /// A filesystem I/O operation failed.
    IoError { message: String, span: Span },
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
            JadeError::UnterminatedString { span } =>
                write!(f, "[{}:{}] unterminated string literal", span.line, span.col),
            JadeError::IndexOutOfBounds { index, len, span } =>
                write!(f, "[{}:{}] index {} out of bounds (length {})", span.line, span.col, index, len),
            JadeError::UndefinedInterface { name, span } =>
                write!(f, "[{}:{}] interface '{}' is not defined", span.line, span.col, name),
            JadeError::MissingInterfaceMethod { type_name, interface_name, method, span } =>
                write!(f, "[{}:{}] type '{}' does not implement interface '{}': missing method '{}'", span.line, span.col, type_name, interface_name, method),
            JadeError::InferenceError { message, span } =>
                write!(f, "[{}:{}] inference error: {}", span.line, span.col, message),
            JadeError::MissingApiKey { span } =>
                write!(f, "[{}:{}] no inference backend available — options:\n  \
                    1. start jade-tree (socket at $HOME/.jade/llm.sock)\n  \
                    2. set JADE_API_KEY and optionally run 'jade configure'", span.line, span.col),
            JadeError::NotAPrompt { name, span } =>
                write!(f, "[{}:{}] '{}' is not a prompt variable", span.line, span.col, name),
            JadeError::PromptOverflow { name, attempts, span } =>
                write!(f, "[{}:{}] prompt '{}' failed to produce a valid typed value after {} attempt(s)", span.line, span.col, name, attempts),
            JadeError::StreamingWithType { span } =>
                write!(f, "[{}:{}] typed dereference '?p |> Type' cannot be used inside print() — assign to a variable first", span.line, span.col),
            JadeError::KeyNotFound { key, span } =>
                write!(f, "[{}:{}] key '{}' not found in dict", span.line, span.col, key),
            JadeError::PromptFieldNotStr { field, span } =>
                write!(f, "[{}:{}] prompt field '{}' requires a string value", span.line, span.col, field),
            JadeError::DuplicateField { field, span } =>
                write!(f, "[{}:{}] field '{}' is specified more than once in struct literal", span.line, span.col, field),
            JadeError::TypeMismatch { expected, got, span } =>
                write!(f, "[{}:{}] type mismatch: expected {}, got {}", span.line, span.col, expected, got),
            JadeError::HeterogeneousArray { first, got, span } =>
                write!(f, "[{}:{}] heterogeneous array: first element is {}, found {}", span.line, span.col, first, got),
            JadeError::ImportNotFound { path, span } =>
                write!(f, "[{}:{}] cannot find import '{}': file not found", span.line, span.col, path),
            JadeError::CircularImport { path, span } =>
                write!(f, "[{}:{}] circular import detected: '{}' is already being imported", span.line, span.col, path),
            JadeError::Exception { message, span } =>
                write!(f, "[{}:{}] unhandled exception: {}", span.line, span.col, message),
            JadeError::NotAFuture { span } =>
                write!(f, "[{}:{}] 'await' applied to a non-Future value", span.line, span.col),
            JadeError::DoubleAwait { span } =>
                write!(f, "[{}:{}] cannot await the same Future more than once", span.line, span.col),
            JadeError::AsyncPanic { message, span } =>
                write!(f, "[{}:{}] async task panicked: {}", span.line, span.col, message),
            JadeError::IoError { message, span } =>
                write!(f, "[{}:{}] I/O error: {}", span.line, span.col, message),
        }
    }
}

/// Shorthand so every module can write `Result<T>` instead of `Result<T, JadeError>`.
pub type Result<T> = std::result::Result<T, JadeError>;
