/// A position in source text. Line and column are 1-based (how editors report them).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

/// The one undefined name worth a sentence of its own.
///
/// Jade has no `exit`, and what happens when someone reaches for it is
/// misleading enough to be worth heading off. The name resolves to nothing, so
/// the call does nothing, so the program appears to run its function and then
/// carry on — which reads like broken control flow rather than a name that was
/// never there. An uncaught `raise` at the top level is the whole exit-code
/// mechanism the language has.
///
/// Shared with the AOT backend, which reports the same thing about a program
/// type inference had to stay lenient on (see `codegen::check_globals_bound`),
/// so the two say it the same way.
pub fn undefined_variable_hint(name: &str) -> &'static str {
    match name {
        "exit" => {
            "\n  Jade has no `exit`. An uncaught `raise` at the top level exits 1, \
             so write `if code != 0 { raise \"failed\" }`."
        }
        _ => "",
    }
}

/// What a failed field access was reaching into.
///
/// Carried rather than inferred from the type name, because `struct array {}`
/// is a legal declaration — so the name alone cannot tell a user's struct from
/// the built-in array, and guessing produces a confidently wrong message.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldOwner {
    /// A declared struct. Has fields; may have `extend` methods.
    Struct,
    /// A dict. Has keys, and the `dict` methods.
    Dict,
    /// Anything else — a str, an array, a number, a function. No fields at all,
    /// so the only thing a name here could have been is a method.
    Value,
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

    /// A stdlib package was asked for a function it does not have.
    UnknownPackageFn { package: String, name: String, available: Vec<String>, span: Span },

    /// Evaluator tried to look up a name that was never declared.
    UndefinedVariable { name: String, span: Span },

    /// Evaluator hit a divide-by-zero.
    DivisionByZero { span: Span },

    /// Evaluator hit a remainder-by-zero.
    RemainderByZero { span: Span },

    /// Evaluator received a negative or out-of-range shift amount.
    InvalidShift { amount: i64, span: Span },

    /// Evaluator applied an operator to an incompatible type.
    TypeError { message: String, span: Span },

    /// Lexer encountered a numeric literal that overflows its target type.
    LiteralOverflow { span: Span },

    /// Called a function with the wrong number of arguments.
    ArityMismatch { expected: usize, got: usize, span: Span },

    /// Tried to call a non-function value.
    NotCallable { span: Span },

    /// `return` used outside of a function body.
    ReturnOutsideFunction { span: Span },

    /// `yield` outside any function body.
    YieldOutsideFunction { span: Span },

    /// `break` with no enclosing loop.
    BreakOutsideLoop { span: Span },

    /// `continue` with no enclosing loop.
    ContinueOutsideLoop { span: Span },

    /// A function body mixes `yield` with a value-returning `return`. It cannot
    /// be both a stream producer and a plain function.
    YieldAndReturn { span: Span },

    /// `fn` definition found inside another function body.
    NestedFunction { span: Span },

    /// Integer arithmetic overflowed the i64 range.
    IntegerOverflow { span: Span },

    /// Tried to access or mutate a field on a non-struct value.
    NotAStruct { span: Span },

    /// Tried to access a field that does not exist on the struct type.
    UndefinedField { type_name: String, field: String, owner: FieldOwner, span: Span },

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

    /// No inference backend is available when a `?` dereference is attempted:
    /// no provider package is configured and no local daemon is running.
    NoInferenceBackend { span: Span },

    /// `?` applied to a variable that holds a non-prompt value.
    NotAPrompt { name: String, span: Span },

    /// Typed dereference `?p |> Type` exhausted its retry budget without producing a valid value.
    PromptOverflow { name: String, attempts: usize, span: Span },

    /// The right side of `|>` is neither callable, a type name, nor a Grammar.
    InvalidPipeStage { got: String, span: Span },

    /// Prefix `?` was applied to a field access (`?obj.field`).  Prefix `?` is for
    /// bare prompts only; dereferencing a prompt held in a field uses a postfix form.
    PrefixDerefOnField { field: String, span: Span },

    /// Dict lookup used a key that does not exist.
    KeyNotFound { key: String, span: Span },

    /// A `prompt` field's default or provided value is not a string.
    PromptFieldNotStr { field: String, span: Span },

    /// A struct literal supplies the same field name more than once.
    DuplicateField { field: String, span: Span },

    /// Type checker: an operator or construct received incompatible types.
    TypeMismatch { expected: String, got: String, span: Span },

    /// `use "path"` used the removed quoted-string import form. Imports name a
    /// module (`use utils`, `use sub::helper`, `use std::math`), never a file path.
    QuotedImport { path: String, span: Span },

    /// `use foo as bar` used the removed `as` alias. An import binds its module's
    /// last path segment automatically (`use sub::helper` binds `helper`).
    ImportAlias { span: Span },

    /// `use "path"` could not find the referenced file.
    ImportNotFound { path: String, span: Span },

    /// A call into a C-ABI dependency names a symbol its `[symbols]` table does
    /// not declare.
    ///
    /// This is the one FFI mistake that used to survive every stage. A symbol
    /// the shim never bound is not a link error — nothing links it — so a typo
    /// compiled, built, packaged, shipped, and failed the first time that line
    /// ran, as "dict has no key or method". The manifest already lists every
    /// symbol the dependency provides, so the answer was there to be read.
    UnknownFfiSymbol {
        /// The name the import bound, as written before the dot.
        module: String,
        symbol: String,
        /// The closest declared symbol, when one is close enough to suggest.
        suggestion: Option<String>,
        span: Span,
    },

    /// A spawned function mutates state its spawner can still reach. Tasks run
    /// concurrently on a shared heap with no lock on collection payloads, so
    /// this is a data race; see `compiler::taskcheck`.
    SharedMutation { task: String, what: String, span: Span },

    /// A handle was passed into a spawned function. Distinct from
    /// [`JadeError::SharedMutation`] because the fix is the opposite one: a
    /// handle is exactly what must *not* be passed in as a parameter, since
    /// nothing on the Jade side can make sharing it safe.
    HandleAcrossTask { type_name: String, span: Span },

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

    /// An error that originated inside an imported file.
    /// Wraps the inner error and records the import path for traceback display.
    InFile { file: String, cause: Box<JadeError> },

    /// A call chain recursed past the interpreter's call-depth limit. Raised
    /// instead of letting the native stack simply run out, which used to abort
    /// the process with an uncatchable Rust panic (`thread 'main' has
    /// overflowed its stack`) at a depth of roughly 700-750 — see
    /// `vm::MAX_CALL_DEPTH` and the AOT runtime's matching `JRT_RECUR_MAX_DEPTH`.
    RecursionLimitExceeded { span: Span },
}

