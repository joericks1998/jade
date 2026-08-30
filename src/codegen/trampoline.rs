//! The uniform entry every callable value is reached through.
//!
//! See this directory's README.
//!
//! A call site that jumps at a *value* does not know which function it holds,
//! so it cannot know how many parameters that function has or what the ones it
//! left off default to. It used to guess, building a fixed-arity call out of
//! the arguments it happened to have and jumping straight at the body. That
//! made three ordinary programs wrong:
//!
//! ```text
//!   fn one(a)          { … }   f(1, 2)   // extra argument silently dropped
//!   fn one(a)          { … }   f()       // `a` read from an uninitialised slot
//!   fn g(a, b = 2)     { … }   f(1)      // `b` read from one too
//! ```
//!
//! So every function that can become a value gets a second entry point,
//! `jf_ind_<uid>(argc, argv)`, and the value points at *that* rather than at
//! the body. The entry knows the parameter list, so it is the natural place to
//! check the count and fill the defaults, and it is the only place that can.
//! `jrt_call_value` in the C runtime is the matching half: one function that
//! knows how to enter a plain function, a bound method and a native binding.
//!
//! A direct call — the common case, where the callee is statically known — does
//! not come through here. It still calls `jf_<uid>` and fills its own defaults.

use super::*;

/// Emit `jf_ind_<uid>(i64 argc, ptr argv) -> i64` for every compiled function.
///
/// Emitted for all of them rather than on demand, the same way `jf_task_<uid>`
/// is: an entry nothing references is dropped by the optimizer, and emitting
/// eagerly keeps this out of the middle of body lowering.
pub(super) fn emit_indirect_entries<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    defs: &[Arc<CompiledFn>],
    funcs: &[FunctionValue<'ctx>],
) -> Result<(), String> {
    let i64_ty = context.i64_type();
    let ptr_ty = context.ptr_type(AddressSpace::default());
    let b = context.create_builder();

    for (uid, cf) in defs.iter().enumerate() {
        let f = module.add_function(
            &format!("jf_ind_{uid}"),
            i64_ty.fn_type(&[i64_ty.into(), ptr_ty.into()], false),
            Some(inkwell::module::Linkage::Internal),
        );
        let nparams = cf.params.len();
        // Defaults are trailing — the frontend rejects a required parameter
        // after an optional one — so the first parameter carrying a default is
        // also the count of required ones.
        let nreq = cf.defaults.iter().position(|d| d.is_some()).unwrap_or(nparams);

        let entry = context.append_basic_block(f, "entry");
        let bad = context.append_basic_block(f, "arity_bad");
        let ok = context.append_basic_block(f, "arity_ok");

        b.position_at_end(entry);
        let argc = f.get_nth_param(0).unwrap().into_int_value();
        let argv = f.get_nth_param(1).unwrap().into_pointer_value();
        let too_few = b
            .build_int_compare(
                IntPredicate::SLT,
                argc,
                i64_ty.const_int(nreq as u64, false),
                "too_few",
            )
            .map_err(|e| e.to_string())?;
        let too_many = b
            .build_int_compare(
                IntPredicate::SGT,
                argc,
                i64_ty.const_int(nparams as u64, false),
                "too_many",
            )
            .map_err(|e| e.to_string())?;
        let wrong = b.build_or(too_few, too_many, "wrong_arity").map_err(|e| e.to_string())?;
        b.build_conditional_branch(wrong, bad, ok).map_err(|e| e.to_string())?;

        // The interpreter names the *total* parameter count, whether or not the
        // tail of it has defaults, and counts `self` for a method. Both engines
        // have to say the same thing.
        b.position_at_end(bad);
        let throw_f = module.get_function("jrt_throw_arity").unwrap_or_else(|| {
            module.add_function(
                "jrt_throw_arity",
                context.void_type().fn_type(&[i64_ty.into(), i64_ty.into()], false),
                None,
            )
        });
        b.build_call(throw_f, &[i64_ty.const_int(nparams as u64, false).into(), argc.into()], "")
            .map_err(|e| e.to_string())?;
        b.build_unreachable().map_err(|e| e.to_string())?;

        b.position_at_end(ok);
        let mut call_args: Vec<BasicMetadataValueEnum> = Vec::with_capacity(nparams);
        // Parameters this entry had to *build*, and so owns: a string or float
        // default is a fresh allocation. Released after the call below.
        let mut built: Vec<(usize, IntValue<'ctx>)> = Vec::new();
        for i in 0..nparams {
            let load_ith = |b: &inkwell::builder::Builder<'ctx>| -> Result<IntValue<'ctx>, String> {
                let slot = unsafe {
                    b.build_in_bounds_gep(
                        i64_ty,
                        argv,
                        &[i64_ty.const_int(i as u64, false)],
                        "argslot",
                    )
                    .map_err(|e| e.to_string())?
                };
                b.build_load(i64_ty, slot, "arg")
                    .map_err(|e| e.to_string())
                    .map(|v| v.into_int_value())
            };

            if i < nreq {
                // `argc >= nreq` held above, so this one is always present.
                call_args.push(load_ith(&b)?.into());
                continue;
            }

            // Optional: present when `i < argc`, otherwise its declared default.
            // The two are separate blocks rather than a `select` because
            // building a float, string or collection default is a *call*, and
            // running it for an argument that was supplied would allocate
            // something nothing ever frees.
            let have_bb = context.append_basic_block(f, "arg_given");
            let dflt_bb = context.append_basic_block(f, "arg_default");
            let join_bb = context.append_basic_block(f, "arg_join");
            let present = b
                .build_int_compare(
                    IntPredicate::SLT,
                    i64_ty.const_int(i as u64, false),
                    argc,
                    "present",
                )
                .map_err(|e| e.to_string())?;
            b.build_conditional_branch(present, have_bb, dflt_bb).map_err(|e| e.to_string())?;

            b.position_at_end(have_bb);
            let given = load_ith(&b)?;
            b.build_unconditional_branch(join_bb).map_err(|e| e.to_string())?;
            let have_end = b.get_insert_block().unwrap();

            b.position_at_end(dflt_bb);
            let dflt = match &cf.defaults[i] {
                Some(v) => default_word_const(context, module, &b, v)?,
                // Unreachable for well-formed input (a parameter past `nreq`
                // has a default by construction), but a missing one must not
                // become an uninitialised read — that is the bug this file
                // exists to remove.
                None => i64_ty.const_int(NIL, false),
            };
            b.build_unconditional_branch(join_bb).map_err(|e| e.to_string())?;
            let dflt_end = b.get_insert_block().unwrap();

            b.position_at_end(join_bb);
            let phi = b.build_phi(i64_ty, "argv").map_err(|e| e.to_string())?;
            phi.add_incoming(&[(&given, have_end), (&dflt, dflt_end)]);
            let w = phi.as_basic_value().into_int_value();
            built.push((i, w));
            call_args.push(w.into());
        }

        let r = b
            .build_call(funcs[uid], &call_args, "indret")
            .map_err(|e| e.to_string())?
            .as_any_value_enum()
            .into_int_value();

        // Release the defaults this entry built. The callee retained every
        // parameter it was handed and releases them at its own scope exit, so
        // without this the entry's reference is simply lost — a `fn g(n, tag =
        // "…")` called two million times through a value grew the process by
        // 64 MB. Only the arguments that were *omitted* were built here, and
        // `i >= argc` is exactly that test, so no extra bookkeeping is needed.
        //
        // A body that raises longjmps past this and leaks one, in common with
        // every other frame the unwind skips.
        for (i, w) in built {
            let omitted = b
                .build_int_compare(
                    IntPredicate::SGE,
                    i64_ty.const_int(i as u64, false),
                    argc,
                    "omitted",
                )
                .map_err(|e| e.to_string())?;
            let rel_bb = context.append_basic_block(f, "rel_default");
            let next_bb = context.append_basic_block(f, "rel_next");
            b.build_conditional_branch(omitted, rel_bb, next_bb).map_err(|e| e.to_string())?;
            b.position_at_end(rel_bb);
            let dec = module.get_function("jrt_decref").unwrap_or_else(|| {
                module.add_function(
                    "jrt_decref",
                    context.void_type().fn_type(&[i64_ty.into()], false),
                    None,
                )
            });
            b.build_call(dec, &[w.into()], "").map_err(|e| e.to_string())?;
            b.build_unconditional_branch(next_bb).map_err(|e| e.to_string())?;
            b.position_at_end(next_bb);
        }
        b.build_return(Some(&r)).map_err(|e| e.to_string())?;
    }
    Ok(())
}
