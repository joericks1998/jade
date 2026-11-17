//! MIR: the typed middle layer between the checked program and LLVM.
//!
//! The checker in `typechecker::infer` gives every value one type and every call
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
pub mod imports;
pub mod lower;
pub mod taskcheck;

#[cfg(test)]
mod tests;

pub use crate::typechecker::infer::{SizedNum, Ty};
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
    /// The staticlib holding the program's Rust packages and the system
    /// libraries it needs, when the program calls one; the backend links it in.
    pub rust: Option<crate::rustpkg::Link>,
    /// The model a program that dereferences a prompt runs, read from its plan
    /// when the program is built into a binary. `None` until then, and for a
    /// program that runs no model.
    pub infer: Option<crate::infer::plan::InferPlan>,
}

impl Program {
    /// Purpose: This function says whether the program runs a model.
    ///
    /// Inputs: none.
    ///
    /// Outputs: whether any function asks `__infer` for anything.
    pub fn runs_a_model(&self) -> bool {
        self.funcs.iter().any(|f| {
            f.blocks.iter().any(|b| {
                b.stmts.iter().any(|s| {
                    let op = match s {
                        Stmt::Assign(_, Rvalue::Op(Op::Infer(m), _))
                        | Stmt::Eval(Rvalue::Op(Op::Infer(m), _)) => Some(*m),
                        _ => None,
                    };
                    // Setting the reply's length runs nothing.
                    op.is_some_and(|m| !matches!(m, InferFn::MaxTokens | InferFn::SetMaxTokens))
                })
            })
        })
    }
}

/// Purpose: This struct describes one call into a Rust package: the wrapper to
/// call, and the static types its arguments are sent as and its result is read
/// back as.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeCall {
    /// The `extern "C"` wrapper `#[jade::export]` wrote. Its companion that
    /// frees the result is the same name with `__free` added.
    pub symbol: String,
    /// How the program writes the call, `geo.midpoint`, for messages.
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
    /// Whether the function is the std library's, written in Jade under
    /// `src/lib/`. A call to one does not count against the recursion limit,
    /// as a call to a built-in never did, so the optimizer can inline it to
    /// nothing beyond what it computes.
    pub library: bool,
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
    /// A sized integer, its bits held in an `i64`: a `ulong` past the int's
    /// range reads as negative here.
    Sized(SizedNum, i64),
    Float32(f32),
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
    /// A sized number type, such as `byte` or `float32`.
    Sized(SizedNum),
}

impl NumTy {
    /// Purpose: This function gives the number type a value's type computes in.
    ///
    /// Inputs:
    /// - `t`: the value's type.
    ///
    /// Outputs: `Float` for a float, the sized kind for a sized type, and `Int`
    /// for anything else.
    pub fn of(t: &Ty) -> NumTy {
        match t {
            Ty::Float => NumTy::Float,
            Ty::Sized(s) => NumTy::Sized(*s),
            _ => NumTy::Int,
        }
    }
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
    /// A bitwise operator on an integer type; a shift's amount is an int.
    Bit(BitOp, NumTy),
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
    BitNot(NumTy),
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
    /// `(v: T) -> int`, for `toInt(v)`.
    ToInt(Ty),
    /// `(v: T) -> float`, for `toFloat(v)`.
    ToFloat(Ty),
    /// `(v: T) -> bool`, for `toBool(v)`.
    ToBool(Ty),
    /// `(v: T) -> char`, for `toChar(v)`.
    ToChar(Ty),
    /// `(v: from) -> to`: a converter where either side is a sized number
    /// type, such as `toByte(v)` or `toInt(b)` with `b` a byte. `from` may also
    /// be a str, bool or char. Raises when the value does not fit `to`, or the
    /// text is not a number.
    Convert { to: Ty, from: Ty },
    /// `(text: str) -> {str: V}`, for `toDict(text)`: JSON text holding an
    /// object, read into the dict type given, whose value type the checker
    /// chose from the program's use. Raises on text that is not JSON, on a
    /// top-level value that is not an object, and on a value that does not
    /// fit `V`. Keys and values take the text's trust.
    ToDict(Ty),

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

    // ── the std library's hooks ──────────────────────────────────────────
    /// What a std package written in Jade asks of the C library or LLVM;
    /// see [`SysFn`].
    Sys(SysFn),

