//! MIR: the typed middle layer between the checked program and LLVM.
//!
//! The checker in `compiler::infer` gives every value one type and every call
//! one target. MIR writes that down as code a backend can lower without asking
//! any question about types: one function per checked instance, every local
//! with its type, control flow as basic blocks, and reference counting as
//! explicit instructions.
//!
//! ## Shape
//!
//! A function is a list of typed locals and a list of blocks. The first
//! `params` locals are its parameters. A block is straight-line statements
//! followed by one terminator. Values move through locals and globals
//! (`Place`), and an instruction reads its inputs as `Operand`s: a place, a
//! constant, or a temporary being handed over.
//!
//! ```text
//! fn add(int, int) -> int
//!   bb0:
//!     _2 = add.int _0, _1
//!     return _2
//! ```
//!
//! ## Ownership
//!
//! Every heap value carries a reference count, and every place holding one owns
//! one reference. The rules the backend applies are few:
//!
//! - `Assign` computes its value, takes a reference when the value was only
//!   borrowed (a copy of a place, a field read, an array element), releases the
//!   place's old value, then stores.
//! - `Operand::Move` hands over a temporary's reference and clears it; every
//!   other operand in a storing position is retained.
//! - A function retains its heap parameters on entry and releases every local
//!   on return, so a parameter behaves like any other local.
//! - `Drop` releases a place and clears it. The lowering drops each statement's
//!   temporaries when the statement ends.
//!
//! Releasing a cleared (null) place does nothing, which is what lets a place be
//! dropped on every path without tracking which paths set it.

mod display;
pub mod lower;
mod ops;
mod packages;
pub mod taskcheck;

#[cfg(test)]
mod tests;

pub use crate::compiler::infer::Ty;
pub use lower::{lower_program, lower_program_with};

/// Purpose: This struct names a local of the current function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Local(pub u32);

/// Purpose: This struct names a block of the current function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

/// Purpose: This struct names a function of the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

/// Purpose: This struct names a global variable of the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlobalId(pub u32);

/// Purpose: This struct holds a whole lowered program.
#[derive(Debug, Clone)]
pub struct Program {
    pub funcs: Vec<Func>,
    pub globals: Vec<Global>,
    /// Every struct type, in type-id order: a struct's id is its index here.
    pub structs: Vec<StructDef>,
    /// The function holding the program's top-level statements.
    pub main: FuncId,
    /// Each class with a destructor chain, with the `drop` functions a value
    /// of it runs when it is freed, in order.
    pub drops: Vec<(String, Vec<FuncId>)>,
    /// The native packages the program calls, which it loads the first time
    /// it calls each.
    pub natives: Vec<NativeLib>,
}

/// Purpose: This struct says where a native package is found when the program
/// runs.
#[derive(Debug, Clone)]
pub struct NativeLib {
    /// The id its calls name.
    pub id: u32,
    /// The name the program imports it by, for messages.
    pub name: String,
    /// Its path within the project's `libs/` directory, which the program
    /// looks for beside itself or under `JADE_LIBS`; `None` for a library
    /// outside `libs/`.
    pub rel: Option<String>,
    /// Where it was when the program was built.
    pub abs: String,
}

/// Purpose: This struct describes one call into a native package: the
/// function and the static types its arguments are sent as and its result is
/// read back as.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeCall {
    pub pkg: u32,
    pub func: String,
    /// How the program writes the call, `handlefix.open`, for messages.
    pub display: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
}

/// Purpose: This struct holds one struct type's layout.
#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    /// Every field in layout order, inherited ones first.
    pub fields: Vec<(String, Ty)>,
    /// Every ancestor, nearest first.
    pub ancestors: Vec<String>,
}

/// Purpose: This struct holds one global variable.
#[derive(Debug, Clone)]
pub struct Global {
    pub name: String,
    pub ty: Ty,
}

