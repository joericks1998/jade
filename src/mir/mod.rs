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

#[cfg(test)]
mod tests;

pub use crate::compiler::infer::Ty;
pub use lower::lower_program;

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
    pub term: Term,
}

/// Purpose: This enum names somewhere a value is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// `() -> nil`: stop with the message as a runtime error.
    Fail(String),
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

impl Rvalue {
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
        Ty::Str | Ty::Array(_) | Ty::Map(_) | Ty::Struct(_) => true,
        Ty::Opt(inner) => is_heap(inner),
        _ => false,
    }
}
