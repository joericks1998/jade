//! Lowering one MIR function into LLVM.
//!
//! Every MIR local becomes a stack slot, zeroed on entry so that releasing a
//! local nothing assigned is releasing null, which does nothing. `mem2reg`
//! turns the slots into registers. Parameters are copied into their slots and
//! retained, so the function owns them like any other local, and every heap
//! local is released when the function returns.
//!
//! The ownership rules applied here are the ones `mir/mod.rs` states: an
//! assignment retains a borrowed result before releasing the old value, a
//! `Move` operand clears its temporary instead of retaining, and the operands
//! an operation keeps (an element pushed, a field set) are consumed.

use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue, GlobalValue, IntValue, PointerValue};
use inkwell::{FloatPredicate, IntPredicate};

use super::layout::{self, ARR_DATA, LEN, TYPE_ID};
use super::{Cx, F, tasks};
use crate::mir::{
    ArithOp, BinOp, BitOp, CmpOp, Const, Func, NumTy, Op, Operand, Place, Program, Rvalue, Stmt,
    StrMethod, Term, Ty, UnOp, is_heap,
};

/// Purpose: This struct holds what every function's lowering needs from the
/// whole program.
pub struct Env<'a, 'ctx> {
    pub prog: &'a Program,
    pub funcs: &'a [FunctionValue<'ctx>],
    pub globals: &'a [GlobalValue<'ctx>],
}

/// Purpose: This struct holds the state of lowering one function.
struct Lw<'a, 'b, 'ctx> {
    f: F<'b, 'ctx>,
    env: &'a Env<'a, 'ctx>,
    func: &'a Func,
    slots: Vec<PointerValue<'ctx>>,
    blocks: Vec<BasicBlock<'ctx>>,
}

/// Purpose: This function lowers one MIR function into its LLVM function.
///
/// Inputs:
/// - `cx`: the module.
/// - `env`: the program's functions and globals.
/// - `func`: the MIR function.
/// - `fv`: its declared LLVM function.
///
/// Outputs: none.
pub fn lower<'a, 'ctx>(
    cx: &Cx<'ctx>,
    env: &'a Env<'a, 'ctx>,
    func: &'a Func,
    fv: FunctionValue<'ctx>,
) {
    let f = F::new(cx, fv);
    let mut slots = Vec::with_capacity(func.locals.len());
    for (i, l) in func.locals.iter().enumerate() {
        let t = cx.llty(&l.ty);
        let p = f.alloca(t);
        if (i as u32) < func.params {
            let v = f.param(i as u32);
            f.store(p, v);
            f.retain(&l.ty, v);
        } else {
            f.store(p, layout::zero(t));
        }
        slots.push(p);
    }
    // Count the call against the recursion limit, raising past it as v1 did.
    // The top level is not a call, so it does not count.
    let d = cx.depth_slot();
    let counts = if func.name == "main" { 0 } else { 1 };
    let depth = f.add(f.load_i64(d), f.i64(counts));
    f.store(d, depth);
    let too_deep = f.sgt(depth, f.i64(super::errors::RECURSION_LIMIT));
    let blocks: Vec<BasicBlock> = func.blocks.iter().map(|_| f.block()).collect();
    let refuse = f.block();
    f.cond_br(too_deep, refuse, blocks[0]);
    f.position(refuse);
    f.raise("recursion limit exceeded");
    for (i, l) in func.locals.iter().enumerate() {
        if is_heap(&l.ty) {
            let old = f.load(cx.llty(&l.ty), slots[i]);
            f.release(&l.ty, old);
        }
    }
    f.store(d, f.sub(f.load_i64(d), f.i64(1)));
    f.ret(layout::zero(cx.llty(&func.ret)));
    let lw = Lw { f, env, func, slots, blocks };
    for (i, b) in func.blocks.iter().enumerate() {
        lw.f.position(lw.blocks[i]);
        for s in &b.stmts {
            lw.stmt(s);
            if lw.f.terminated() {
                let dead = lw.f.block();
                lw.f.position(dead);
            }
        }
        lw.term(&b.term);
    }
}

impl<'a, 'b, 'ctx> Lw<'a, 'b, 'ctx> {
    fn cx(&self) -> &'b Cx<'ctx> {
        self.f.cx
    }

    // ── places and operands ──────────────────────────────────────────────

