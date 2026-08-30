//! Tests for the lowering.

use super::*;
use crate::bytecode::Instr::*;

/// Lower an isolated body and hand back its IR to assert against.
///
/// There used to be a second helper beside this one, because reference
/// counting was a per-program decision and `lower_chunk` built an `FnCtx` with
/// it switched off — so a retain was invisible in the IR this returned. The
/// decision is gone and the rc ops always emit, so one helper covers both.
fn ir_of(code: &[Instr], n_slots: u32) -> String {
    let context = Context::create();
    let module = context.create_module("t");
    lower_chunk(&context, &module, "f", code, n_slots).expect("lowering failed");
    // Verify catches malformed IR (unterminated blocks, type errors) — the
    // real correctness gate for a lowering before it is wired up.
    module.verify().expect("module failed LLVM verification");
    module.print_to_string().to_string()
}

#[test]
fn dict_get_retains_the_borrowed_value() {
    // r1 = TABLE.get(r0) — the value word comes back borrowed (the dict
    // still owns it), so the destination must take its own reference. It
    // did not, so a nested `TABLE.get(k)["f"]` read correct values twice,
    // then double-freed the inner dict.
    let ir = ir_of(
        &[
            MakeDict(0, vec![]),
            LoadStr(1, "k".to_string()),
            GetField(2, 0, "get".to_string()),
            Call(3, 2, vec![1]),
            Return(Some(3)),
        ],
        4,
    );
    assert!(ir.contains("jrt_coll_dict_get"), "get lowered to the runtime lookup:\n{ir}");
    assert!(ir.contains("jrt_incref"), "the borrowed value word is retained:\n{ir}");
}

#[test]
fn dict_method_guards_its_receiver() {
    // A method name does not prove the receiver's kind, so `keys` has to
    // check before untagging to a DictObj*. Without this, `"str".keys()`
    // dereferenced a char* as a dict and killed the process.
    let ir = ir_of(
        &[
            MakeDict(0, vec![]),
            GetField(1, 0, "keys".to_string()),
            Call(2, 1, vec![]),
            Return(Some(2)),
        ],
        3,
    );
    assert!(ir.contains("jrt_require_kind"), "the receiver is kind-checked:\n{ir}");
    assert!(ir.contains("jrt_coll_dict_keys"), "then the real call:\n{ir}");
}

#[test]
fn array_method_guards_its_receiver() {
    let ir = ir_of(
        &[
            MakeArray(0, vec![]),
            LoadInt(1, 1),
            GetField(2, 0, "push".to_string()),
            Call(3, 2, vec![1]),
            Return(Some(3)),
        ],
        4,
    );
    assert!(ir.contains("jrt_require_kind"), "the receiver is kind-checked:\n{ir}");
    assert!(ir.contains("jrt_karr_push"), "then the real call:\n{ir}");
}

#[test]
fn str_method_guards_receiver_and_arguments() {
    // Both the receiver and the argument are untagged to char*, so both
    // need the check — and a bad argument reports as an argument type
    // error, not as a missing field.
    let ir = ir_of(
        &[
            LoadStr(0, "abc".to_string()),
            LoadStr(1, "a".to_string()),
            GetField(2, 0, "starts_with".to_string()),
            Call(3, 2, vec![1]),
            Return(Some(3)),
        ],
        4,
    );
    assert!(ir.contains("jrt_require_kind"), "the receiver is kind-checked:\n{ir}");
    assert!(ir.contains("jrt_require_str_arg"), "the argument is checked too:\n{ir}");
    assert!(ir.contains("jrt_str_starts_with"), "then the real call:\n{ir}");
}

#[test]
fn runtime_dispatched_methods_are_not_guarded() {
    // `len` and `contains` hand the whole tagged word to jrt_len_chunk /
    // jrt_in_any, which dispatch on the tag themselves and are safe on a
    // scalar. Guarding them would raise on receivers the VM accepts.
    let ir = ir_of(
        &[
            LoadStr(0, "abc".to_string()),
            GetField(1, 0, "len".to_string()),
            Call(2, 1, vec![]),
            Return(Some(2)),
        ],
        3,
    );
    assert!(ir.contains("jrt_len_chunk"), "len goes to the tag dispatcher:\n{ir}");
    assert!(!ir.contains("jrt_require_kind"), "and is not kind-guarded:\n{ir}");
}

#[test]
fn arithmetic_lowers_to_add_and_ret() {
    // r2 = r0 + r1 ; return r2   with r0=2, r1=3
    let ir = ir_of(&[LoadInt(0, 2), LoadInt(1, 3), AddInt(2, 0, 1), Return(Some(2))], 3);
    assert!(ir.contains("alloca i64"), "slots allocated:\n{ir}");
    assert!(ir.contains(" add "), "native add emitted:\n{ir}");
    assert!(ir.contains("ret i64"), "returns a value word:\n{ir}");
}

#[test]
fn conditional_lowers_to_condbr() {
    // if r0 { return 1 } else { return 2 }
    let ir = ir_of(
        &[
            LoadBool(0, true),
            JumpIfFalse(0, 2), // → idx 4
            LoadInt(1, 1),
            Return(Some(1)),
            LoadInt(1, 2),
            Return(Some(1)),
        ],
        2,
    );
    assert!(ir.contains("br i1"), "conditional branch emitted:\n{ir}");
    // Two distinct return sites.
    assert_eq!(ir.matches("ret i64").count(), 2, "two returns:\n{ir}");
}

#[test]
fn every_block_is_terminated() {
    // Backward loop: must still produce valid (terminated) blocks.
    let ir = ir_of(&[LoadInt(0, 0), Jump(-1)], 1);
    // A well-formed module verifies; unterminated blocks would fail printing
    // to be verifiable, so a non-empty IR with a branch is our smoke signal.
    assert!(ir.contains("br label"));
}