    // ── inference ────────────────────────────────────────────────────────
    /// What the inference prelude asks of the backend; see [`InferFn`].
    Infer(InferFn),
    /// `(b: bytes) -> [int]`: each octet, for a `for` loop over bytes.
    BytesInts,

    /// A call into a Rust package, with each argument passed in its C form;
    /// see [`NativeCall`].
    Native(Box<NativeCall>),
}

/// Purpose: This enum lists what the inference prelude, `src/infer/prelude.jde`,
/// asks of the backend: the tables in the model's plan, and the model itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferFn {
    /// `(k: int) -> str`: one of the tokenizer's tables, as text.
    Data,
    /// `(k: int) -> int`: a number from the plan, such as which pre-tokenizer.
    Config,
    /// `(c: char) -> int`: 1 for a letter, 2 a number, 3 white space, else 0.
    CharClass,
    /// `(s: str) -> str`: the text, marked as coming from outside the program.
    Taint,
    /// `(p: prompt) -> str`: a prompt's text.
    Text,
    /// `() -> bool`: whether the model is mapped and its kernels built.
    Ready,
    /// `() -> nil`: open every region of the model's memory by the name dovata
    /// gave at build time, and map it.
    Map,
    /// `() -> nil`: build the GPU buffers and kernels over the mapped regions.
    Setup,
    /// `(ids: [int], conversation: int, prompt_len: int) -> int`: the token
    /// the model picks after `ids`, greedily, or -1 at a stop token or when
    /// the context is full. `ids` is the prompt's tokens and then the reply's
    /// so far; only the ones the model has not seen yet go through it.
    Next,
    /// `() -> int`: a new conversation's id, which `Next` takes.
    Conversation,
    /// `() -> int`: the most tokens a reply runs to, `max_tokens()`.
    MaxTokens,
    /// `(n: int) -> int`: set it, at least 1, and return what it is now.
    SetMaxTokens,
}