    fn place_ptr(&self, p: &Place) -> PointerValue<'ctx> {
        match p {
            Place::Local(l) => self.slots[l.0 as usize],
            Place::Global(g) => self.env.globals[g.0 as usize].as_pointer_value(),
        }
    }

    fn place_ty(&self, p: &Place) -> Ty {
        match p {
            Place::Local(l) => self.func.locals[l.0 as usize].ty.clone(),
            Place::Global(g) => self.env.prog.globals[g.0 as usize].ty.clone(),
        }
    }

    fn op_ty(&self, op: &Operand) -> Ty {
        match op {
            Operand::Copy(p) => self.place_ty(p),
            Operand::Move(l) => self.func.locals[l.0 as usize].ty.clone(),
            Operand::Const(c) => match c {
                Const::Int(_) => Ty::Int,
                Const::Float(_) => Ty::Float,
                Const::Bool(_) => Ty::Bool,
                Const::Char(_) => Ty::Char,
                Const::Str(_) => Ty::Str,
                Const::Nil(t) | Const::Static(t) => t.clone(),
            },
        }
    }

    /// Purpose: This function reads an operand, borrowing it; a `Move` clears
    /// its temporary.
    fn operand(&self, op: &Operand) -> BasicValueEnum<'ctx> {
        let f = &self.f;
        match op {
            Operand::Copy(p) => {
                let t = self.cx().llty(&self.place_ty(p));
                f.load(t, self.place_ptr(p))
            }
            Operand::Move(l) => {
                let ty = &self.func.locals[l.0 as usize].ty;
                let t = self.cx().llty(ty);
                let p = self.slots[l.0 as usize];
                let v = f.load(t, p);
                f.store(p, layout::zero(t));
                v
            }
            Operand::Const(c) => match c {
                Const::Int(i) => f.i64(*i).into(),
                Const::Float(x) => f.f64(*x).into(),
                Const::Bool(b) => f.bool(*b).into(),
                Const::Char(ch) => f.i32(*ch as i64).into(),
                Const::Str(s) => self.cx().lit(s).into(),
                Const::Nil(t) | Const::Static(t) => layout::zero(self.cx().llty(t)),
            },
        }
    }

    /// Purpose: This function reads an operand that will be stored, so the
    /// store owns a reference: a `Move` hands one over, anything else is
    /// retained.
    fn consume(&self, op: &Operand) -> BasicValueEnum<'ctx> {
        let v = self.operand(op);
        if !matches!(op, Operand::Move(_)) {
            self.f.retain(&self.op_ty(op), v);
        }
        v
    }

    /// Purpose: This function addresses a field of an object read through its
    /// static class.
    ///
    /// A child keeps its first parent's fields where the parent keeps them, so
    /// usually the static class's offset is right for every object it can hold.
    /// A class reached as a second parent keeps its fields elsewhere in the
    /// child. When any descendant of the static class does that, the address is
    /// chosen by the object's class id, which is the per-class table design
    /// decision 9 of `design/v2-classes.md` describes, written as a switch.
    ///
    /// Inputs:
    /// - `obj`: the object.
    /// - `strukt`: its static class.
    /// - `index`: the field's index in that class's layout.
    ///
    /// Outputs: a pointer to the field.
    fn field_ptr(&self, obj: PointerValue<'ctx>, strukt: &str, index: u32) -> PointerValue<'ctx> {
        let f = &self.f;
        let cx = self.cx();
        let st = cx.struct_ty(strukt);
        let direct = f.field(st, obj, index + 2);
        let Some(field) = cx
            .structs
            .iter()
            .find(|s| s.name == strukt)
            .and_then(|s| s.fields.get(index as usize))
            .map(|(n, _)| n.clone())
        else {
            return direct;
        };
        let moved: Vec<(usize, u32)> = cx
            .structs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.ancestors.iter().any(|a| a == strukt))
            .filter_map(|(id, s)| {
                let i = s.fields.iter().position(|(n, _)| *n == field)? as u32;
                (i != index).then_some((id, i))
            })
            .collect();
        if moved.is_empty() {
            return direct;
        }
        let at = f.var(direct);
        let tid = f.type_id(obj);
        let end = f.block();
        let mut cases = Vec::new();
        let mut bodies = Vec::new();
        for (id, i) in &moved {
            let b = f.block();
            cases.push((f.i64(*id as i64), b));
            bodies.push((b, *id, *i));
        }
        f.switch(tid, end, &cases);
        for (b, id, i) in bodies {
            f.position(b);
            let child = cx.struct_ty(&cx.structs[id].name);
            f.store(at, f.field(child, obj, i + 2));
            f.br(end);
        }
        f.position(end);
        f.load_ptr(at)
    }

    fn nil(&self) -> BasicValueEnum<'ctx> {
        self.f.i8(0).into()
    }

    /// Purpose: This function stores pointer operands in a stack array, for a
    /// support function that takes any number of futures.
    fn pointer_array(&self, args: &[Operand]) -> PointerValue<'ctx> {
        let f = &self.f;
        let a = f.alloca(self.cx().ptrt().array_type(args.len().max(1) as u32));
        for (i, op) in args.iter().enumerate() {
            f.store(f.elem(self.cx().ptrt(), a, f.i64(i as i64)), self.operand(op));
        }
        a
    }

    // ── statements ───────────────────────────────────────────────────────

    fn stmt(&self, s: &Stmt) {
        let f = &self.f;
        match s {
            Stmt::Assign(p, rv) => {
                let t = self.place_ty(p);
                let (v, _) = self.rvalue(rv);
                if rv.borrows_result() {
                    f.retain(&t, v);
                }
                let ptr = self.place_ptr(p);
                let old = f.load(self.cx().llty(&t), ptr);
                f.store(ptr, v);
                f.release(&t, old);
            }
            Stmt::Eval(rv) => {
                let (v, t) = self.rvalue(rv);
                if !rv.borrows_result() && !f.terminated() {
                    f.release(&t, v);
                }
            }
            Stmt::SetField { object, strukt, index, value } => {
                let obj = self.operand(object).into_pointer_value();
                let v = self.consume(value);
                let fields = self.cx().struct_fields(strukt);
                let ft = fields.get(*index as usize).cloned().unwrap_or(Ty::Nil);
                let fp = self.field_ptr(obj, strukt, *index);
                let old = f.load(self.cx().llty(&ft), fp);
                f.store(fp, v);
                f.release(&ft, old);
            }
            Stmt::DictSet { dict, key, value } => {
                let Ty::Map(vt) = self.place_ty(dict) else { return };
                let k = self.consume(key);
                let v = self.consume(value);
                let ptr = self.place_ptr(dict);
                let d = f.load_ptr(ptr);
                let u = f.call_ptr(self.cx().dict_fn("unique", &vt), &[d.into()]);
                f.store(ptr, u);
                f.call(self.cx().dict_fn("set", &vt), &[u.into(), k, v]);
            }
            Stmt::Raise(op) => {
                let v = self.consume(op);
                f.call(self.cx().set_err_fn(), &[v]);
            }
            Stmt::Drop(p) => {
                let t = self.place_ty(p);
                if is_heap(&t) {
                    let lt = self.cx().llty(&t);
                    let ptr = self.place_ptr(p);
                    let v = f.load(lt, ptr);
                    f.store(ptr, layout::zero(lt));
                    f.release(&t, v);
                }
            }
        }
    }

    fn term(&self, t: &Term) {
        let f = &self.f;
        match t {
            Term::Goto(b) => f.br(self.blocks[b.0 as usize]),
            Term::If(c, a, b) => {
                let c = self.operand(c).into_int_value();
                f.cond_br(c, self.blocks[a.0 as usize], self.blocks[b.0 as usize]);
            }
            Term::Switch { value, cases, default } => {
                let v = self.operand(value).into_int_value();
                let cs: Vec<(IntValue, BasicBlock)> =
                    cases.iter().map(|(k, b)| (f.i64(*k), self.blocks[b.0 as usize])).collect();
                f.switch(v, self.blocks[default.0 as usize], &cs);
            }
            Term::Return(op) => {
                let v = self.consume(op);
                self.leave();
                f.ret(v);
            }
            Term::Unwind => {
                self.leave();
                f.ret(layout::zero(self.cx().llty(&self.func.ret)));
            }
            Term::Unreachable => f.unreachable(),
        }
    }

    /// Purpose: This function does what every way out of a function does:
    /// release each local and count the call off the recursion depth.
    fn leave(&self) {
        let f = &self.f;
        for (i, l) in self.func.locals.iter().enumerate() {
            if is_heap(&l.ty) {
                let old = f.load(self.cx().llty(&l.ty), self.slots[i]);
                f.release(&l.ty, old);
            }
        }
        let d = self.cx().depth_slot();
        let counts = if self.func.name == "main" { 0 } else { 1 };
        f.store(d, f.sub(f.load_i64(d), f.i64(counts)));
    }

    // ── rvalues ──────────────────────────────────────────────────────────

    /// Purpose: This function computes an rvalue.
    ///
    /// Outputs: its value and type.
    fn rvalue(&self, rv: &Rvalue) -> (BasicValueEnum<'ctx>, Ty) {
        let f = &self.f;
        let cx = self.cx();
        match rv {
            Rvalue::Use(op) => (self.operand(op), self.op_ty(op)),
            Rvalue::Bin(op, a, b) => self.binary(op, a, b),
            Rvalue::Un(op, a) => {
                let v = self.operand(a);
                match op {
                    UnOp::Neg(NumTy::Int) => {
                        let (r, o) = f.checked("ssub", f.i64(0), v.into_int_value());
                        f.raise_if(o, "integer overflow");
                        (r.into(), Ty::Int)
                    }
                    UnOp::Neg(NumTy::Float) => (f.fneg(v.into_float_value()).into(), Ty::Float),
                    UnOp::Not => (f.not(v.into_int_value()).into(), Ty::Bool),
                    UnOp::BitNot => (f.not(v.into_int_value()).into(), Ty::Int),
                }
            }
            Rvalue::Call(id, args) => {
                let vals: Vec<BasicValueEnum> = args.iter().map(|a| self.operand(a)).collect();
                let callee = self.env.funcs[id.0 as usize];
                let ret = self.env.prog.funcs[id.0 as usize].ret.clone();
                (f.callv(callee, &vals), ret)
            }
            Rvalue::Op(op, args) => self.op(op, args),
            Rvalue::Array(t, els) => {
                let a = f.array_new(t, f.i64(els.len() as i64));
                let data = f.load_ptr(f.at_c(a, ARR_DATA));
                let et = cx.llty(t);
                for (i, e) in els.iter().enumerate() {
                    let v = self.consume(e);
                    f.store(f.elem(et, data, f.i64(i as i64)), v);
                }
                f.store(f.at_c(a, LEN), f.i64(els.len() as i64));
                (a.into(), Ty::Array(Box::new(t.clone())))
            }
            Rvalue::Dict(t, entries) => {
                let pairs: Vec<(BasicValueEnum, BasicValueEnum)> =
                    entries.iter().map(|(k, v)| (self.consume(k), self.consume(v))).collect();
                (f.dict_literal(t, &pairs).into(), Ty::Map(Box::new(t.clone())))
            }
            Rvalue::Struct(name, fields) => {
                let st = cx.struct_ty(name);
                let size = f.size_of(st.into());
                let p = f.call_ptr(cx.libc("malloc"), &[size.into()]);
                f.store(p, f.i64(1));
                f.store(f.at_c(p, TYPE_ID), f.i64(cx.struct_id(name) as i64));
                for (i, v) in fields.iter().enumerate() {
                    let v = self.consume(v);
                    f.store(f.field(st, p, i as u32 + 2), v);
                }
                (p.into(), Ty::Struct(name.clone()))
            }
            Rvalue::Field { object, strukt, index } => {
                let obj = self.operand(object).into_pointer_value();
                let ft = cx.struct_fields(strukt).get(*index as usize).cloned().unwrap_or(Ty::Nil);
                let v = f.load(cx.llty(&ft), self.field_ptr(obj, strukt, *index));
                (v, ft)
            }
            Rvalue::Wrap(t, op) => (f.wrap(t, self.operand(op)), t.clone()),
            Rvalue::Unwrap(op) => {
                let t = self.op_ty(op);
                let inner = match &t {
                    Ty::Opt(i) => (**i).clone(),
                    other => other.clone(),
                };
                (f.unwrap(&t, self.operand(op)), inner)
            }
            Rvalue::IsNil(op) => {
                let t = self.op_ty(op);
                (f.is_nil(&t, self.operand(op)).into(), Ty::Bool)
            }
            Rvalue::Spawn(id, args) => {
                let target = self.env.funcs[id.0 as usize];
                let mf = &self.env.prog.funcs[id.0 as usize];
                let params: Vec<Ty> =
                    mf.locals[..mf.params as usize].iter().map(|l| l.ty.clone()).collect();
                let at = cx.args_ty(&params);
                let size = f.size_of(at.into());
                let packed = f.call_ptr(cx.libc("malloc"), &[size.into()]);
                for (i, a) in args.iter().enumerate() {
                    let v = self.consume(a);
                    f.store(f.field(at, packed, i as u32), v);
                }
                let entry = cx.task_entry_fn(target, &params, &mf.ret);
                let job = f.new_job(&mf.ret, entry, packed);
                (job.into(), Ty::Future(Box::new(mf.ret.clone())))
            }
            Rvalue::FnAddr(id) => {
                (self.env.funcs[id.0 as usize].as_global_value().as_pointer_value().into(), Ty::Nil)
            }
            Rvalue::CallIndirect(c, args) => {
                let Ty::Fn(ft) = self.op_ty(c) else {
                    return (layout::zero(cx.i8t().into()), Ty::Nil);
                };
                let callee = self.operand(c).into_pointer_value();
                let vals: Vec<BasicValueEnum> = args.iter().map(|a| self.operand(a)).collect();
                let params: Vec<inkwell::types::BasicMetadataTypeEnum> =
                    ft.params.iter().map(|p| cx.llty(&crate::mir::lower::norm(p)).into()).collect();
                let ret = crate::mir::lower::norm(&ft.ret);
                let fn_ty = inkwell::types::BasicType::fn_type(&cx.llty(&ret), &params, false);
                let meta: Vec<inkwell::values::BasicMetadataValueEnum> =
                    vals.iter().map(|v| (*v).into()).collect();
                let cs = f.b.build_indirect_call(fn_ty, callee, &meta, "").expect("positioned");
                let v = match cs.try_as_basic_value() {
                    inkwell::values::ValueKind::Basic(v) => v,
                    inkwell::values::ValueKind::Instruction(_) => layout::zero(cx.i8t().into()),
                };
                (v, ret)
            }
            Rvalue::ErrPending => (f.err_pending().into(), Ty::Bool),
            Rvalue::TakeErr => {
                let slot = cx.err_slot();
                let e = f.load_ptr(slot);
                f.store(slot, f.null());
                (e.into(), Ty::Error)
            }
            Rvalue::IntToFloat(op) => (f.itof(self.operand(op).into_int_value()).into(), Ty::Float),
            Rvalue::TypeId(op) => {
                let p = self.operand(op).into_pointer_value();
                (f.type_id(p).into(), Ty::Int)
            }
            Rvalue::IsA(op, ids) => {
                let p = self.operand(op).into_pointer_value();
                let tid = f.type_id(p);
                let mut any = f.bool(false);
                for id in ids {
                    any = f.or(any, f.eq(tid, f.i64(*id)));
                }
                (any.into(), Ty::Bool)
            }
        }
    }

    /// Purpose: This function computes a binary operator.
    fn binary(&self, op: &BinOp, a: &Operand, b: &Operand) -> (BasicValueEnum<'ctx>, Ty) {
        let f = &self.f;
        let (x, y) = (self.operand(a), self.operand(b));
        match op {
            BinOp::Arith(o, NumTy::Int) => {
                let (x, y) = (x.into_int_value(), y.into_int_value());
                let r = match o {
                    ArithOp::Add | ArithOp::Sub | ArithOp::Mul => {
                        let name = match o {
                            ArithOp::Add => "sadd",
                            ArithOp::Sub => "ssub",
                            _ => "smul",
                        };
                        let (r, over) = f.checked(name, x, y);
                        f.raise_if(over, "integer overflow");
                        r
                    }
                    // A raise only marks the error; the division still runs, so
                    // it divides by something safe and the result is discarded.
                    ArithOp::Div => {
                        let zero = f.eq(y, f.i64(0));
                        f.raise_if(zero, "division by zero");
                        let edge = f.and(f.eq(x, f.i64(i64::MIN)), f.eq(y, f.i64(-1)));
                        f.raise_if(edge, "integer overflow");
                        let safe = f.select_int(f.or(zero, edge), f.i64(1), y);
                        f.sdiv(x, safe)
                    }
                    ArithOp::Rem => {
                        let zero = f.eq(y, f.i64(0));
                        f.raise_if(zero, "modulo by zero");
                        // i64::MIN % -1 is 0, but `srem` would trap on it.
                        let edge = f.eq(y, f.i64(-1));
                        let safe = f.select_int(f.or(zero, edge), f.i64(1), y);
                        let r = f.srem(x, safe);
                        f.select_int(edge, f.i64(0), r)
                    }
                };
                (r.into(), Ty::Int)
            }
            BinOp::Arith(o, NumTy::Float) => {
                let (x, y) = (x.into_float_value(), y.into_float_value());
                let r = match o {
                    ArithOp::Add => f.fadd(x, y),
                    ArithOp::Sub => f.fsub(x, y),
                    ArithOp::Mul => f.fmul(x, y),
                    ArithOp::Div => {
                        f.raise_if(f.fcmp(FloatPredicate::OEQ, y, f.f64(0.0)), "division by zero");
                        f.fdiv(x, y)
                    }
                    ArithOp::Rem => {
                        f.raise_if(f.fcmp(FloatPredicate::OEQ, y, f.f64(0.0)), "modulo by zero");
                        f.frem(x, y)
                    }
                };
                (r.into(), Ty::Float)
            }
            BinOp::Bit(o) => {
                let (x, y) = (x.into_int_value(), y.into_int_value());
                let r = match o {
                    BitOp::And => f.and(x, y),
                    BitOp::Or => f.or(x, y),
                    BitOp::Xor => f.xor(x, y),
                    BitOp::Shl | BitOp::Shr => {
                        let bad = f.or(f.slt(y, f.i64(0)), f.sge(y, f.i64(64)));
                        f.if_(bad, |f| {
                            let text = f.int_text(y);
                            let sb = f.sb_new();
                            f.sb_push_lit(sb, "invalid shift amount ");
                            f.sb_push_str(sb, text);
                            let m = f.sb_finish(sb);
                            f.raise_str(m);
                        });
                        // A shift of 64 or more is undefined in LLVM, so the
                        // amount is kept in range even when it was refused.
                        let y = f.and(y, f.i64(63));
                        if *o == BitOp::Shl {
                            let r = f.shl(x, y);
                            let lost = f.and(f.not(bad), f.ne(f.ashr(r, y), x));
                            f.raise_if(lost, "integer overflow");
                            r
                        } else {
                            f.ashr(x, y)
                        }
                    }
                };
                (r.into(), Ty::Int)
            }
            BinOp::Cmp(c, t) => (self.compare(*c, t, x, y).into(), Ty::Bool),
            BinOp::Concat => {
                let s = f.call_ptr(self.cx().str_concat_fn(), &[x, y]);
                (s.into(), Ty::Str)
            }
        }
    }

    /// Purpose: This function compares two values of one type.
    fn compare(
        &self,
        c: CmpOp,
        t: &Ty,
        x: BasicValueEnum<'ctx>,
        y: BasicValueEnum<'ctx>,
    ) -> IntValue<'ctx> {
        let f = &self.f;
        match (c, t) {
            (CmpOp::Eq, _) => f.eq_values(t, x, y),
            (CmpOp::Ne, Ty::Float) => {
                f.fcmp(FloatPredicate::UNE, x.into_float_value(), y.into_float_value())
            }
            (CmpOp::Ne, _) => f.not(f.eq_values(t, x, y)),
            (_, Ty::Int) => {
                let p = match c {
                    CmpOp::Lt => IntPredicate::SLT,
                    CmpOp::Le => IntPredicate::SLE,
                    CmpOp::Gt => IntPredicate::SGT,
                    _ => IntPredicate::SGE,
                };
                f.icmp(p, x.into_int_value(), y.into_int_value())
            }
            (_, Ty::Float) => {
                let p = match c {
                    CmpOp::Lt => FloatPredicate::OLT,
                    CmpOp::Le => FloatPredicate::OLE,
                    CmpOp::Gt => FloatPredicate::OGT,
                    _ => FloatPredicate::OGE,
                };
                f.fcmp(p, x.into_float_value(), y.into_float_value())
            }
            _ => {
                let r = f.cmp_values(t, x, y);
                let p = match c {
                    CmpOp::Lt => IntPredicate::SLT,
                    CmpOp::Le => IntPredicate::SLE,
                    CmpOp::Gt => IntPredicate::SGT,
                    _ => IntPredicate::SGE,
                };
                f.icmp(p, r, f.i32(0))
            }
        }
    }

    /// Purpose: This function performs a support operation.
    fn op(&self, op: &Op, args: &[Operand]) -> (BasicValueEnum<'ctx>, Ty) {
        let f = &self.f;
        let cx = self.cx();
        let arg = |i: usize| self.operand(&args[i]);
        let ptr = |i: usize| self.operand(&args[i]).into_pointer_value();
        match op {
            Op::Print => {
                f.call(cx.print_fn(), &[arg(0), arg(1)]);
                (self.nil(), Ty::Nil)
            }
            Op::Input => (f.callv(cx.input_fn(), &[arg(0)]), Ty::Str),
            Op::ToStr(t) => (f.callv(cx.tostr_fn(t), &[arg(0)]), Ty::Str),
            Op::StrCat => {
                let parts: Vec<PointerValue> = (0..args.len()).map(ptr).collect();
                (f.str_cat(&parts).into(), Ty::Str)
            }
            Op::CharToStr => (f.callv(cx.char_tostr_fn(), &[arg(0)]), Ty::Str),
            Op::StrLen => (f.callv(cx.str_charlen_fn(), &[arg(0)]), Ty::Int),
            Op::StrIndex => (f.callv(cx.str_index_fn(), &[arg(0), arg(1)]), Ty::Char),
            Op::StrChars => (f.callv(cx.str_chars_fn(), &[arg(0)]), Ty::Array(Box::new(Ty::Char))),
            Op::Str(m) => {
                let vals: Vec<BasicValueEnum> = (0..args.len()).map(arg).collect();
                let t = match m {
                    StrMethod::Split | StrMethod::Lines => Ty::Array(Box::new(Ty::Str)),
                    StrMethod::Contains
                    | StrMethod::StartsWith
                    | StrMethod::EndsWith
                    | StrMethod::IsEmpty => Ty::Bool,
                    StrMethod::IndexOf | StrMethod::LastIndexOf | StrMethod::Count => Ty::Int,
                    _ => Ty::Str,
                };
                (f.str_method(*m, &vals), t)
            }
            Op::ToInt(t) => {
                let v = arg(0);
                let r = match t {
                    Ty::Float => {
                        let intr = inkwell::intrinsics::Intrinsic::find("llvm.fptosi.sat")
                            .expect("intrinsic");
                        let decl = intr
                            .get_declaration(&cx.module, &[cx.i64t().into(), cx.f64t().into()])
                            .expect("declaration");
                        f.call_int(decl, &[v])
                    }
                    Ty::Bool | Ty::Char => f.zext(v.into_int_value(), cx.i64t()),
                    _ => f.call_int(cx.toint_str_fn(), &[v]),
                };
                (r.into(), Ty::Int)
            }
            Op::ToFloat(_) => (f.callv(cx.tofloat_str_fn(), &[arg(0)]), Ty::Float),
            Op::ToBool(t) => {
                let v = arg(0);
                let r = match t {
                    Ty::Int => f.ne(v.into_int_value(), f.i64(0)),
                    _ => f.call_int(cx.tobool_str_fn(), &[v]),
                };
                (r.into(), Ty::Bool)
            }
            Op::ToChar(t) => {
                let r = match t {
                    Ty::Int => f.callv(cx.tochar_int_fn(), &[arg(0)]),
                    _ => f.callv(cx.tochar_str_fn(), &[arg(0)]),
                };
                (r, Ty::Char)
            }
            Op::ArrayLen | Op::DictLen => (f.load_i64(f.at_c(ptr(0), LEN)).into(), Ty::Int),
            Op::ArrayGet(t) => (f.callv(cx.array_fn("get", t), &[arg(0), arg(1)]), t.clone()),
            Op::ArraySet(t) => {
                let (a, i) = (arg(0), arg(1));
                let v = self.consume(&args[2]);
                f.call(cx.array_fn("set", t), &[a, i, v]);
                (self.nil(), Ty::Nil)
            }
            Op::ArrayPush(t) => {
                let a = arg(0);
                let v = self.consume(&args[1]);
                f.call(cx.array_fn("push", t), &[a, v]);
                (self.nil(), Ty::Nil)
            }
            Op::ArrayPop(t) => (f.callv(cx.array_fn("pop", t), &[arg(0)]), t.clone()),
            Op::ArrayContains(t) => {
                (f.callv(cx.array_fn("contains", t), &[arg(0), arg(1)]), Ty::Bool)
            }
            Op::ArraySort(t) | Op::ArrayReverse(t) => {
                let which = if matches!(op, Op::ArraySort(_)) { "sort" } else { "reverse" };
                f.call(cx.array_fn(which, t), &[arg(0)]);
                (self.nil(), Ty::Nil)
            }
            Op::ArraySorted(t) | Op::ArrayReversed(t) => {
                let copy = f.callv(cx.array_fn("copy", t), &[arg(0)]);
                let which = if matches!(op, Op::ArraySorted(_)) { "sort" } else { "reverse" };
                f.call(cx.array_fn(which, t), &[copy]);
                (copy, Ty::Array(Box::new(t.clone())))
            }
            Op::ArrayJoin(t) => (f.callv(cx.array_fn("join", t), &[arg(0), arg(1)]), Ty::Str),
            Op::DictGet(t) => (f.dict_get(t, ptr(0), ptr(1)), t.clone()),
            Op::DictGetOpt(t) => {
                (f.callv(cx.dict_fn("getopt", t), &[arg(0), arg(1)]), Ty::opt(t.clone()))
            }
            Op::DictHas(_) => (f.dict_has(ptr(0), ptr(1)).into(), Ty::Bool),
            Op::DictKeys(t) => {
                (f.callv(cx.dict_fn("keys", t), &[arg(0)]), Ty::Array(Box::new(Ty::Str)))
            }
            Op::DictValues(t) => {
                (f.callv(cx.dict_fn("values", t), &[arg(0)]), Ty::Array(Box::new(t.clone())))
            }
            Op::DictMerge(t) => {
                (f.callv(cx.dict_fn("merge", t), &[arg(0), arg(1)]), Ty::Map(Box::new(t.clone())))
            }
            Op::Fail(msg) => {
                f.raise(msg);
                (self.nil(), Ty::Nil)
            }
            Op::ErrorMessage => (f.callv(cx.error_message_fn(false), &[arg(0)]), Ty::Str),
            Op::Await(t) => (f.callv(cx.await_fn(t), &[arg(0)]), t.clone()),
            Op::Join(_) | Op::JoinSettled(..) | Op::Wait => {
                let jobs = self.pointer_array(args);
                let n = f.i64(args.len() as i64).into();
                match op {
                    Op::Join(t) => {
                        (f.callv(cx.join_fn(t), &[jobs.into(), n]), Ty::Array(Box::new(t.clone())))
                    }
                    Op::JoinSettled(t, s) => (
                        f.callv(cx.join_settled_fn(t, s), &[jobs.into(), n]),
                        Ty::Array(Box::new(Ty::Struct(s.clone()))),
                    ),
                    _ => (f.callv(cx.wait_fn(), &[jobs.into(), n]), Ty::Int),
                }
            }
            Op::WaitArray(_) => {
                let a = ptr(0);
                let data = f.load_ptr(f.at_c(a, ARR_DATA));
                let n = f.load_i64(f.at_c(a, LEN));
                (f.callv(cx.wait_fn(), &[data.into(), n.into()]), Ty::Int)
            }
            Op::Ready => {
                let job = ptr(0);
                f.lock();
                let st = f.load_i64(f.at_c(job, tasks::STATE));
                let cancelled = f.ne(f.load_i64(f.at_c(job, tasks::CANCEL)), f.i64(0));
                let r = f.or(f.sge(st, f.i64(tasks::DONE)), cancelled);
                f.unlock();
                (r.into(), Ty::Bool)
            }
            Op::Cancel => {
                let job = ptr(0);
                f.lock();
                f.store(f.at_c(job, tasks::CANCEL), f.i64(1));
                f.broadcast();
                f.unlock();
                (self.nil(), Ty::Nil)
            }
            Op::Cancelled => (f.cancelled().into(), Ty::Bool),
            Op::MaxTasks => (f.max_tasks().into(), Ty::Int),
            Op::SetMaxTasks => (f.set_max_tasks(arg(0).into_int_value()).into(), Ty::Int),
            Op::After => (f.callv(cx.after_fn(), &[arg(0)]), Ty::Future(Box::new(Ty::Nil))),
            Op::Sleep => {
                f.call(cx.sleep_fn(), &[arg(0)]);
                (self.nil(), Ty::Nil)
            }
            Op::Now => (f.clock(false, 1).into(), Ty::Int),
            Op::NowMs => (f.clock(false, 1000).into(), Ty::Int),
            Op::Monotonic => (f.monotonic().into(), Ty::Float),
        }
    }
}