#[test]
fn float_arithmetic_boxes_and_unboxes() {
    // r2 = r0 + r1 (floats) ; return r2
    let ir = ir_of(&[LoadFloat(0, 2.5), LoadFloat(1, 1.5), AddFloat(2, 0, 1), Return(Some(2))], 3);
    assert!(ir.contains("jrt_box_float"), "boxes floats:\n{ir}");
    assert!(ir.contains("jrt_unbox_float"), "unboxes operands:\n{ir}");
    assert!(ir.contains("fadd"), "native fadd emitted:\n{ir}");
    // The runtime symbols are declared exactly once each.
    assert_eq!(ir.matches("declare i64 @jrt_box_float").count(), 1, "one box decl:\n{ir}");
    assert_eq!(ir.matches("declare double @jrt_unbox_float").count(), 1, "one unbox decl:\n{ir}");
}

#[test]
fn int_to_float_widens_then_boxes() {
    // r1 = float(r0) ; return r1   with r0 = 3
    let ir = ir_of(&[LoadInt(0, 3), IntToFloat(1, 0), Return(Some(1))], 2);
    assert!(ir.contains("sitofp"), "signed int→float conversion:\n{ir}");
    assert!(ir.contains("jrt_box_float"), "result boxed:\n{ir}");
}

#[test]
fn string_literal_and_concat() {
    // r2 = "ab" + "cd" ; return r2
    let ir = ir_of(
        &[
            LoadStr(0, "ab".to_string()),
            LoadStr(1, "cd".to_string()),
            ConcatStr(2, 0, 1),
            Return(Some(2)),
        ],
        3,
    );
    // Two pre-tagged, 8-aligned internal literal globals.
    assert!(ir.matches("str_lit_t").count() >= 2, "two literal globals:\n{ir}");
    assert!(ir.contains("align 8"), "literal globals 8-aligned:\n{ir}");
    // The literal payload keeps a 7-byte pad + trust header before the bytes.
    assert!(ir.contains("jrt_str_concat"), "concat via runtime:\n{ir}");
    // Words carry the STRING tag: an `or …, 5` after ptrtoint.
    assert!(ir.contains("ptrtoint"), "pointer tagged into a word:\n{ir}");
}

#[test]
fn int_div_guards_zero_divisor_with_a_raise() {
    // r2 = r0 / r1 ; return r2
    let ir = ir_of(&[LoadInt(0, 6), LoadInt(1, 2), DivInt(2, 0, 1), Return(Some(2))], 3);
    assert!(ir.contains("sdiv"), "native signed div:\n{ir}");
    assert!(ir.contains("divzero_throw"), "a throw block guards the divisor:\n{ir}");
    // Via jrt_throw_runtime, not a bare throw: codegen's own failures are
    // runtime errors and must reach `catch` as the VM's `RuntimeError`
    // struct, so `e.message` and `catch RuntimeError e` work compiled.
    assert!(ir.contains("jrt_throw_runtime"), "raises on zero divisor:\n{ir}");
    assert!(ir.contains("unreachable"), "throw path is noreturn:\n{ir}");
}

#[test]
fn mod_uses_srem() {
    let ir = ir_of(&[LoadInt(0, 7), LoadInt(1, 3), ModInt(2, 0, 1), Return(Some(2))], 3);
    assert!(ir.contains("srem"), "native signed remainder:\n{ir}");
    assert!(ir.contains("divzero_throw"), "modulo also guards zero:\n{ir}");
}

#[test]
fn raise_throws_and_terminates() {
    // raise "boom"
    let ir = ir_of(&[LoadStr(0, "boom".to_string()), Raise(0)], 1);
    assert!(ir.contains("jade_exc_throw_typed"), "raises the value:\n{ir}");
    assert!(ir.contains("unreachable"), "raise terminates its block:\n{ir}");
}

#[test]
fn try_catch_lowers_to_setjmp_frame() {
    // 0: SetupHandler(caught=r1, →4)   1: LoadInt r0,1 (try body)
    // 2: PopHandler                    3: Jump →5 (skip handler)
    // 4: Move r0,r1 (handler body)     5: Halt
    let ir = ir_of(&[SetupHandler(1, 3), LoadInt(0, 1), PopHandler, Jump(1), Move(0, 1), Halt], 2);
    assert!(ir.contains("jade_exc_push_frame"), "frame registered:\n{ir}");
    assert!(ir.contains("call i32 @setjmp"), "setjmp split:\n{ir}");
    assert!(ir.contains("returns_twice"), "setjmp marked returns_twice:\n{ir}");
    assert!(ir.contains("jade_exc_pop"), "clean exit pops frame:\n{ir}");
    assert!(ir.contains("jade_exc_value"), "landing binds the caught value:\n{ir}");
    assert!(ir.contains("exc_landing"), "distinct landing block:\n{ir}");
}

#[test]
fn a_function_with_a_handler_scopes_it_to_the_frame() {
    // SetupHandler ; Return — the return leaves the try WITHOUT reaching
    // PopHandler, so the depth captured on entry has to be restored or the
    // frame outlives the stack that holds its jmp_buf.
    let ir = ir_of(&[SetupHandler(1, 2), LoadInt(0, 1), Return(Some(0)), Halt], 2);
    assert!(ir.contains("jade_exc_depth"), "entry snapshots the depth:\n{ir}");
    assert!(ir.contains("jade_exc_restore"), "return unwinds to it:\n{ir}");
}

#[test]
fn a_function_without_a_handler_pays_nothing() {
    // No try → the function cannot push a frame, so it needs neither the
    // prologue call nor the restore.
    let ir = ir_of(&[LoadInt(0, 1), Return(Some(0))], 1);
    assert!(!ir.contains("jade_exc_depth"), "no snapshot without a try:\n{ir}");
    assert!(!ir.contains("jade_exc_restore"), "no restore without a try:\n{ir}");
}

#[test]
fn globals_load_and_store_a_named_cell() {
    // x = 5 ; return x     (SetGlobal then GetGlobal)
    let ir = ir_of(
        &[
            LoadInt(0, 5),
            SetGlobal("x".to_string(), 0),
            GetGlobal(1, "x".to_string()),
            Return(Some(1)),
        ],
        2,
    );
    // One internal global cell named for the variable, nil-initialized.
    assert!(ir.contains("@jgl_x"), "named global cell emitted:\n{ir}");
    assert_eq!(ir.matches("@jgl_x = internal global").count(), 1, "one cell, reused:\n{ir}");
}

