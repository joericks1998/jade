//! How each type is laid out in registers and in memory.
//!
//! A scalar is the LLVM type that holds it. Strings, arrays, dicts and structs
//! are pointers to reference-counted objects (see the layouts in `mod.rs`). An
//! optional of a pointer type is the same pointer, null for nil; an optional of
//! a scalar is `{ i1 present, T }`.

use inkwell::types::{BasicTypeEnum, StructType};
use inkwell::values::{BasicValue, BasicValueEnum, IntValue};

use super::{Cx, F};
use crate::mir::Ty;

/// Byte offset of an object's length (strings, arrays, dicts).
pub const LEN: i64 = 8;
/// Byte offset of a string's first byte.
pub const STR_DATA: i64 = 16;
/// Byte offset of an array's or dict's capacity.
pub const CAP: i64 = 16;
/// Byte offset of an array's element buffer.
pub const ARR_DATA: i64 = 24;
/// Size of an array object.
pub const ARR_SIZE: i64 = 32;
/// Byte offsets of a dict's key buffer, value buffer, hash index and index size.
pub const DICT_KEYS: i64 = 24;
pub const DICT_VALS: i64 = 32;
pub const DICT_INDEX: i64 = 40;
pub const DICT_ICAP: i64 = 48;
/// Size of a dict object.
pub const DICT_SIZE: i64 = 56;
/// Byte offset of a struct's type id.
pub const TYPE_ID: i64 = 8;
/// A bit of the type id word set once an object's destructors have run, so a
/// value a destructor revived never runs them again. Type id reads mask it off.
pub const DROPPED: i64 = 1 << 62;

/// Purpose: This function says whether a type is held as a pointer.
///
/// Inputs:
/// - `t`: the type.
///
/// Outputs: true for strings, arrays, dicts and structs.
pub fn is_ptr(t: &Ty) -> bool {
    matches!(t, Ty::Str | Ty::Array(_) | Ty::Map(_) | Ty::Struct(_))
}

/// Purpose: This function returns the zero value of an LLVM type.
///
/// Inputs:
/// - `t`: the type.
///
/// Outputs: zero, null, or a zeroed aggregate.
pub fn zero(t: BasicTypeEnum<'_>) -> BasicValueEnum<'_> {
    match t {
        BasicTypeEnum::IntType(i) => i.const_zero().into(),
        BasicTypeEnum::FloatType(f) => f.const_zero().into(),
        BasicTypeEnum::PointerType(p) => p.const_null().into(),
        BasicTypeEnum::StructType(s) => s.const_zero().into(),
        BasicTypeEnum::ArrayType(a) => a.const_zero().into(),
        BasicTypeEnum::VectorType(v) => v.const_zero().into(),
        BasicTypeEnum::ScalableVectorType(v) => v.const_zero().into(),
    }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the LLVM type a value of a type is held in.
    ///
    /// Inputs:
    /// - `t`: the type.
    ///
    /// Outputs: the LLVM type. `nil` is an `i8` that is always zero.
    pub fn llty(&self, t: &Ty) -> BasicTypeEnum<'ctx> {
        match t {
            Ty::Int => self.i64t().into(),
            Ty::Float => self.f64t().into(),
            Ty::Bool => self.i1t().into(),
            Ty::Char => self.i32t().into(),
            Ty::Nil => self.i8t().into(),
            // A named function or builtin as a value is known when compiling;
            // only a joined function type is held as a pointer.
            Ty::Func(_) => self.i8t().into(),
            Ty::Opt(inner) if is_ptr(inner) => self.ptrt().into(),
            Ty::Opt(inner) => self.opt_ty(inner).into(),
            _ => self.ptrt().into(),
        }
    }

    /// Purpose: This function returns the `{ i1, T }` type of a scalar optional.
    pub fn opt_ty(&self, inner: &Ty) -> StructType<'ctx> {
        self.ctx.struct_type(&[self.i1t().into(), self.llty(inner)], false)
    }

    /// Purpose: This function returns a struct's object type: its reference
    /// count, its type id, then its fields.
    ///
    /// Inputs:
    /// - `name`: the struct.
    ///
    /// Outputs: the named LLVM struct type, made the first time.
    pub fn struct_ty(&self, name: &str) -> StructType<'ctx> {
        if let Some(t) = self.struct_tys.borrow().get(name) {
            return *t;
        }
        let t = self.ctx.opaque_struct_type(&format!("jade.struct.{name}"));
        self.struct_tys.borrow_mut().insert(name.to_string(), t);
        let mut body: Vec<BasicTypeEnum> = vec![self.i64t().into(), self.i64t().into()];
        if let Some(def) = self.structs.iter().find(|s| s.name == name) {
            for (_, ft) in &def.fields {
                body.push(self.llty(ft));
            }
        }
        t.set_body(&body, false);
        t
    }

    /// Purpose: This function returns a struct's fields with their types.
    pub fn struct_fields(&self, name: &str) -> Vec<Ty> {
        self.structs
            .iter()
            .find(|s| s.name == name)
            .map(|d| d.fields.iter().map(|(_, t)| t.clone()).collect())
            .unwrap_or_default()
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function says whether a value of an optional type, or of
    /// type `nil`, is absent.
    ///
    /// Inputs:
    /// - `t`: the value's type.
    /// - `v`: the value.
    ///
    /// Outputs: an `i1`, true when absent.
    pub fn is_nil(&self, t: &Ty, v: BasicValueEnum<'ctx>) -> IntValue<'ctx> {
        match t {
            Ty::Nil => self.bool(true),
            Ty::Opt(inner) if is_ptr(inner) => self.is_null(v.into_pointer_value()),
            Ty::Error => self.is_null(v.into_pointer_value()),
            Ty::Opt(_) => {
                let present = self
                    .b
                    .build_extract_value(v.into_struct_value(), 0, "")
                    .expect("positioned")
                    .into_int_value();
                self.not(present)
            }
            _ => self.bool(false),
        }
    }

    /// Purpose: This function reads the value out of an optional known present.
    pub fn unwrap(&self, t: &Ty, v: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match t {
            Ty::Opt(inner) if !is_ptr(inner) => {
                self.b.build_extract_value(v.into_struct_value(), 1, "").expect("positioned")
            }
            _ => v,
        }
    }

    /// Purpose: This function makes an optional holding a value.
    ///
    /// Inputs:
    /// - `t`: the optional type.
    /// - `v`: the value it holds.
    ///
    /// Outputs: the optional.
    pub fn wrap(&self, t: &Ty, v: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match t {
            Ty::Opt(inner) if !is_ptr(inner) => {
                let st = self.cx.opt_ty(inner);
                let s = self
                    .b
                    .build_insert_value(st.get_undef(), self.bool(true), 0, "")
                    .expect("positioned");
                let s = self.b.build_insert_value(s, v, 1, "").expect("positioned");
                s.as_basic_value_enum()
            }
            _ => v,
        }
    }

    /// Purpose: This function reads a struct object's type id, without the
    /// destructor mark.
    ///
    /// Inputs:
    /// - `p`: the object.
    ///
    /// Outputs: its type id.
    pub fn type_id(&self, p: inkwell::values::PointerValue<'ctx>) -> IntValue<'ctx> {
        let word = self.load_i64(self.at_c(p, TYPE_ID));
        self.and(word, self.i64(!DROPPED))
    }
}