/// Purpose: This struct holds one function: one checked instance of a `fn`,
/// a method, or the top level.
#[derive(Debug, Clone)]
pub struct Func {
    /// A readable name, unique in the program, used for the symbol.
    pub name: String,
    /// How many of the first locals are parameters.
    pub params: u32,
    pub locals: Vec<LocalDecl>,
    pub ret: Ty,
    pub blocks: Vec<Block>,
}

/// Purpose: This struct holds one local's type and, for a variable, its name.
#[derive(Debug, Clone)]
pub struct LocalDecl {
    pub ty: Ty,
    pub name: Option<String>,
}

/// Purpose: This struct holds one basic block.
#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Where in the source each statement came from, one per statement, for
    /// the passes that report on MIR, such as `taskcheck`.
    pub spans: Vec<crate::frontend::error::Span>,
    pub term: Term,
}

impl Block {
    /// Purpose: This function makes a block with no statements.
    ///
    /// Inputs:
    /// - `term`: how it ends.
    ///
    /// Outputs: the block.
    pub fn new(term: Term) -> Self {
        Block { stmts: Vec::new(), spans: Vec::new(), term }
    }
}

/// Purpose: This enum names somewhere a value is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    Local(Local),
    Global(GlobalId),
}

/// Purpose: This enum holds an instruction's input.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// The value in a place, borrowed.
    Copy(Place),
    /// A temporary's value, handed over: the temporary is cleared. Only ever
    /// appears where the value is stored.
    Move(Local),
    Const(Const),
}

/// Purpose: This enum holds a constant value.
#[derive(Debug, Clone, PartialEq)]
pub enum Const {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(String),
    /// The absent value of a type that has one: `nil` itself, or an optional.
    Nil(Ty),
    /// A value whose type says everything about it: a named function or a
    /// builtin used as a value, which is known when the program compiles and
    /// is called directly wherever it ends up.
    Static(Ty),
}

/// Purpose: This enum holds one statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// Compute a value and store it in a place.
    Assign(Place, Rvalue),
    /// Compute a value for its effect and release it.
    Eval(Rvalue),
    /// Store a value in a struct's field: `object.fields[index] = value`.
    /// `strukt` is the object's static type.
    SetField { object: Operand, strukt: String, index: u32, value: Operand },
    /// Store a value in a dict held by a place, copying the dict first when
    /// another place shares it. This is what gives a dict value semantics.
    DictSet { dict: Place, key: Operand, value: Operand },
    /// Release a place's value and clear it.
    Drop(Place),
    /// Raise a class value: put it in this thread's error slot. Control goes
    /// to the handler the lowering names next, by a terminator.
    Raise(Operand),
}

/// Purpose: This enum holds the right-hand side of an assignment.
#[derive(Debug, Clone, PartialEq)]
pub enum Rvalue {
    Use(Operand),
    Bin(BinOp, Operand, Operand),
    Un(UnOp, Operand),
    /// A direct call. Arguments are borrowed; the callee retains what it keeps.
    Call(FuncId, Vec<Operand>),
    /// A support operation; see [`Op`].
    Op(Op, Vec<Operand>),
    /// A new array of this element type.
    Array(Ty, Vec<Operand>),
    /// A new dict of this value type, from key and value pairs.
    Dict(Ty, Vec<(Operand, Operand)>),
    /// A new struct: every field in layout order.
    Struct(String, Vec<Operand>),
    /// Read a field. `strukt` is the object's static type.
    Field {
        object: Operand,
        strukt: String,
        index: u32,
    },
    /// Make `T?` from a `T`. `ty` is the optional type.
    Wrap(Ty, Operand),
    /// Read the `T` out of a `T?` already known not to be nil.
    Unwrap(Operand),
    /// Whether a `T?` (or `nil`) is absent.
    IsNil(Operand),
    /// An int as a float.
    IntToFloat(Operand),
    /// The type id of a struct value, as an int.
    TypeId(Operand),
    /// Whether a struct value's type id is one of these: the class `x is C`
    /// names and every class that descends from it.
    IsA(Operand, Vec<i64>),
    /// Start an async function as a task: `-> future of T`, where `T` is the
    /// function's return type. Arguments are kept by the task.
    Spawn(FuncId, Vec<Operand>),
    /// A pointer to a function, for a value of a joined function type.
    FnAddr(FuncId),
    /// A call through a function pointer whose type is the callee operand's.
    CallIndirect(Operand, Vec<Operand>),
    /// Whether an error has been raised and not yet caught on this thread.
    ErrPending,
    /// Take the raised error out of this thread's slot, owned, clearing it.
    TakeErr,
}