#[test]
fn locals_are_moves_within_the_slot_array() {
    // GetLocal/SetLocal shuffle slots; must verify and touch both slots.
    let ir = ir_of(&[LoadInt(0, 7), SetLocal(1, 0), GetLocal(2, 1), Return(Some(2))], 3);
    assert!(ir.contains("ret i64"), "returns a word:\n{ir}");
}

#[test]
fn unsupported_opcode_is_reported_not_panicked() {
    let context = Context::create();
    let module = context.create_module("t");
    // `ImportFile` is resolved away before lowering, so it never reaches the
    // backend in a real chunk — a clean "unsupported opcode" Err, not a panic.
    let err =
        lower_chunk(&context, &module, "f", &[ImportFile("a".into(), "b".into())], 1).unwrap_err();
    assert!(err.contains("unsupported opcode"), "got: {err}");
}

#[test]
fn typed_comparisons_lower_to_native_icmp_fcmp() {
    // r2 = (r0 < r1) int ; r5 = (r3 < r4) float ; return via bool words
    let ir = ir_of(
        &[
            LoadInt(0, 1),
            LoadInt(1, 2),
            CmpLtInt(2, 0, 1),
            LoadFloat(3, 1.0),
            LoadFloat(4, 2.0),
            CmpLtFloat(5, 3, 4),
            Return(Some(2)),
        ],
        6,
    );
    assert!(ir.contains("icmp slt"), "signed int compare:\n{ir}");
    assert!(ir.contains("fcmp olt"), "ordered float compare:\n{ir}");
    assert!(ir.contains("select i1"), "bool word materialized:\n{ir}");
}

#[test]
fn print_devirtualizes_to_jrt_print_any() {
    // GetGlobal print ; LoadInt r1,5 ; Call r2 = print(r1) ; Halt
    let ir =
        ir_of(&[GetGlobal(0, "print".to_string()), LoadInt(1, 5), Call(2, 0, vec![1]), Halt], 3);
    assert!(ir.contains("jrt_print_any"), "print devirtualized to runtime:\n{ir}");
}

// Build a two-fn program by hand: top defines `add(a, b)` and calls it.
fn add_program() -> Chunk {
    use std::sync::Arc;
    // fn add(a, b): return a + b   (slots 0=a, 1=b, 2=sum)
    let body = vec![AddInt(2, 0, 1), Return(Some(2))];
    let add_fn = Arc::new(CompiledFn {
        params: vec!["a".to_string(), "b".to_string()],
        defaults: vec![None, None],
        chunk: Chunk { name: "add".into(), code: body, spans: vec![], fn_defs: vec![] },
        n_slots: 3,
        source_file: String::new(),
        module_scope: None,
        is_generator: false,
    });
    // top:  LoadFn r0 add ; SetGlobal add r0 ;
    //       GetGlobal r1 add ; LoadInt r2 2 ; LoadInt r3 3 ;
    //       Call r4 = r1(r2, r3) ; Halt
    let mut top = Chunk::new("<top>");
    top.fn_defs.push(add_fn);
    top.code = vec![
        LoadFn(0, 0),
        SetGlobal("add".into(), 0),
        GetGlobal(1, "add".into()),
        LoadInt(2, 2),
        LoadInt(3, 3),
        Call(4, 1, vec![2, 3]),
        Halt,
    ];
    top
}

#[test]
fn user_function_lowers_to_a_direct_call() {
    let context = Context::create();
    let module = context.create_module("t");
    let top = add_program();
    lower_program(&context, &module, &top, 5, &HashMap::new(), &HashMap::new(), &HashMap::new())
        .expect("program lowering failed");
    module.verify().expect("module failed verification");
    let ir = module.print_to_string().to_string();
    // The function body is its own LLVM function taking two i64 params.
    assert!(ir.contains("define i64 @jf_0(i64"), "fn lowered with params:\n{ir}");
    // The top-level call is a *direct* call to it (devirtualized), not indirect.
    assert!(ir.contains("call i64 @jf_0("), "direct call emitted:\n{ir}");
    assert!(ir.contains("@jade_toplevel"), "top-level fn present:\n{ir}");
}

#[test]
fn call_with_omitted_default_is_filled_at_the_call_site() {
    use std::sync::Arc;
    // fn greet(n = 5): return n     ; call greet() with no args → fills 5
    let body = vec![GetLocal(1, 0), Return(Some(1))];
    let greet = Arc::new(CompiledFn {
        params: vec!["n".to_string()],
        defaults: vec![Some(VmValue::Int(5))],
        chunk: Chunk { name: "greet".into(), code: body, spans: vec![], fn_defs: vec![] },
        n_slots: 2,
        source_file: String::new(),
        module_scope: None,
        is_generator: false,
    });
    let mut top = Chunk::new("<top>");
    top.fn_defs.push(greet);
    top.code = vec![
        LoadFn(0, 0),
        SetGlobal("greet".into(), 0),
        GetGlobal(1, "greet".into()),
        Call(2, 1, vec![]), // no args → default 5
        Halt,
    ];
    let context = Context::create();
    let module = context.create_module("t");
    lower_program(&context, &module, &top, 3, &HashMap::new(), &HashMap::new(), &HashMap::new())
        .expect("lowering failed");
    module.verify().expect("verification failed");
    let ir = module.print_to_string().to_string();
    // Default 5 materialized as a tagged int (5<<1 = 10) passed to the call.
    assert!(ir.contains("call i64 @jf_0(i64 10)"), "default filled as 10:\n{ir}");
}