/// Purpose: This enum lists the hooks: what the std library written in Jade,
/// `src/lib/`, asks of the C library and LLVM through the `__sys` package.
///
/// Each hook is one operation, typed exactly: a hook taking a float refuses
/// an int, so library code says `float(x)`. Only library modules can name
/// `__sys`. A hook that brings outside text in marks it tainted, a hook that
/// runs a command or reads a path refuses tainted text itself, and state the
/// whole program shares stays behind a hook, under the hook's own lock.
/// [`SysFn::params`] and [`SysFn::result`] are the one place a hook's type is
/// written; the checker and MIR both read them.
///
/// A hook whose C library call can fail returns an empty value when it does
/// and leaves the C library's error code in the thread's slot, which
/// [`SysFn::OsError`] reads; library code turns it into the error v1 raised.
/// Only hooks write the slot, so nothing between the hook and the read can
/// change it, as an allocation can change C's `errno`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysFn {
    // ── floats, each one LLVM intrinsic or C library function ────────────
    /// `(x: float) -> float`: `llvm.floor`.
    Floor,
    /// `llvm.ceil`.
    Ceil,
    /// `llvm.round`, halves away from zero.
    Round,
    /// `llvm.trunc`.
    Trunc,
    /// `llvm.sqrt`.
    Sqrt,
    /// `llvm.log`.
    Ln,
    /// `llvm.log2`.
    Log2,
    /// `llvm.log10`.
    Log10,
    /// `llvm.exp`.
    Exp,
    /// `llvm.sin`.
    Sin,
    /// `llvm.cos`.
    Cos,
    /// C's `tan`, which LLVM 18 has no intrinsic for.
    Tan,
    /// C's `asin`.
    Asin,
    /// C's `acos`.
    Acos,
    /// C's `atan`.
    Atan,
    /// `llvm.fabs`.
    Fabs,
    /// `(y: float, x: float) -> float`: C's `atan2`.
    Atan2,
    /// `(a: float, b: float) -> float`: C's `hypot`.
    Hypot,
    /// `(base: float, exp: float) -> float`: `llvm.pow`.
    Pow,
    /// `(a: float, b: float) -> float`: `llvm.minnum`, where a NaN gives
    /// way to the other operand.
    Minnum,
    /// `llvm.maxnum`.
    Maxnum,
    /// `(mag: float, sign: float) -> float`: `llvm.copysign`.
    Copysign,
    /// `(base: float, exp: int) -> float`: `llvm.powi`, with the exponent
    /// cut to 32 bits as v1 passed it.
    Powi,
    /// `(x: float) -> int`: `llvm.fptosi.sat`, saturating at the ends of the
    /// int range and giving 0 for NaN.
    ToInt,
    /// `(bits: int) -> float`: the float with these bits, for the infinity
    /// and NaN no literal can write.
    FloatBits,

    // ── ints ─────────────────────────────────────────────────────────────
    /// `(a: int, b: int) -> int`: `llvm.sadd.sat`, saturating.
    AddSat,
    /// `(a: int, b: int) -> int`: a multiply that saturates at the end of
    /// the range the true product lies past.
    MulSat,
    /// `(a: int, b: int) -> int`: LLVM's `add`, which wraps on overflow, for
    /// arithmetic that v1 let wrap, such as `time.stamp`'s calendar sums.
    WrapAdd,
    /// LLVM's `sub`, which wraps.
    WrapSub,
    /// LLVM's `mul`, which wraps.
    WrapMul,

    // ── clocks, sleeping and the time zone ───────────────────────────────
    /// `() -> int`: Unix seconds, from the wall clock.
    Now,
    /// `() -> int`: Unix milliseconds, from the wall clock.
    NowMs,
    /// `() -> float`: seconds on a clock that never moves backwards.
    Monotonic,
    /// `(secs: float) -> nil`: pause the running thread; a delay that is not
    /// positive returns at once.
    Sleep,
    /// `(secs: float) -> future of nil`: a task that only sleeps, from
    /// `codegen/tasks/`.
    After,
    /// `(tz: str?, ts: int) -> int`: seconds east of UTC at an instant, in a
    /// named zone or the system's. `TZ` is set for the moment it takes, under
    /// a lock, since the environment is the whole program's.
    LocalOffset,
    /// `(tz: str?, ts: int) -> str`: the zone's abbreviation at an instant,
    /// such as `PST`, tainted, under the same lock.
    LocalZone,

    // ── errors and text ──────────────────────────────────────────────────
    /// `() -> int`: the C library's error code the last fallible hook on this
    /// thread failed with, or 0 when it succeeded.
    OsError,
    /// `(code: int) -> str`: the C library's text for an error code.
    Strerror,
    /// `(b: bytes) -> str`: the octets as text, each invalid UTF-8 sequence
    /// replaced by U+FFFD, with Rust's rule for how much one covers, keeping
    /// their trust.
    Lossy,
    /// `(d: {str: str}) -> [str]`: a dict's keys in the order they were
    /// added, which a `for` loop, sorting them, does not give.
    KeysInOrder,

    // ── the environment ──────────────────────────────────────────────────
    /// `(name: str) -> str?`: a variable's value, tainted, or nil when it is
    /// not set or its value is not UTF-8, as Rust's `env::var` gave.
    Getenv,
    /// `(name: str, value: str) -> nil`.
    Setenv,
    /// `() -> [str]`: the program's arguments, its own path first, trusted.
    Args,
    /// `() -> str`: the working directory, or "" when it cannot be read,
    /// trusted.
    Cwd,

    // ── the file system ──────────────────────────────────────────────────
    /// `(path: str, trust: bool, sink: str) -> str?`: a file's text, tainted
    /// unless `trust`, or nil when it is not UTF-8. A tainted path is refused
    /// unless `trust`, naming the library function `sink`, such as `fs.read`,
    /// as `fs.read(path)`. Fallible.
    ReadText,
    /// `(path: str, trust: bool, sink: str) -> bytes`: a file's octets, the
    /// same way. Fallible.
    ReadFile,
    /// `(path: str, data: bytes, append: bool) -> nil`: write a file, or add
    /// to its end, making it when it is not there. Fallible.
    WriteFile,
    /// `(path: str) -> [int]`: `stat`'s mode and size. Fallible.
    Stat,
    /// `(path: str) -> nil`: `unlink`. Fallible.
    Unlink,
    /// `(path: str) -> nil`: `rmdir`, an empty directory only. Fallible.
    Rmdir,
    /// `(path: str) -> nil`: `mkdir`, one level, mode 0777. Fallible.
    Mkdir,
    /// `(src: str, dst: str) -> nil`: `rename`. Fallible.
    Rename,
    /// `(src: str, dst: str) -> nil`: copy a file's octets in 64 KiB pieces,
    /// with v1's errors. Fallible.
    CopyFile,
    /// `(path: str) -> [str]`: every entry's name, `.` and `..` included, as
    /// the file system orders them, trusted. Fallible.
    ReadDir,

    // ── processes and descriptors ────────────────────────────────────────
    /// `(cmd: str, sink: str, capture: bool) -> [int]`: start `/bin/sh -c
    /// cmd`, refusing a tainted command and naming the library function
    /// `sink`, such as `sh.exec`, as `sh.exec(cmd)`.
    /// Gives the pid and, when `capture`, the read ends of its standard output
    /// and standard error, else -1 for each. Fallible.
    Shell,
    /// `(argv: [str], input: bool) -> [int]`: start `argv[0]`, found on
    /// `PATH`, with its standard output a pipe, its standard error going
    /// nowhere, and with `input` its standard input a pipe. A tainted program
    /// name is refused. Gives the pid, the write end of its input or -1, and
    /// the read end of its output. Fallible.
    Spawn,
    /// `(fd: int) -> bytes`: read to the end, tainted, keeping what came
    /// before a failure. Fallible.
    ReadAll,
    /// `(fd: int) -> bytes`: one read of up to 64 KiB, tainted; empty at the
    /// end or on a failure.
    ReadChunk,
    /// `(out: int, err: int) -> [bytes]`: read two pipes to their ends
    /// together, so a writer that fills one cannot stall, both tainted.
    Drain,
    /// `(fd: int, data: bytes) -> nil`: one `write`. Fallible.
    WriteFd,
    /// `(fd: int, data: bytes) -> nil`: one `write` to a child's input, with
    /// `SIGPIPE` ignored, so a child that exits early does not end the
    /// program. A failure is not reported.
    WriteChild,
    /// `(fd: int) -> nil`: `close`.
    Close,
    /// `(pid: int) -> int`: wait for a child, giving its exit code, or -1
    /// when a signal ended it.
    Wait,
    /// `(pid: int) -> nil`: send a child `SIGTERM`.
    Kill,

    // ── the random generator ─────────────────────────────────────────────
    /// `(n: int) -> int`: a draw from v1's xoshiro256** generator, which the
    /// whole program shares under a lock: a value from 0 up to `n`, read as
    /// an unsigned number, without modulo bias, or all 64 bits when `n` is 0.
    /// The generator seeds itself from the clock and the process id the
    /// first time, unless it was seeded.
    RandBelow,
    /// `(n: int) -> nil`: seed the generator through splitmix64.
    RandSeed,
}