/// Purpose: This enum lists the number types arithmetic works on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumTy {
    Int,
    Float,
}

/// Purpose: This enum lists the arithmetic operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// Purpose: This enum lists the bitwise operators, all on ints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    And,
    Or,
    Xor,
    Shl,
    Shr,
}

/// Purpose: This enum lists the comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Purpose: This enum lists the binary operators. Both operands have the same
/// type, except that a shift amount is always an int.
#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
    /// Int arithmetic raises on overflow and on division by zero; float
    /// division raises on division by zero, as v1 did.
    Arith(ArithOp, NumTy),
    Bit(BitOp),
    /// Compares two values of the type given. Ordering exists for numbers,
    /// strings, chars and bools; equality for every type.
    Cmp(CmpOp, Ty),
    /// Joins two strings.
    Concat,
}

/// Purpose: This enum lists the unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// Int negation raises on overflow.
    Neg(NumTy),
    Not,
    BitNot,
}

/// Purpose: This enum lists the support operations: everything a program does
/// to a value that is not plain arithmetic.
///
/// Each says what its operands are. Results are owned unless
/// [`Op::borrows_result`] says otherwise.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // ── output and input ─────────────────────────────────────────────────
    /// `(text: str, end: str) -> nil`: write both to standard output.
    Print,
    /// `(prompt: str) -> str`: write the prompt, read one line.
    Input,

    // ── text ─────────────────────────────────────────────────────────────
    /// `(v: T) -> str`: the text `print` shows for a value of type `T`.
    ToStr(Ty),
    /// `(parts: str...) -> str`: every part joined, in one allocation.
    StrCat,
    /// `(c: char) -> str`.
    CharToStr,
    /// `(s: str) -> int`: the number of characters.
    StrLen,
    /// `(s: str, i: int) -> char`: the character at a character index.
    StrIndex,
    /// `(s: str) -> [char]`: every character, for a `for` loop over a string.
    StrChars,
    /// A string method; see [`StrMethod`].
    Str(StrMethod),

    // ── conversions ──────────────────────────────────────────────────────
    /// `(v: T) -> int`, for `int(v)`.
    ToInt(Ty),
    /// `(v: T) -> float`, for `float(v)`.
    ToFloat(Ty),
    /// `(v: T) -> bool`, for `bool(v)`.
    ToBool(Ty),
    /// `(v: T) -> char`, for `char(v)`.
    ToChar(Ty),

    // ── arrays (the element type is given) ───────────────────────────────
    /// `(a: [T]) -> int`.
    ArrayLen,
    /// `(a: [T], i: int) -> T`, borrowed. Raises when out of bounds.
    ArrayGet(Ty),
    /// `(a: [T], i: int, v: T) -> nil`. Raises when out of bounds.
    ArraySet(Ty),
    /// `(a: [T], v: T) -> nil`.
    ArrayPush(Ty),
    /// `(a: [T]) -> T`. Raises when empty.
    ArrayPop(Ty),
    /// `(a: [T], v: T) -> bool`.
    ArrayContains(Ty),
    /// `(a: [T]) -> nil`: sort in place.
    ArraySort(Ty),
    /// `(a: [T]) -> nil`: reverse in place.
    ArrayReverse(Ty),
    /// `(a: [T]) -> [T]`: a sorted copy, for `array.sort(a)`.
    ArraySorted(Ty),
    /// `(a: [T]) -> [T]`: a reversed copy, for `array.reverse(a)`.
    ArrayReversed(Ty),
    /// `(a: [T], sep: str) -> str`: each element's text, joined.
    ArrayJoin(Ty),

    // ── dicts (the value type is given) ──────────────────────────────────
    /// `(d: {str: V}) -> int`.
    DictLen,
    /// `(d: {str: V}, k: str) -> V`, borrowed. Raises when the key is missing.
    DictGet(Ty),
    /// `(d: {str: V}, k: str) -> V?`.
    DictGetOpt(Ty),
    /// `(d: {str: V}, k: str) -> bool`.
    DictHas(Ty),
    /// `(d: {str: V}) -> [str]`, sorted.
    DictKeys(Ty),
    /// `(d: {str: V}) -> [V]`, in sorted key order.
    DictValues(Ty),
    /// `(a: {str: V}, b: {str: V}) -> {str: V}`: `a` with `b`'s entries over it.
    DictMerge(Ty),

    /// `() -> nil`: raise a `RuntimeError` with this message.
    Fail(String),
    /// `(e: error) -> str`: a caught error's message: a `RuntimeError`'s, or
    /// the `message` field of a class that has one, or its printed form.
    ErrorMessage,

    // ── tasks (the result type is given) ─────────────────────────────────
    /// `(f: future of T) -> T`: wait for a task, then its value or its error.
    Await(Ty),
    /// `(f…: future of T) -> [T]`: await each in turn, raising the first error.
    Join(Ty),
    /// `(f…: future of T) -> [S]`: await each, building the `Settled` class `S`
    /// for every outcome instead of raising.
    JoinSettled(Ty, String),
    /// `(f…: futures) -> int`: the lowest index of a task that has finished,
    /// waiting until one has. The futures may have different result types.
    Wait,
    /// `(fs: [future of T]) -> int`: `Wait` over an array.
    WaitArray(Ty),
    /// `(f: future) -> bool`: whether the task has finished.
    Ready,
    /// `(f: future) -> nil`: ask the task to stop.
    Cancel,
    /// `() -> bool`: whether the running task was asked to stop.
    Cancelled,
    /// `() -> int`: the concurrency limit.
    MaxTasks,
    /// `(n: int) -> int`: set the concurrency limit, clamped to 1..=512.
    SetMaxTasks,
    /// `(secs: float) -> future of nil`: a task that finishes after a delay.
    After,

    // ── time ─────────────────────────────────────────────────────────────
    /// `() -> int`: Unix seconds, from the wall clock.
    Now,
    /// `() -> int`: Unix milliseconds, from the wall clock.
    NowMs,
    /// `() -> float`: seconds on a clock that never moves backwards.
    Monotonic,
    /// `(secs: float) -> nil`: pause the running thread.
    Sleep,
    /// `(ts: int) -> str`: ISO 8601 in UTC.
    TimeUtc,
    /// `(ts: int) -> TimeParts`: UTC calendar fields.
    TimeParts,
    /// `(y, mo, d, h, mi, s: int) -> int`: the inverse of `TimeParts`.
    TimeStamp,
    /// `(tz: str?) -> str`: the local time, formatted as `date` formats it,
    /// in a named zone or the system's. It is tainted, as in v1.
    TimeLocal,

    // ── bytes ────────────────────────────────────────────────────────────
    /// `(b: bytes, i: int) -> int`: one octet.
    BytesGet,
    /// `(b: bytes, i: int, v: int) -> nil`: write one octet in place.
    BytesSet,
    /// `(b: bytes) -> int`.
    BytesLen,
    /// `(b: bytes) -> str`: the octets as UTF-8, which must be valid.
    BytesDecode,
    /// `(b: bytes, start: int, end: int) -> bytes`: a copy, both ends clamped.
    BytesSlice,
    /// `(s: str) -> bytes`: the string's UTF-8 octets.
    StrEncode,
    /// `(n: int) -> bytes`: `n` zero octets.
    BytesZeros,
    /// `(a: [int]) -> bytes`: each int an octet in 0 to 255.
    BytesFromInts,
    /// `(a: bytes, b: bytes) -> bytes`.
    BytesConcat,

    // ── math ─────────────────────────────────────────────────────────────
    /// A `std::math` function on numbers whose kinds are settled; see [`MathFn`].
    Math(MathFn),

    // ── files ────────────────────────────────────────────────────────────
    /// A `std::fs` function; see [`FsFn`].
    Fs(FsFn),

    // ── the shell ────────────────────────────────────────────────────────
    /// A `std::sh` function; see [`ShFn`].
    Sh(ShFn),

    // ── env, path and random ─────────────────────────────────────────────
    /// A `std::env` function; see [`EnvFn`].
    Env(EnvFn),
    /// A `std::path` function; see [`PathFn`].
    Path(PathFn),
    /// A `std::random` function; see [`RandomFn`].
    Random(RandomFn),

    // ── json ─────────────────────────────────────────────────────────────
    /// `std::json`, or an operation on a `Json` value; see [`JsonFn`].
    Json(JsonFn),
    /// `(b: bytes) -> [int]`: each octet, for a `for` loop over bytes.
    BytesInts,

    // ── http ─────────────────────────────────────────────────────────────
    /// An `std::http` request, or with `true` an `std::uhttp` one; see
    /// [`HttpFn`]. The operands are the url, the body when the method has one,
    /// and the headers, a `{str}` or nil.
    Http(HttpFn, bool),
    /// A call into a native package, with arguments marshalled by their
    /// static types; see [`NativeCall`].
    Native(Box<NativeCall>),
    /// `(url: str, handler: fn(str) -> T, headers: {str}?) -> nil`:
    /// `uhttp.stream`, calling the handler once a line until the stream ends
    /// or the handler returns `false`. `T` is the handler's result type.
    UhttpStream(Ty),
}

