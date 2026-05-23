use inkwell::{
    context::Context,
    types::{BasicMetadataTypeEnum, BasicTypeEnum, FunctionType},
    AddressSpace,
};

use crate::compiler::tir::JadeType;

/// Map a `JadeType` to the LLVM basic type used for variable slots and values.
///
/// `Unknown` (unannotated function parameters) is treated as `i64`.  This is
/// correct for the common case of integer-heavy programs and is a documented
/// limitation for the initial LLVM backend.
pub fn jade_to_llvm<'ctx>(ty: &JadeType, ctx: &'ctx Context) -> BasicTypeEnum<'ctx> {
    match ty {
        JadeType::Int | JadeType::Unknown | JadeType::Nil => ctx.i64_type().into(),
        JadeType::Float => ctx.f64_type().into(),
        JadeType::Bool => ctx.bool_type().into(),
        JadeType::Str
        | JadeType::Array(_)
        | JadeType::Dict
        | JadeType::Struct(_)
        | JadeType::Fn { .. }
        | JadeType::AsyncFn { .. }
        | JadeType::Future(_)
        | JadeType::Prompt
        | JadeType::Grammar => ctx.ptr_type(AddressSpace::default()).into(),
    }
}

/// `BasicMetadataTypeEnum` wrapper used for function parameter type lists.
pub fn jade_to_meta<'ctx>(ty: &JadeType, ctx: &'ctx Context) -> BasicMetadataTypeEnum<'ctx> {
    jade_to_llvm(ty, ctx).into()
}

/// Build the LLVM `FunctionType` for a Jade function.
///
/// `JadeType::Nil` → LLVM `void`.  All other return types map through
/// `jade_to_llvm`, and `fn_type` is called on each concrete LLVM type variant
/// because `BasicTypeEnum` does not expose `fn_type` directly in inkwell 0.8.
pub fn jade_fn_type<'ctx>(
    ret_ty: &JadeType,
    param_meta: &[BasicMetadataTypeEnum<'ctx>],
    ctx: &'ctx Context,
) -> FunctionType<'ctx> {
    match ret_ty {
        JadeType::Nil => ctx.void_type().fn_type(param_meta, false),
        _ => match jade_to_llvm(ret_ty, ctx) {
            BasicTypeEnum::IntType(t)            => t.fn_type(param_meta, false),
            BasicTypeEnum::FloatType(t)          => t.fn_type(param_meta, false),
            BasicTypeEnum::PointerType(t)        => t.fn_type(param_meta, false),
            BasicTypeEnum::StructType(t)         => t.fn_type(param_meta, false),
            BasicTypeEnum::ArrayType(t)          => t.fn_type(param_meta, false),
            BasicTypeEnum::VectorType(t)         => t.fn_type(param_meta, false),
            BasicTypeEnum::ScalableVectorType(t) => t.fn_type(param_meta, false),
        },
    }
}