impl SysFn {
    /// Every hook, for listing them.
    pub const ALL: &'static [SysFn] = &[
        SysFn::Floor,
        SysFn::Ceil,
        SysFn::Round,
        SysFn::Trunc,
        SysFn::Sqrt,
        SysFn::Ln,
        SysFn::Log2,
        SysFn::Log10,
        SysFn::Exp,
        SysFn::Sin,
        SysFn::Cos,
        SysFn::Tan,
        SysFn::Asin,
        SysFn::Acos,
        SysFn::Atan,
        SysFn::Fabs,
        SysFn::Atan2,
        SysFn::Hypot,
        SysFn::Pow,
        SysFn::Minnum,
        SysFn::Maxnum,
        SysFn::Copysign,
        SysFn::Powi,
        SysFn::ToInt,
        SysFn::FloatBits,
        SysFn::AddSat,
        SysFn::MulSat,
        SysFn::WrapAdd,
        SysFn::WrapSub,
        SysFn::WrapMul,
        SysFn::Now,
        SysFn::NowMs,
        SysFn::Monotonic,
        SysFn::Sleep,
        SysFn::After,
        SysFn::LocalOffset,
        SysFn::LocalZone,
        SysFn::OsError,
        SysFn::Strerror,
        SysFn::Lossy,
        SysFn::KeysInOrder,
        SysFn::Getenv,
        SysFn::Setenv,
        SysFn::Args,
        SysFn::Cwd,
        SysFn::ReadText,
        SysFn::ReadFile,
        SysFn::WriteFile,
        SysFn::Stat,
        SysFn::Unlink,
        SysFn::Rmdir,
        SysFn::Mkdir,
        SysFn::Rename,
        SysFn::CopyFile,
        SysFn::ReadDir,
        SysFn::Shell,
        SysFn::Spawn,
        SysFn::ReadAll,
        SysFn::ReadChunk,
        SysFn::Drain,
        SysFn::WriteFd,
        SysFn::WriteChild,
        SysFn::Close,
        SysFn::Wait,
        SysFn::Kill,
        SysFn::RandBelow,
        SysFn::RandSeed,
    ];

    /// Purpose: This function returns the name library code calls a hook by,
    /// as in `__sys.to_int(x)`.
    pub fn name(self) -> &'static str {
        match self {
            SysFn::Floor => "floor",
            SysFn::Ceil => "ceil",
            SysFn::Round => "round",
            SysFn::Trunc => "trunc",
            SysFn::Sqrt => "sqrt",
            SysFn::Ln => "ln",
            SysFn::Log2 => "log2",
            SysFn::Log10 => "log10",
            SysFn::Exp => "exp",
            SysFn::Sin => "sin",
            SysFn::Cos => "cos",
            SysFn::Tan => "tan",
            SysFn::Asin => "asin",
            SysFn::Acos => "acos",
            SysFn::Atan => "atan",
            SysFn::Fabs => "fabs",
            SysFn::Atan2 => "atan2",
            SysFn::Hypot => "hypot",
            SysFn::Pow => "pow",
            SysFn::Minnum => "minnum",
            SysFn::Maxnum => "maxnum",
            SysFn::Copysign => "copysign",
            SysFn::Powi => "powi",
            SysFn::ToInt => "to_int",
            SysFn::FloatBits => "float_bits",
            SysFn::AddSat => "add_sat",
            SysFn::MulSat => "mul_sat",
            SysFn::WrapAdd => "wrap_add",
            SysFn::WrapSub => "wrap_sub",
            SysFn::WrapMul => "wrap_mul",
            SysFn::Now => "now",
            SysFn::NowMs => "now_ms",
            SysFn::Monotonic => "monotonic",
            SysFn::Sleep => "sleep",
            SysFn::After => "after",
            SysFn::LocalOffset => "local_offset",
            SysFn::LocalZone => "local_zone",
            SysFn::OsError => "os_error",
            SysFn::Strerror => "strerror",
            SysFn::Lossy => "lossy",
            SysFn::KeysInOrder => "keys_in_order",
            SysFn::Getenv => "getenv",
            SysFn::Setenv => "setenv",
            SysFn::Args => "args",
            SysFn::Cwd => "cwd",
            SysFn::ReadText => "read_text",
            SysFn::ReadFile => "read_file",
            SysFn::WriteFile => "write_file",
            SysFn::Stat => "stat",
            SysFn::Unlink => "unlink",
            SysFn::Rmdir => "rmdir",
            SysFn::Mkdir => "mkdir",
            SysFn::Rename => "rename",
            SysFn::CopyFile => "copy_file",
            SysFn::ReadDir => "read_dir",
            SysFn::Shell => "shell",
            SysFn::Spawn => "spawn",
            SysFn::ReadAll => "read_all",
            SysFn::ReadChunk => "read_chunk",
            SysFn::Drain => "drain",
            SysFn::WriteFd => "write_fd",
            SysFn::WriteChild => "write_child",
            SysFn::Close => "close",
            SysFn::Wait => "wait",
            SysFn::Kill => "kill",
            SysFn::RandBelow => "rand_below",
            SysFn::RandSeed => "rand_seed",
        }
    }

    /// Purpose: This function finds a hook by the name library code calls it by.
    pub fn from_name(name: &str) -> Option<SysFn> {
        SysFn::ALL.iter().copied().find(|h| h.name() == name)
    }

    /// Purpose: This function returns the type of each argument a hook takes.
    pub fn params(self) -> Vec<Ty> {
        use SysFn::*;
        let (int, str, bool, bytes) = (Ty::Int, Ty::Str, Ty::Bool, Ty::Bytes);
        match self {
            Floor | Ceil | Round | Trunc | Sqrt | Ln | Log2 | Log10 | Exp | Sin | Cos | Tan
            | Asin | Acos | Atan | Fabs | ToInt | Sleep | After => vec![Ty::Float],
            Atan2 | Hypot | Pow | Minnum | Maxnum | Copysign => vec![Ty::Float, Ty::Float],
            Powi => vec![Ty::Float, int],
            FloatBits | Strerror | ReadAll | ReadChunk | Close | Wait | Kill | RandBelow
            | RandSeed => vec![int],
            AddSat | MulSat | WrapAdd | WrapSub | WrapMul | Drain => vec![int.clone(), int],
            Now | NowMs | Monotonic | OsError | Args | Cwd => Vec::new(),
            LocalOffset | LocalZone => vec![Ty::opt(str), int],
            Lossy => vec![bytes],
            KeysInOrder => vec![Ty::Map(Box::new(str))],
            Getenv | Stat | Unlink | Rmdir | Mkdir | ReadDir => vec![str],
            Setenv | Rename | CopyFile => vec![str.clone(), str],
            ReadText | ReadFile => vec![str.clone(), bool, str],
            WriteFile => vec![str, bytes, bool],
            Shell => vec![str.clone(), str, bool],
            Spawn => vec![Ty::Array(Box::new(str)), bool],
            WriteFd | WriteChild => vec![int, bytes],
        }
    }

    /// Purpose: This function returns the type a hook returns.
    pub fn result(self) -> Ty {
        use SysFn::*;
        match self {
            ToInt | AddSat | MulSat | WrapAdd | WrapSub | WrapMul | Now | NowMs | LocalOffset
            | OsError | Wait | RandBelow => Ty::Int,
            Sleep | Setenv | WriteFile | Unlink | Rmdir | Mkdir | Rename | CopyFile | WriteFd
            | WriteChild | Close | Kill | RandSeed => Ty::Nil,
            After => Ty::Future(Box::new(Ty::Nil)),
            LocalZone | Cwd | Strerror | Lossy => Ty::Str,
            Getenv | ReadText => Ty::opt(Ty::Str),
            ReadFile | ReadAll | ReadChunk => Ty::Bytes,
            KeysInOrder | Args | ReadDir => Ty::Array(Box::new(Ty::Str)),
            Stat | Shell | Spawn => Ty::Array(Box::new(Ty::Int)),
            Drain => Ty::Array(Box::new(Ty::Bytes)),
            Floor | Ceil | Round | Trunc | Sqrt | Ln | Log2 | Log10 | Exp | Sin | Cos | Tan
            | Asin | Acos | Atan | Fabs | Atan2 | Hypot | Pow | Minnum | Maxnum | Copysign
            | Powi | FloatBits | Monotonic => Ty::Float,
        }
    }

    /// Purpose: This function says whether a hook can raise. Only a hook that
    /// refuses tainted text does; every other failure is left in the error
    /// slot for library code to raise.
    pub fn may_raise(self) -> bool {
        matches!(self, SysFn::ReadText | SysFn::ReadFile | SysFn::Shell | SysFn::Spawn)
    }
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
        if let Op::Sys(h) = self {
            return h.may_raise();
        }
        matches!(
            self,
            Op::StrIndex
                | Op::Convert { .. }
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
                | Op::BytesGet
                | Op::BytesSet
                | Op::BytesDecode
                | Op::BytesZeros
                | Op::BytesFromInts
                | Op::BytesConcat
                | Op::ToDict(_)
                | Op::Infer(InferFn::Map | InferFn::Setup | InferFn::Next)
                | Op::Native(_)
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
            // Dividing by a constant other than 0 and -1 can neither divide by
            // zero nor overflow.
            Rvalue::Bin(
                BinOp::Arith(ArithOp::Div | ArithOp::Rem, NumTy::Int),
                _,
                Operand::Const(Const::Int(d)),
            ) => *d == 0 || *d == -1,
            Rvalue::Bin(BinOp::Arith(_, NumTy::Int), ..) => true,
            Rvalue::Bin(BinOp::Arith(ArithOp::Div | ArithOp::Rem, NumTy::Float), ..) => true,
            Rvalue::Bin(BinOp::Arith(op, NumTy::Sized(s)), ..) => {
                !s.is_float() || matches!(op, ArithOp::Div | ArithOp::Rem)
            }
            Rvalue::Bin(BinOp::Bit(BitOp::Shl | BitOp::Shr, _), ..) => true,
            Rvalue::Un(UnOp::Neg(NumTy::Int), _) => true,
            Rvalue::Un(UnOp::Neg(NumTy::Sized(s)), _) => !s.is_float(),
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
        | Ty::Future(_) => true,
        Ty::Opt(inner) => is_heap(inner),
        _ => false,
    }
}