/// Purpose: This enum lists the request functions `std::http` and `std::uhttp`
/// share. A transport failure raises; a 4xx or 5xx is an ordinary status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpFn {
    Get,
    Delete,
    Head,
    Post,
    Put,
    /// Like `Get` and `Post`, with a `bytes` body.
    GetBytes,
    PostBytes,
}

impl HttpFn {
    /// Purpose: This function returns the HTTP method.
    pub fn method(self) -> &'static str {
        match self {
            HttpFn::Get | HttpFn::GetBytes => "GET",
            HttpFn::Delete => "DELETE",
            HttpFn::Head => "HEAD",
            HttpFn::Post | HttpFn::PostBytes => "POST",
            HttpFn::Put => "PUT",
        }
    }

    /// Purpose: This function says whether the request sends a body.
    pub fn has_body(self) -> bool {
        matches!(self, HttpFn::Post | HttpFn::Put | HttpFn::PostBytes)
    }

    /// Purpose: This function returns the class the response comes back as.
    pub fn result(self) -> Ty {
        match self {
            HttpFn::GetBytes => Ty::Struct("HttpBytesResponse".into()),
            _ => Ty::Struct("HttpResponse".into()),
        }
    }
}

/// Purpose: This enum lists `std::json` and what a `Json` value supports. A
/// `Json` value carries its kind; one of the wrong kind raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonFn {
    /// `(s: str) -> json`: raises `I/O error: json.parse: …` on bad input.
    Parse,
    /// `(v: T) -> str`: compact JSON; the operand's type picks the writer.
    Stringify,
    /// `(v: T) -> str`: two-space indented JSON.
    StringifyPretty,
    /// `(j: json, key: str) -> json`: an object's member.
    Get,
    /// `(j: json, i: int) -> json`: an array's element.
    At,
    /// `(j: json) -> int`: an array's, object's or string's length.
    Len,
    /// `(j: json) -> [json]`: what a `for` loop walks: an array's elements,
    /// an object's keys or a string's characters.
    Items,
    /// `(j: json) -> int`, `-> float`, `-> bool`: `int(j)` and friends.
    ToInt,
    ToFloat,
    ToBool,
}