#[test]
fn function_value_is_first_class_and_returnable() {
    use std::sync::Arc;
    // A program that *returns* a function value now succeeds — the value is a
    // boxed function pointer (a global `@jf_box_0`), not a decline.
    let f = Arc::new(CompiledFn {
        params: vec![],
        defaults: vec![],
        chunk: Chunk { name: "f".into(), code: vec![Return(None)], spans: vec![], fn_defs: vec![] },
        n_slots: 0,
        source_file: String::new(),
        module_scope: None,
        is_generator: false,
    });
    let mut top = Chunk::new("<top>");
    top.fn_defs.push(f);
    top.code = vec![LoadFn(0, 0), Return(Some(0))];
    let context = Context::create();
    let module = context.create_module("t");
    lower_program(&context, &module, &top, 1, &HashMap::new(), &HashMap::new(), &HashMap::new())
        .expect("first-class fn value should lower");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("@jf_box_0"), "boxed function pointer global emitted:\n{ir}");
}

#[test]
fn keyword_call_reorders_args_to_parameter_order() {
    use std::sync::Arc;
    // fn f(a, b, c): return a   ; call f(r1, c=r3, b=r2) — named args reorder.
    let f = Arc::new(CompiledFn {
        params: vec!["a".into(), "b".into(), "c".into()],
        defaults: vec![None, None, None],
        chunk: Chunk {
            name: "f".into(),
            code: vec![GetLocal(3, 0), Return(Some(3))],
            spans: vec![],
            fn_defs: vec![],
        },
        n_slots: 4,
        source_file: String::new(),
        module_scope: None,
        is_generator: false,
    });
    let mut top = Chunk::new("<top>");
    top.fn_defs.push(f);
    top.code = vec![
        LoadFn(0, 0),
        SetGlobal("f".into(), 0),
        GetGlobal(1, "f".into()),
        LoadInt(2, 1),
        LoadInt(3, 3),
        LoadInt(4, 2),
        // f(a=r2, c=r3, b=r4)  → positional r2 for a, named c=r3, named b=r4
        CallNamed(5, 1, vec![(None, 2), (Some("c".into()), 3), (Some("b".into()), 4)]),
        Return(Some(5)),
    ];
    let context = Context::create();
    let module = context.create_module("t");
    lower_program(&context, &module, &top, 6, &HashMap::new(), &HashMap::new(), &HashMap::new())
        .expect("keyword call lowering");
    module.verify().expect("module failed verification");
    let ir = module.print_to_string().to_string();
    // A direct call to jf_0 with three i64 args (reordered to a, b, c).
    assert!(ir.contains("call i64 @jf_0(i64"), "direct call with reordered args:\n{ir}");
}

#[test]
fn higher_order_call_lowers_to_indirect_call() {
    use std::sync::Arc;
    // fn apply(f, x): return f(x)   — f is a param, so f(x) is an indirect call.
    //   slots: 0=f, 1=x, 2=result
    let apply_body = vec![GetLocal(3, 0), GetLocal(4, 1), Call(2, 3, vec![4]), Return(Some(2))];
    let apply = Arc::new(CompiledFn {
        params: vec!["f".into(), "x".into()],
        defaults: vec![None, None],
        chunk: Chunk { name: "apply".into(), code: apply_body, spans: vec![], fn_defs: vec![] },
        n_slots: 5,
        source_file: String::new(),
        module_scope: None,
        is_generator: false,
    });
    let mut top = Chunk::new("<top>");
    top.fn_defs.push(apply);
    top.code = vec![LoadFn(0, 0), SetGlobal("apply".into(), 0), Halt];
    let context = Context::create();
    let module = context.create_module("t");
    lower_program(&context, &module, &top, 1, &HashMap::new(), &HashMap::new(), &HashMap::new())
        .expect("higher-order lowering");
    module.verify().expect("module failed verification");
    let ir = module.print_to_string().to_string();
    // apply's body calls its parameter indirectly (a load then a call of a ptr).
    assert!(ir.contains("call i64 %fnld") || ir.contains("%icall"), "indirect call emitted:\n{ir}");
}

#[test]
fn fstring_folds_parts_with_concat() {
    // f"n={r0}"  →  concat("n=", str_of_any(r0))
    let ir = ir_of(
        &[
            LoadInt(0, 42),
            BuildFStr(1, vec![FStrPart::Literal("n=".to_string()), FStrPart::Reg(0)]),
            Return(Some(1)),
        ],
        2,
    );
    assert!(ir.contains("jrt_str_of_any"), "interpolated part rendered:\n{ir}");
    assert!(ir.contains("jrt_str_concat"), "parts folded via concat:\n{ir}");
}

#[test]
fn array_make_index_and_set_lower_to_kind_runtime() {
    // a = [10, 20]; a[0]; a[1] = 30
    let ir = ir_of(
        &[
            LoadInt(0, 10),
            LoadInt(1, 20),
            MakeArray(2, vec![0, 1]),
            LoadInt(3, 0),
            GetIndex(4, 2, 3),
            LoadInt(5, 1),
            LoadInt(6, 30),
            SetIndex(2, 5, 6),
            Return(Some(4)),
        ],
        7,
    );
    assert!(ir.contains("jrt_karr_new"), "array allocated:\n{ir}");
    assert!(ir.contains("jrt_karr_push"), "elements pushed:\n{ir}");
    assert!(ir.contains("jrt_val_index"), "GetIndex via runtime dispatch:\n{ir}");
    assert!(ir.contains("jrt_val_set_index"), "SetIndex via runtime dispatch:\n{ir}");
    // The array word carries the non-string heap tag (`or …, 1`).
    assert!(ir.contains("tagptr"), "array pointer tagged TAG_PTR:\n{ir}");
}