impl std::fmt::Display for JadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JadeError::UnexpectedChar { ch, span } => write!(
                f,
                "[{}:{}] syntax error: unexpected character {:?}",
                span.line, span.col, ch
            ),
            JadeError::UnexpectedToken { expected, got, span } => write!(
                f,
                "[{}:{}] syntax error: expected {}, found {}",
                span.line, span.col, expected, got
            ),
            JadeError::UnexpectedEof { span } => write!(
                f,
                "[{}:{}] syntax error: unexpected end of file — did you forget a closing `}}`?",
                span.line, span.col
            ),
            JadeError::UnknownPackageFn { package, name, available, span } => {
                // Listing what the package *does* have is the whole value here:
                // there is no registry to go and read, the set is small, and the
                // mistake is nearly always a name from another language.
                write!(
                    f,
                    "[{}:{}] {} has no function '{}'\n  It provides: {}.",
                    span.line,
                    span.col,
                    package,
                    name,
                    available.join(", ")
                )
            }
            JadeError::UndefinedVariable { name, span } => {
                write!(
                    f,
                    "[{}:{}] undefined variable '{}'{}",
                    span.line,
                    span.col,
                    name,
                    undefined_variable_hint(name)
                )
            }
            JadeError::DivisionByZero { span } => {
                write!(f, "[{}:{}] division by zero", span.line, span.col)
            }
            JadeError::RemainderByZero { span } => {
                write!(f, "[{}:{}] remainder by zero", span.line, span.col)
            }
            JadeError::InvalidShift { amount, span } => {
                write!(f, "[{}:{}] invalid shift amount {}", span.line, span.col, amount)
            }
            JadeError::TypeError { message, span } => {
                write!(f, "[{}:{}] type error: {}", span.line, span.col, message)
            }
            JadeError::LiteralOverflow { span } => {
                write!(f, "[{}:{}] numeric literal overflows its type", span.line, span.col)
            }
            JadeError::ArityMismatch { expected, got, span } => write!(
                f,
                "[{}:{}] wrong number of arguments: expected {}, got {}",
                span.line, span.col, expected, got
            ),
            JadeError::NotCallable { span } => {
                write!(f, "[{}:{}] value is not callable", span.line, span.col)
            }
            JadeError::YieldOutsideFunction { span } => write!(
                f,
                "[{}:{}] 'yield' outside a function — it appends to the stream a function produces, so there must be one",
                span.line, span.col
            ),
            JadeError::YieldAndReturn { span } => write!(
                f,
                "[{}:{}] a function that yields cannot also return a value — it produces a stream, not a single value (a bare 'return' to stop early is fine)",
                span.line, span.col
            ),
            JadeError::ReturnOutsideFunction { span } => {
                write!(f, "[{}:{}] 'return' used outside of a function", span.line, span.col)
            }
            JadeError::BreakOutsideLoop { span } => write!(
                f,
                "[{}:{}] 'break' outside a loop — it leaves the innermost 'for' or 'while', so there must be one (a loop outside the enclosing function does not count)",
                span.line, span.col
            ),
            JadeError::ContinueOutsideLoop { span } => write!(
                f,
                "[{}:{}] 'continue' outside a loop — it starts the innermost 'for' or 'while' on its next iteration, so there must be one (a loop outside the enclosing function does not count)",
                span.line, span.col
            ),
            JadeError::NestedFunction { span } => {
                write!(f, "[{}:{}] function definitions cannot be nested", span.line, span.col)
            }
            JadeError::IntegerOverflow { span } => {
                write!(f, "[{}:{}] integer overflow", span.line, span.col)
            }
            JadeError::NotAStruct { span } => {
                write!(f, "[{}:{}] value is not a struct", span.line, span.col)
            }
            // Said "struct" for everything until v1.3.21, so a missing method
            // on an array reported `struct 'array' has no field 'map'` — three
            // wrong words about a value that is not a struct and has no fields.
            JadeError::UndefinedField { type_name, field, owner, span } => match owner {
                FieldOwner::Struct => write!(
                    f,
                    "[{}:{}] struct '{}' has no field '{}'",
                    span.line, span.col, type_name, field
                ),
                FieldOwner::Dict => {
                    write!(f, "[{}:{}] dict has no key or method '{}'", span.line, span.col, field)
                }
                FieldOwner::Value => {
                    write!(
                        f,
                        "[{}:{}] {} has no method '{}'",
                        span.line, span.col, type_name, field
                    )
                }
            },
            JadeError::UndefinedType { name, span } => {
                write!(f, "[{}:{}] undefined struct type '{}'", span.line, span.col, name)
            }
            JadeError::MissingField { field, span } => write!(
                f,
                "[{}:{}] missing required field '{}' in struct literal",
                span.line, span.col, field
            ),
            JadeError::UnterminatedString { span } => {
                write!(f, "[{}:{}] unterminated string literal", span.line, span.col)
            }
            JadeError::IndexOutOfBounds { index, len, span } => write!(
                f,
                "[{}:{}] index {} out of bounds (length {})",
                span.line, span.col, index, len
            ),
            JadeError::UndefinedInterface { name, span } => {
                write!(f, "[{}:{}] interface '{}' is not defined", span.line, span.col, name)
            }
            JadeError::MissingInterfaceMethod { type_name, interface_name, method, span } => {
                write!(
                    f,
                    "[{}:{}] type '{}' does not implement interface '{}': missing method '{}'",
                    span.line, span.col, type_name, interface_name, method
                )
            }
            JadeError::InferenceError { message, span } => {
                write!(f, "[{}:{}] inference error: {}", span.line, span.col, message)
            }
            JadeError::NoInferenceBackend { span } => write!(
                f,
                "[{}:{}] no inference backend available — run `jade register` to \
                    choose a provider and set your API key",
                span.line, span.col
            ),
            JadeError::NotAPrompt { name, span } => {
                write!(f, "[{}:{}] '{}' is not a prompt variable", span.line, span.col, name)
            }
            JadeError::PromptOverflow { name, attempts, span } => write!(
                f,
                "[{}:{}] prompt '{}' failed to produce a valid typed value after {} attempt(s)",
                span.line, span.col, name, attempts
            ),
            JadeError::InvalidPipeStage { got, span } => write!(
                f,
                "[{}:{}] '|>' needs a function, a type name, or a Grammar on its right; got {}",
                span.line, span.col, got
            ),
            JadeError::PrefixDerefOnField { field, span } => write!(
                f,
                "[{}:{}] prefix '?' cannot be applied to a field — write 'obj.(?{})' or 'obj~>{}' instead",
                span.line, span.col, field, field
            ),
            JadeError::KeyNotFound { key, span } => {
                write!(f, "[{}:{}] key '{}' not found in dict", span.line, span.col, key)
            }
            JadeError::PromptFieldNotStr { field, span } => write!(
                f,
                "[{}:{}] prompt field '{}' requires a string value",
                span.line, span.col, field
            ),
            JadeError::DuplicateField { field, span } => write!(
                f,
                "[{}:{}] field '{}' is specified more than once in struct literal",
                span.line, span.col, field
            ),
            JadeError::TypeMismatch { expected, got, span } => write!(
                f,
                "[{}:{}] type mismatch: expected {}, got {}",
                span.line, span.col, expected, got
            ),
            JadeError::QuotedImport { path, span } => {
                let dotted = path.trim_end_matches(".jde").replace('/', "::");
                write!(
                    f,
                    "[{}:{}] quoted file imports were removed. Import by module name with `::` notation: `use {}` (a sibling `.jde` file, a subdir with `sub::name`, a stdlib package like `std::math`, or a registered `[lib]`/dependency).",
                    span.line, span.col, dotted
                )
            }
            JadeError::ImportAlias { span } => write!(
                f,
                "[{}:{}] the `as` import alias was removed; an import binds its module's last path segment automatically (`use sub::helper` binds `helper`).",
                span.line, span.col
            ),
            JadeError::ImportNotFound { path, span } => write!(
                f,
                "[{}:{}] cannot find import '{}': file not found",
                span.line, span.col, path
            ),
            JadeError::UnknownFfiSymbol { module, symbol, suggestion, span } => {
                write!(f, "[{}:{}] '{}' has no symbol '{}'", span.line, span.col, module, symbol)?;
                match suggestion {
                    Some(s) => write!(f, " — did you mean '{}'?", s)?,
                    None => write!(
                        f,
                        ". Add it to [dependencies.{}.symbols] in jade.toml, \
                         or re-run `jade pkg bind {} --header <h>`",
                        module, module
                    )?,
                }
                Ok(())
            }
            JadeError::SharedMutation { task, what, span } => write!(
                f,
                "[{}:{}] async function '{}' {}\n  \
                 tasks run concurrently on a shared heap, so this is a data race\n  \
                 help: pass the value in as a parameter and return the result instead",
                span.line, span.col, task, what
            ),
            JadeError::HandleAcrossTask { type_name, span } => write!(
                f,
                "[{}:{}] cannot pass handle<{}> into a task\n  \
                 a handle is a pointer into a C library, and Jade cannot see what the library \
                 does with it or know whether it is thread-safe\n  \
                 help: open the handle inside the task and close it before returning",
                span.line, span.col, type_name
            ),
            JadeError::CircularImport { path, span } => write!(
                f,
                "[{}:{}] circular import detected: '{}' is already being imported",
                span.line, span.col, path
            ),
            JadeError::Exception { message, span } => {
                write!(f, "[{}:{}] unhandled exception: {}", span.line, span.col, message)
            }
            JadeError::NotAFuture { span } => {
                write!(f, "[{}:{}] 'await' applied to a non-Future value", span.line, span.col)
            }
            JadeError::DoubleAwait { span } => write!(
                f,
                "[{}:{}] cannot await the same Future more than once",
                span.line, span.col
            ),
            JadeError::AsyncPanic { message, span } => {
                write!(f, "[{}:{}] async task panicked: {}", span.line, span.col, message)
            }
            JadeError::IoError { message, span } => {
                write!(f, "[{}:{}] I/O error: {}", span.line, span.col, message)
            }
            JadeError::InFile { file, cause } => write!(f, "in \"{}\": {}", file, cause),
            JadeError::RecursionLimitExceeded { span } => {
                write!(f, "[{}:{}] recursion limit exceeded", span.line, span.col)
            }
        }
    }
}

/// Shorthand so every module can write `Result<T>` instead of `Result<T, JadeError>`.
pub type Result<T> = std::result::Result<T, JadeError>;