/// Purpose: This enum lists the `std::env` functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvFn {
    /// `(name: str) -> str?`: a variable's value, tainted, or nil.
    Get,
    /// `(name: str, value: str) -> nil`.
    Set,
    /// `() -> [str]`: the program's arguments, its own path first.
    Args,
    /// `() -> str`: the working directory.
    Cwd,
}

/// Purpose: This enum lists the `std::path` functions, which follow Rust's
/// `Path` on Unix and give results the trust of their inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathFn {
    /// `(a: str, b: str) -> str`: `b` after `a`; an absolute `b` replaces it.
    Join,
    /// `(p: str) -> str`: the last component, or "".
    Basename,
    /// `(p: str) -> str`: everything before it, or ".".
    Dirname,
    /// `(p: str) -> str`: the last component without its extension.
    Stem,
    /// `(p: str) -> str?`: the extension with its dot, or nil.
    Ext,
    /// `(p: str) -> str`: an absolute form, without resolving links.
    Abs,
    /// `(p: str) -> bool`.
    IsAbs,
}

/// Purpose: This enum lists the `std::random` operations: a xoshiro256**
/// generator shared by the whole program, seeded from the clock until
/// `random.seed` is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomFn {
    /// `(lo: int, hi: int) -> int`, both ends included; `lo > hi` raises.
    Int,
    /// `() -> float` in `[0, 1)`.
    Float,
    /// `(n: int) -> int` in `[0, n)`, for `random.choice`; 0 raises.
    Index,
    /// `(a: [T]) -> nil`: shuffle in place.
    Shuffle,
    /// `(n: int) -> nil`.
    Seed,
}