/// A native fn value's layout is `{ sentinel@0, ObjKind::Fn@8, env@16 }`.
///
/// The kind word at offset 8 is what makes the value safe to hand to
/// `jrt_decref`: without it, offset 8 held the `env` pointer, and a heap
/// address whose low byte happened to be 2/3/4 would have been read as
/// Array/Dict/Struct and reclaimed as one. That hazard is why native refs
/// used to veto refcounting for the entire program.
///
/// Nothing covered this path before, so the slot indices could be changed
/// silently — a wrong index compiles cleanly and corrupts at runtime.
///
/// The box is a link-time constant rather than a `malloc` now (it used to leak
/// one per FFI call), so the layout is pinned by reading its initializer rather
/// than the stores that used to fill it. That pins the slot *order* too, which
/// the old form could not.
#[test]
fn native_fn_value_carries_an_objkind_at_offset_8() {
    // `let f = <native ref>` — loading the ref as a value (not calling it)
    // is what materializes the box.
    let ir = ir_of(&[GetGlobal(0, "__native$0$somefn".into()), Return(Some(0))], 2);
    let line = ir
        .lines()
        .find(|l| l.contains(r#"@"native_fnval$0$somefn" = "#))
        .unwrap_or_else(|| panic!("native fn value emitted as a global:\n{ir}"));
    assert!(line.contains("internal constant"), "shared and unwritable: {line}");
    // The kind slot is a full i64: the low byte is the ObjKind the refcount ops
    // read, and the byte above it says which sort of callable this is, so the
    // renderer can print `<native lib fn somefn>` instead of `<object>`.
    let kind_word = OBJKIND_FN | (OBJ_FN_NATIVE << 8);
    assert!(
        line.contains(&format!(
            r#"{{ ptr @jrt_native_call, i64 {kind_word}, ptr @"native_env$0$somefn" }}"#
        )),
        "slots are {{ sentinel, kind, env }} in that order — with the kind at offset 8, \
         where jrt_decref would otherwise misread the env pointer as a kind: {line}"
    );
    assert!(
        line.contains("align 8"),
        "8-aligned, because TAG_PTR lives in the low three bits untag_ptr masks off: {line}"
    );
    assert!(ir.contains("tagptr"), "native fn value tagged TAG_PTR:\n{ir}");
}

/// A buffer a call site needs belongs in the entry block, because the lowered
/// code puts calls inside loops.
///
/// LLVM does not reclaim an `alloca` until the function returns, so marshalling
/// arguments into a fresh one at the call site walked the stack down 16 bytes
/// per iteration: an FFI call in a `while` loop died at a fixed count, and the
/// count scaled exactly with `ulimit -s`, which is what named it stack
/// exhaustion rather than a leak. The entry block runs once per frame, so
/// placement is the whole fix — and it is visible without a loop, since a
/// call-site alloca lands in the body block either way.
#[test]
fn native_call_argv_is_allocated_in_the_entry_block() {
    let ir = ir_of(
        &[
            GetGlobal(0, "__native$0$somefn".into()),
            LoadInt(1, 55),
            Call(2, 0, vec![1]),
            Return(Some(2)),
        ],
        3,
    );
    assert!(ir.contains("jrt_native_call"), "the call really is a native one:\n{ir}");
    let (entry, body) = ir.split_once("\nbb0:").expect("an entry block precedes the body:\n{ir}");
    assert!(entry.contains("nargv"), "the argv buffer is in the entry block:\n{ir}");
    assert!(!body.contains("alloca"), "and nothing allocas inside the body:\n{ir}");
}

/// A native fn value costs no allocation, so calling one in a loop does not grow
/// the heap.
///
/// `GetGlobal` materializes the value and `emit_native_fn_value` used to
/// `malloc` a box and its env each time, on the stated assumption that a
/// reference immediately called would be dead-code-eliminated. It is not — the
/// tagged word is stored into the register-file alloca, so LLVM must keep it —
/// and nothing frees it either, because the `ObjKind::Fn` that makes the box
/// safe for `jrt_decref` to skip is exactly what stops anything reclaiming it.
/// A compiled binary leaked 48 bytes per FFI call while `jade run` leaked none.
/// The value depends only on `(pkgid, fname)`, so it is now one shared constant.
#[test]
fn a_native_call_in_a_loop_allocates_nothing() {
    // i = 0; while i < 3 { somefn(55); i = i + 1 }
    // A jump's target is its own index + 1 + offset.
    let ir = ir_of(
        &[
            LoadInt(0, 0),                                 // 0: i
            LoadInt(1, 3),                                 // 1: limit
            CmpLtInt(2, 0, 1),                             // 2: i < limit
            JumpIfFalse(2, 6),                             // 3: → 10
            GetGlobal(3, "__native$0$somefn".to_string()), // 4
            LoadInt(4, 55),                                // 5
            Call(5, 3, vec![4]),                           // 6
            LoadInt(6, 1),                                 // 7
            AddInt(0, 0, 6),                               // 8: i = i + 1
            Jump(-8),                                      // 9: → 2
            Return(Some(5)),                               // 10
        ],
        7,
    );
    assert!(ir.contains("jrt_native_call"), "the loop really does call a native fn:\n{ir}");
    assert!(!ir.contains("call ptr @malloc"), "and allocates nothing to do it:\n{ir}");
    // The box and its env are link-time constants instead. `internal constant`
    // is the part worth pinning: a mutable global would be a shared object any
    // write could corrupt, and the whole reason one may be shared is that the
    // refcount ops skip it and nobody writes.
    assert!(
        ir.contains("@\"native_fnval$0$somefn\" = internal constant"),
        "the fn value is a shared constant:\n{ir}"
    );
    assert!(
        ir.contains("@\"native_env$0$somefn\" = internal constant"),
        "and so is its env:\n{ir}"
    );
}

#[test]
fn async_spawn_await_lower_to_runtime() {
    use std::sync::Arc;
    // async fn f(x): return x   ; fa = spawn f(1); await fa
    let f = Arc::new(CompiledFn {
        params: vec!["x".into()],
        defaults: vec![None],
        chunk: Chunk {
            name: "f".into(),
            code: vec![GetLocal(1, 0), Return(Some(1))],
            spans: vec![],
            fn_defs: vec![],
        },
        n_slots: 2,
        source_file: String::new(),
        module_scope: None,
        is_generator: false,
    });
    let mut top = Chunk::new("<top>");
    top.fn_defs.push(f);
    top.code = vec![
        LoadFn(0, 0),
        SetGlobal("f".into(), 0),
        GetGlobal(1, "f".into()),
        LoadInt(2, 1),
        Spawn(3, 1, vec![2]),
        Await(4, 3),
        Return(Some(4)),
    ];
    let context = Context::create();
    let module = context.create_module("t");
    lower_program(&context, &module, &top, 5, &HashMap::new(), &HashMap::new(), &HashMap::new())
        .expect("async lowering");
    module.verify().expect("module failed verification");
    let ir = module.print_to_string().to_string();
    assert!(ir.contains("@jf_task_0"), "task wrapper emitted:\n{ir}");
    assert!(ir.contains("jade_spawn"), "spawn via runtime:\n{ir}");
    // The word-taking entry point, not the pointer-taking one. Asserting
    // "jade_await" alone would pass either way, since it is a prefix of
    // "jade_await_word" — the test has to name the tagged form to detect a
    // regression back to raw pointers.
    assert!(ir.contains("jade_await_word"), "await takes a tagged word:\n{ir}");
    // A future is a tagged value now, so the spawn result is OR'd with
    // TAG_PTR rather than passed through as a bare pointer integer.
    assert!(ir.contains("tagptr"), "spawn result is TAG_PTR-tagged:\n{ir}");
}

#[test]
fn struct_make_field_and_typename_lower_to_runtime() {
    // p = Point{x: 10}; p.x; p.x = 20; typename(p)
    let ir = ir_of(
        &[
            LoadInt(0, 10),
            MakeStruct(1, "Point".to_string(), vec![("x".to_string(), 0, false)], None),
            GetField(2, 1, "x".to_string()),
            LoadInt(3, 20),
            SetField(1, "x".to_string(), 3),
            GetTypeName(4, 1),
            Return(Some(2)),
        ],
        5,
    );
    assert!(ir.contains("jrt_kstruct_new"), "struct allocated:\n{ir}");
    assert!(ir.contains("jrt_kstruct_set"), "fields set:\n{ir}");
    assert!(ir.contains("jrt_get_field"), "field read:\n{ir}");
    assert!(ir.contains("jrt_set_field"), "field written:\n{ir}");
    assert!(ir.contains("jrt_get_type_name"), "type name for typed catch:\n{ir}");
}

#[test]
fn in_operator_lowers_to_runtime_membership() {
    use crate::frontend::ast::BinOpKind;
    // r2 = (r0 in r1) → jrt_in_any → bool word
    let ir = ir_of(
        &[
            LoadStr(0, "x".to_string()),
            LoadStr(1, "xyz".to_string()),
            BinOp(2, BinOpKind::In, 0, 1),
            Return(Some(2)),
        ],
        3,
    );
    assert!(ir.contains("jrt_in_any"), "membership via runtime:\n{ir}");
    assert!(ir.contains("select i1"), "produces a bool word:\n{ir}");
}

#[test]
fn dict_make_and_index_lower_to_kind_runtime() {
    // d = {"k": 1}; d["k"]; d["k"] = 2   (kind-tagged dict, value semantics)
    let ir = ir_of(
        &[
            LoadStr(0, "k".to_string()),
            LoadInt(1, 1),
            MakeDict(2, vec![(0, 1)]),
            LoadStr(3, "k".to_string()),
            GetIndex(4, 2, 3),
            LoadStr(5, "k".to_string()),
            LoadInt(6, 2),
            SetIndex(2, 5, 6),
            Return(Some(4)),
        ],
        7,
    );
    assert!(ir.contains("jrt_kdict_new"), "dict allocated:\n{ir}");
    assert!(ir.contains("jrt_kdict_set"), "entries set:\n{ir}");
    assert!(ir.contains("jrt_val_index"), "index via runtime dispatch:\n{ir}");
    // SetIndex stores the returned container word back (value-semantic copy).
    assert!(ir.contains("jrt_val_set_index"), "set-index via runtime:\n{ir}");
}

#[test]
fn string_comparison_lowers_to_strcmp() {
    // r2 = ("a" < "b")  → strcmp on untagged data pointers, folded to a bool word.
    let ir = ir_of(
        &[
            LoadStr(0, "a".to_string()),
            LoadStr(1, "b".to_string()),
            CmpLtStr(2, 0, 1),
            Return(Some(2)),
        ],
        3,
    );
    assert!(ir.contains("call i32 @strcmp"), "compares via strcmp:\n{ir}");
    assert!(ir.contains("icmp slt"), "folds strcmp result by predicate:\n{ir}");
    assert!(ir.contains("select i1"), "produces a bool word:\n{ir}");
}

/// A struct type is not a function, and calling one must be a *named* build
/// error. It cannot simply be left to fall through: a type name is not a
/// known user function, so it would classify as an indirect call and jump
/// through a global cell codegen never assigns for a type name — a silent
/// miscompile rather than a diagnostic.
#[test]
fn calling_a_struct_type_is_a_named_build_error() {
    let mut struct_defs = HashMap::new();
    struct_defs.insert("City".to_string(), vec![StructFieldDef::Required("name".to_string())]);
    let mut top = Chunk::new("<top>");
    top.code =
        vec![MakeDict(0, vec![]), GetGlobal(1, "City".to_string()), Call(2, 1, vec![0]), Halt];
    let context = Context::create();
    let module = context.create_module("t");
    let err = match lower_program(
        &context,
        &module,
        &top,
        3,
        &struct_defs,
        &HashMap::new(),
        &HashMap::new(),
    ) {
        Err(e) => e,
        Ok(_) => panic!("calling a struct type should decline"),
    };
    assert!(err.contains("City"), "the error names the type: {err}");
    assert!(err.contains("not a function"), "the error explains why: {err}");
}

#[test]
fn conversion_builtins_devirtualize_to_runtime() {
    // int("42")  →  jrt_int_any
    let ir = ir_of(
        &[
            GetGlobal(0, "int".to_string()),
            LoadStr(1, "42".to_string()),
            Call(2, 0, vec![1]),
            Return(Some(2)),
        ],
        3,
    );
    assert!(ir.contains("jrt_int_any"), "int() lowered to runtime conversion:\n{ir}");
    // bool(x) and float(x) route to their own helpers.
    let ir2 = ir_of(
        &[GetGlobal(0, "bool".to_string()), LoadInt(1, 1), Call(2, 0, vec![1]), Return(Some(2))],
        3,
    );
    assert!(ir2.contains("jrt_bool_any"), "bool() lowered:\n{ir2}");
}

#[test]
fn write_builtin_lowers_to_the_flushing_writer() {
    // write("x") → jrt_write_any (print with no newline, flushed). Until
    // v1.1.34 this declined and the whole build failed, so a program using
    // `write` ran under the VM and could not be compiled at all.
    let ir = ir_of(
        &[
            GetGlobal(0, "write".to_string()),
            LoadStr(1, "x".to_string()),
            Call(2, 0, vec![1]),
            Return(Some(2)),
        ],
        3,
    );
    assert!(ir.contains("jrt_write_any"), "write lowered:\n{ir}");
    // print is a separate symbol — write must not silently become print,
    // which would add a newline the program did not ask for.
    assert!(!ir.contains("jrt_print_any"), "write is not print:\n{ir}");
}

#[test]
fn uhttp_stream_lowers_with_the_handler_as_a_value() {
    // uhttp.stream(url, handler) → jrt_uhttp_stream(url, fn word, headers).
    // The handler is passed as its whole tagged word, since the C driver
    // reads the function pointer out of the box the way array.map does.
    let ir = ir_of(
        &[
            GetGlobal(0, "uhttp".to_string()),
            GetField(1, 0, "stream".to_string()),
            LoadStr(2, "unix:///tmp/s.sock:/events".to_string()),
            LoadStr(3, "handler-placeholder".to_string()),
            Call(4, 1, vec![2, 3]),
            Return(Some(4)),
        ],
        5,
    );
    assert!(ir.contains("jrt_uhttp_stream"), "stream lowered:\n{ir}");
}

#[test]
fn the_byte_bodied_http_pair_lowers_on_both_modules() {
    // Until v1.2.5 these had no lowering at all: `jade check` accepted them and
    // `jade build` refused with "unsupported module call", so a program using a
    // byte body only discovered it could not ship at packaging time. Assert the
    // symbols by name, on both modules, so that cannot come back quietly.
    for module in ["http", "uhttp"] {
        let get = ir_of(
            &[
                GetGlobal(0, module.to_string()),
                GetField(1, 0, "get_bytes".to_string()),
                LoadStr(2, "u".to_string()),
                Call(3, 1, vec![2]),
                Return(Some(3)),
            ],
            4,
        );
        assert!(get.contains(&format!("jrt_{module}_get_bytes")), "{module}.get_bytes:\n{get}");

        let post = ir_of(
            &[
                GetGlobal(0, module.to_string()),
                GetField(1, 0, "post_bytes".to_string()),
                LoadStr(2, "u".to_string()),
                LoadStr(3, "body-placeholder".to_string()),
                Call(4, 1, vec![2, 3]),
                Return(Some(4)),
            ],
            5,
        );
        assert!(post.contains(&format!("jrt_{module}_post_bytes")), "{module}.post_bytes:\n{post}");
    }
}

#[test]
fn the_bytes_constructors_lower_to_their_raising_forwarders() {
    // The package tripwire below only checks `chunk_module_supported`. Two other
    // lists have to name a module too, and a green `cargo test` says nothing
    // about either: without `is_stdlib_module`, `bytes.zeros(4)` is classified
    // as a method call on a dict and declines; without `RESERVED_BUILTINS`, a
    // bare `bytes(...)` becomes an indirect call through a nil global cell.
    // Both are hard `jade build` failures on a real program, so assert the
    // symbols by name.
    let zeros = ir_of(
        &[
            GetGlobal(0, "bytes".to_string()),
            GetField(1, 0, "zeros".to_string()),
            LoadInt(2, 4),
            Call(3, 1, vec![2]),
            Return(Some(3)),
        ],
        4,
    );
    assert!(zeros.contains("jk_bytes_zeros"), "bytes.zeros:\n{zeros}");

    let from_ints = ir_of(
        &[
            GetGlobal(0, "bytes".to_string()),
            GetField(1, 0, "from_ints".to_string()),
            LoadInt(2, 1),
            MakeArray(3, vec![2]),
            Call(4, 1, vec![3]),
            Return(Some(4)),
        ],
        5,
    );
    assert!(from_ints.contains("jk_bytes_from_ints"), "bytes.from_ints:\n{from_ints}");

    let concat = ir_of(
        &[
            GetGlobal(0, "bytes".to_string()),
            GetField(1, 0, "concat".to_string()),
            LoadStr(2, "a".to_string()),
            LoadStr(3, "b".to_string()),
            Call(4, 1, vec![2, 3]),
            Return(Some(4)),
        ],
        5,
    );
    assert!(concat.contains("jk_bytes_concat"), "bytes.concat:\n{concat}");
}

/// `b[i] = v` goes through the same runtime entry point every index assignment
/// does, so the blob arm lives in one C function rather than in codegen.
#[test]
fn writing_an_octet_lowers_through_the_shared_set_index() {
    let ir = ir_of(
        &[
            GetGlobal(0, "bytes".to_string()),
            GetField(1, 0, "zeros".to_string()),
            LoadInt(2, 2),
            Call(3, 1, vec![2]),
            LoadInt(4, 0),
            LoadInt(5, 65),
            SetIndex(3, 4, 5),
            Return(Some(3)),
        ],
        6,
    );
    assert!(ir.contains("jrt_val_set_index"), "octet write via runtime dispatch:\n{ir}");
}

#[test]
fn str_builtin_devirtualizes_to_str_of_any() {
    // GetGlobal str ; LoadInt r1,42 ; Call r2 = str(r1) ; Return r2
    let ir = ir_of(
        &[GetGlobal(0, "str".to_string()), LoadInt(1, 42), Call(2, 0, vec![1]), Return(Some(2))],
        3,
    );
    assert!(ir.contains("jrt_str_of_any"), "str() lowered to runtime render:\n{ir}");
}

#[test]
fn print_falls_back_when_shadowed_by_a_user_global() {
    // If the program SetGlobals `print`, it is a user value, not the builtin
    // → the Call must NOT devirtualize (stays unsupported → fallback).
    let context = Context::create();
    let module = context.create_module("t");
    let err = lower_chunk(
        &context,
        &module,
        "f",
        &[
            LoadInt(0, 1),
            SetGlobal("print".to_string(), 0),
            GetGlobal(1, "print".to_string()),
            LoadInt(2, 5),
            Call(3, 1, vec![2]),
            Halt,
        ],
        4,
    )
    .unwrap_err();
    assert!(err.contains("unsupported call"), "got: {err}");
}

/// A build error names the line it happened on, like every interpreter error.
///
/// The position was the last thing missing from TOOLCHAIN-BUGS #19: the message
/// named `lower.rs` and the construct rather than the mistake, and gave a large
/// file nothing to search for. `Chunk` has always carried a span per
/// instruction; it just never reached the resolver.
#[test]
fn a_build_error_from_the_resolver_names_its_line() {
    use crate::frontend::error::Span;
    let mut chunk = crate::bytecode::Chunk::new("t");
    // `x.nosuchmethod()` — a GetField result that is then called.
    chunk.code = vec![
        LoadStr(0, "hi".to_string()),
        GetField(1, 0, "nosuchmethod".to_string()),
        Call(2, 1, vec![]),
        Return(Some(2)),
    ];
    chunk.spans = vec![
        Span { line: 1, col: 1 },
        Span { line: 7, col: 3 },
        Span { line: 7, col: 3 },
        Span { line: 8, col: 1 },
    ];
    let context = Context::create();
    let module = context.create_module("t");
    let function = module.add_function("f", context.i64_type().fn_type(&[], false), None);
    let err = lower_body(
        &context,
        &module,
        function,
        &chunk,
        &FnCtx::empty(),
        BodyOpts { n_slots: 3, n_params: 0, is_generator: false, track_recursion: false },
    )
    .expect_err("a method no type defines should be refused");

    assert!(err.starts_with("[7:3]"), "should name the line: {err}");
    assert!(err.contains("nosuchmethod"), "should name the method: {err}");
    assert!(!err.contains("lower.rs"), "should not name a Rust source file: {err}");
    assert!(!err.contains("unsupported"), "method calls are not unsupported: {err}");
}

// ── Unbound globals ───────────────────────────────────────────────────────────

/// Build a top-level chunk with spans, for the whole-program checks.
fn top_chunk(code: Vec<Instr>) -> Chunk {
    use crate::frontend::error::Span;
    let spans = (0..code.len()).map(|i| Span { line: i + 1, col: 1 }).collect();
    Chunk { name: "<top>".to_string(), code, spans, fn_defs: vec![] }
}

#[test]
fn reading_a_global_nothing_binds_is_a_build_error() {
    // `exit(0)` — the call type inference has to let through in a file that
    // imports a user module. It used to lower to a read of a nil global and
    // build a binary that trapped in the runtime with nothing printed.
    let chunk =
        top_chunk(vec![GetGlobal(0, "exit".to_string()), LoadInt(1, 0), Call(2, 0, vec![1]), Halt]);
    let err = check_globals_bound(&chunk, &[], &HashMap::new(), &HashMap::new())
        .expect_err("an unbound global should be refused");

    assert!(err.starts_with("[1:1]"), "should name the line: {err}");
    assert!(err.contains("undefined variable 'exit'"), "should name the variable: {err}");
    assert!(err.contains("raise"), "should name what to write instead: {err}");
}

#[test]
fn a_global_the_program_binds_is_fine_in_either_order() {
    // Forward reference: a function body reads a global the top level stores to
    // later. Checking per-chunk would reject this, so the scan is whole-program.
    let body = top_chunk(vec![GetGlobal(0, "later".to_string()), Return(Some(0))]);
    let f = std::sync::Arc::new(CompiledFn {
        params: vec![],
        defaults: vec![],
        chunk: body,
        n_slots: 1,
        source_file: String::new(),
        module_scope: None,
        is_generator: false,
    });
    let top = top_chunk(vec![LoadInt(0, 1), SetGlobal("later".to_string(), 0), Halt]);
    check_globals_bound(&top, &[f], &HashMap::new(), &HashMap::new())
        .expect("a global bound anywhere in the program is bound");
}

#[test]
fn runtime_builtins_and_package_globals_are_bound() {
    // Neither is a `SetGlobal` target, and both are legal to read. The allowed
    // set is read from `builtins` rather than restated, so adding a builtin
    // cannot make a valid program stop building.
    let top =
        top_chunk(vec![GetGlobal(0, "print".to_string()), GetGlobal(1, "fs".to_string()), Halt]);
    check_globals_bound(&top, &[], &HashMap::new(), &HashMap::new())
        .expect("builtins and package globals are bound");
}

// ── The VM's package tables and this backend's lowering ───────────────────────

/// Every stdlib package function the interpreter exposes must be lowerable here
/// at some arity.
///
/// A module call this backend declines is a *hard build error*, not a fallback,
/// so a function present in a package's `fns` table and absent from
/// `chunk_module_supported` is a program `jade run` accepts and `jade build`
/// refuses. That is not hypothetical: `string.upper(s)`, `dict.keys(d)` and a
/// dozen more sat in exactly that state until v1.3.21, and nothing prevented it
/// happening again. This is what prevents it.
///
/// Arity is not checked, only that *some* arity lowers — the predicate is the
/// authority on which, and the VM does its own arity checking per function.
#[test]
fn every_package_fn_lowers_in_the_chunk_backend() {
    let mut missing = Vec::new();
    for pkg in crate::builtins::all_packages() {
        let names = pkg.fns.iter().map(|f| f.name).chain(pkg.natives.iter().map(|(n, _)| *n));
        for name in names {
            if !(0..=4).any(|argc| chunk_module_supported(pkg.global_name, name, argc)) {
                missing.push(format!("{}.{name}", pkg.global_name));
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "{} package function(s) the VM accepts have no lowering, so `jade build` \
         refuses a program `jade run` runs:\n  {}\n\
         Add an arm to `chunk_module_supported` and `emit_module_call`.",
        missing.len(),
        missing.join("\n  ")
    );
}