/// Purpose: This enum lists the `std::sh` functions. Each runs `sh -c cmd`,
/// refuses a tainted command, and gives back tainted output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShFn {
    /// `(cmd: str) -> str`: stdout without trailing newlines; a non-zero exit
    /// raises with stderr in the message.
    Exec,
    /// `(cmd: str) -> int`: the exit code, with output going to the program's.
    Run,
    /// `(cmd: str) -> ShOutput`: stdout, stderr and the code.
    Output,
}

impl ShFn {
    /// Purpose: This function returns the type a `std::sh` function returns.
    pub fn result(self) -> Ty {
        match self {
            ShFn::Exec => Ty::Str,
            ShFn::Run => Ty::Int,
            ShFn::Output => Ty::Struct("ShOutput".into()),
        }
    }
}

/// Purpose: This enum lists the `std::fs` functions. A failure raises
/// `I/O error: <op> '<path>': <reason> (os error <n>)`, as v1 did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsFn {
    /// `(path: str, trust: bool) -> str`: the file as UTF-8 text, tainted
    /// unless `trust` is true. A tainted path is refused unless `trust` is.
    Read,
    /// `(path: str, trust: bool) -> bytes`: the same, as octets.
    ReadBytes,
    /// `(path: str, text: str) -> nil`.
    Write,
    Append,
    /// `(path: str, data: bytes) -> nil`.
    WriteBytes,
    AppendBytes,
    /// `() -> bytes`: all of standard input, tainted.
    ReadStdinBytes,
    /// `(data: bytes) -> nil`.
    WriteStdoutBytes,
    /// `(path: str) -> bool`; these never raise.
    Exists,
    IsFile,
    IsDir,
    /// `(path: str) -> nil`.
    Delete,
    /// Makes missing parents too, and succeeds when the directory exists.
    Mkdir,
    /// Removes an empty directory only.
    Rmdir,
    /// `(path: str) -> [str]`: entry names, without `.` and `..`.
    ListDir,
    /// `(path: str) -> int`: the size in bytes.
    Size,
    /// `(src: str, dst: str) -> nil`.
    Copy,
    Rename,
}

impl FsFn {
    /// Purpose: This function returns the type a `std::fs` function returns.
    pub fn result(self) -> Ty {
        match self {
            FsFn::Read => Ty::Str,
            FsFn::ReadBytes | FsFn::ReadStdinBytes => Ty::Bytes,
            FsFn::Exists | FsFn::IsFile | FsFn::IsDir => Ty::Bool,
            FsFn::ListDir => Ty::Array(Box::new(Ty::Str)),
            FsFn::Size => Ty::Int,
            _ => Ty::Nil,
        }
    }
}

/// Purpose: This enum lists the `std::math` operations, each for one set of
/// operand kinds. The lowering picks the variant from the argument types, and
/// gives an int argument to a float-only function as a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathFn {
    /// `(x: float) -> int`, saturating as v1's `as i64` did.
    Floor,
    Ceil,
    /// Halves round away from zero.
    Round,
    Trunc,
    /// `(x: float) -> float`.
    Sqrt,
    Ln,
    Log2,
    Log10,
    Exp,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    /// `(a: float, b: float) -> float`.
    Atan2,
    Hypot,
    Pow,
    /// `(base: float, exp: int) -> float`.
    PowI,
    /// `(base: int, exp: int) -> int`; overflow or a negative exponent raises.
    PowInt,
    /// `(a: int, b: int) -> int`.
    MinInt,
    MaxInt,
    /// `(a: float, b: float) -> float`; a NaN operand gives the other.
    MinFloat,
    MaxFloat,
    /// `(x: int) -> int`; the most negative int raises.
    AbsInt,
    AbsFloat,
    /// `(x: int) -> int`: -1, 0 or 1.
    SignInt,
    /// `(x: float) -> float`: -1.0, 0.0 or 1.0, and NaN for NaN.
    SignFloat,
    /// `(x: float) -> bool`.
    IsNan,
    IsInf,
}

/// Purpose: This enum lists the string methods the static backend has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrMethod {
    Upper,
    Lower,
    Capitalize,
    Trim,
    TrimStart,
    TrimEnd,
    /// `(s, sep) -> [str]`.
    Split,
    /// `(s) -> [str]`.
    Lines,
    /// `(s, sub) -> bool`.
    Contains,
    StartsWith,
    EndsWith,
    /// `(s, sub) -> int`, a character index or -1.
    IndexOf,
    LastIndexOf,
    /// `(s, sub) -> int`: non-overlapping occurrences.
    Count,
    /// `(s, from, to) -> str`.
    Replace,
    /// `(s, n) -> str`.
    Repeat,
    /// `(s, start, end) -> str`, by character index.
    Slice,
    /// `(s, width, pad) -> str`.
    PadStart,
    PadEnd,
    /// `(s) -> bool`.
    IsEmpty,
}

impl Op {
    /// Purpose: This function says whether an operation's result is a borrowed
    /// reference to a value something else owns.
    ///
    /// Inputs: none.
    ///
    /// Outputs: true for element and value reads, whose result the assignment
    /// must retain.
    pub fn borrows_result(&self) -> bool {
        matches!(self, Op::ArrayGet(_) | Op::DictGet(_))
    }
}

impl Op {
    /// Purpose: This function says whether an operation can raise.
    ///
    /// Inputs: none.
    ///
    /// Outputs: true when it can fail, such as an index past the end.
    pub fn may_raise(&self) -> bool {
        matches!(
            self,
            Op::StrIndex
                | Op::ToInt(Ty::Str)
                | Op::ToFloat(_)
                | Op::ToChar(_)
                | Op::ArrayGet(_)
                | Op::ArraySet(_)
                | Op::ArrayPop(_)
                | Op::DictGet(_)
                | Op::Fail(_)
                | Op::Await(_)
                | Op::Join(_)
                | Op::Math(MathFn::PowInt | MathFn::AbsInt)
                | Op::BytesGet
                | Op::BytesSet
                | Op::BytesDecode
                | Op::BytesZeros
                | Op::BytesFromInts
                | Op::BytesConcat
                | Op::Sh(_)
                | Op::Random(RandomFn::Int | RandomFn::Index)
                | Op::Json(_)
                | Op::Http(..)
                | Op::UhttpStream(_)
                | Op::Native(_)
                | Op::Fs(
                    FsFn::Read
                        | FsFn::ReadBytes
                        | FsFn::Write
                        | FsFn::Append
                        | FsFn::WriteBytes
                        | FsFn::AppendBytes
                        | FsFn::ReadStdinBytes
                        | FsFn::WriteStdoutBytes
                        | FsFn::Delete
                        | FsFn::Mkdir
                        | FsFn::Rmdir
                        | FsFn::ListDir
                        | FsFn::Size
                        | FsFn::Copy
                        | FsFn::Rename
                )
                | Op::Str(StrMethod::Repeat | StrMethod::PadStart | StrMethod::PadEnd)
        )
    }
}

impl Rvalue {
    /// Purpose: This function says whether computing an rvalue can raise, so
    /// the lowering checks for an error after it.
    ///
    /// Inputs: none.
    ///
    /// Outputs: true for calls and for operations that can fail.
    pub fn may_raise(&self) -> bool {
        match self {
            Rvalue::Call(..) | Rvalue::CallIndirect(..) => true,
            Rvalue::Op(op, _) => op.may_raise(),
            Rvalue::Bin(BinOp::Arith(_, NumTy::Int), ..) => true,
            Rvalue::Bin(BinOp::Arith(ArithOp::Div | ArithOp::Rem, NumTy::Float), ..) => true,
            Rvalue::Bin(BinOp::Bit(BitOp::Shl | BitOp::Shr), ..) => true,
            Rvalue::Un(UnOp::Neg(NumTy::Int), _) => true,
            _ => false,
        }
    }

    /// Purpose: This function says whether an rvalue's result is borrowed,
    /// so that storing it needs a retain.
    ///
    /// Inputs: none.
    ///
    /// Outputs: true for copies of a place, field reads, unwraps of a copy and
    /// borrowing operations.
    pub fn borrows_result(&self) -> bool {
        match self {
            Rvalue::Use(Operand::Copy(_)) | Rvalue::Use(Operand::Const(_)) => true,
            Rvalue::Field { .. } => true,
            Rvalue::Unwrap(Operand::Copy(_)) | Rvalue::Unwrap(Operand::Const(_)) => true,
            Rvalue::Wrap(_, Operand::Copy(_)) | Rvalue::Wrap(_, Operand::Const(_)) => true,
            Rvalue::Op(op, _) => op.borrows_result(),
            _ => false,
        }
    }
}

/// Purpose: This enum holds how a block ends.
#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Goto(BlockId),
    /// Branch on a bool.
    If(Operand, BlockId, BlockId),
    /// Branch on an int: the first case whose value matches, else `default`.
    Switch {
        value: Operand,
        cases: Vec<(i64, BlockId)>,
        default: BlockId,
    },
    /// Return a value. The backend retains it, then releases every local.
    Return(Operand),
    /// A block control never reaches, such as the one after a failure.
    Unreachable,
    /// Leave the function with an error raised: release every local and
    /// return, leaving the error for the caller to find.
    Unwind,
}

/// Purpose: This function says whether values of a type live on the heap and
/// carry a reference count.
///
/// Inputs:
/// - `t`: a resolved type.
///
/// Outputs: true for strings, arrays, dicts, structs, and optionals of them.
pub fn is_heap(t: &Ty) -> bool {
    match t {
        Ty::Str
        | Ty::Bytes
        | Ty::Array(_)
        | Ty::Map(_)
        | Ty::Struct(_)
        | Ty::Error
        | Ty::Future(_)
        | Ty::Json
        | Ty::Handle(_) => true,
        Ty::Opt(inner) => is_heap(inner),
        _ => false,
    }
}
