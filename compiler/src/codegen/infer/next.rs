//! Generation: `next` gives one token of a reply per call. It runs the tokens
//! the model has not seen yet through as many steps as they take, then picks
//! the likeliest next token, greedily, with `argmax`.

use inkwell::values::FunctionValue;

use crate::codegen::Cx;
use crate::codegen::libcache::shared::Slot;
use crate::codegen::memory::layout::{ARR_DATA, LEN};

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.infer.argmax() -> int`: the token
    /// the last step's logits favor.
    fn infer_argmax_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.argmax", Some(self.i64t().into()), &[], |f| {
            let cx = f.cx;
            let p = cx.plan().clone();
            let f32t = cx.ctx.f32_type();
            let logits = cx.host_addr(f, p.logits);
            let best = f.var(f32t.const_float(f64::NEG_INFINITY));
            let at = f.var(f.i64(0));
            f.for_range(f.i64(0), f.i64(p.n_vocab), |f, i, _| {
                let x = f.load(f32t, f.elem(f32t, logits, i)).into_float_value();
                let b = f.load(f32t, best).into_float_value();
                f.if_(f.fcmp(inkwell::FloatPredicate::OGT, x, b), |f| {
                    f.store(best, x);
                    f.store(at, i);
                });
            });
            f.ret(f.load_i64(at));
        })
    }

    /// Purpose: This function returns the body of `__infer.next(ids,
    /// conversation, prompt_len)`, called with the lock held: the token after
    /// `ids`, greedily, or -1 at a stop token or when the context is full.
    ///
    /// The model's memory holds one conversation at a time, the first
    /// `jade.infer.kv_len` tokens of `jade.infer.kv_owner`'s. When `ids`
    /// belongs to that conversation, only the tokens after those go through,
    /// usually the one token the last call gave. When another prompt ran in
    /// between, all of `ids` goes through again from the start, as many tokens
    /// a step as the plan allows.
    pub(super) fn infer_next_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t) = (self.ptrt(), self.i64t());
        let params = [ptr.into(), i64t.into(), i64t.into()];
        self.support("jade.infer.next.body", Some(i64t.into()), &params, |f| {
            let cx = f.cx;
            let p = cx.plan().clone();
            let (ids, conv, prompt_len) = (f.param_ptr(0), f.param_int(1), f.param_int(2));
            let owner = cx.infer_global("jade.infer.kv_owner", Slot::Word(0));
            let held = cx.infer_global("jade.infer.kv_len", Slot::Word(0));
            let n = f.load_i64(f.at_c(ids, LEN));
            f.if_(f.eq(n, f.i64(0)), |f| f.ret(f.i64(-1)));
            f.if_(f.eq(f.load_i64(cx.infer_ready()), f.i64(0)), |f| {
                f.fail("the model was not set up before a prompt ran");
            });
            f.if_(f.sge(n, f.i64(p.n_ctx)), |f| {
                f.if_(f.sle(n, prompt_len), |f| {
                    f.fail(
                        "the prompt is longer than the model's context; dovata plans the \
                         context's length",
                    );
                });
                f.ret(f.i64(-1));
            });
            // Where this conversation's tokens stop matching what the model
            // holds. The memory is marked as holding nothing until the rest are
            // in, so a raise part way leaves no conversation half read.
            let same = f.eq(f.load_i64(owner), conv);
            let kept = f.load_i64(held);
            let from = f.select_int(f.and(same, f.slt(kept, n)), kept, f.i64(0));
            f.store(owner, f.i64(0));
            let data = f.load_ptr(f.at_c(ids, ARR_DATA));
            let pos = f.var(from);
            f.while_(
                |f| f.slt(f.load_i64(pos), n),
                |f, _| {
                    let at = f.load_i64(pos);
                    let left = f.sub(n, at);
                    let cnt = f.select_int(f.sgt(left, f.i64(p.n_tokens)), f.i64(p.n_tokens), left);
                    let first = f.elem(i64t, data, at);
                    f.call(cx.infer_step_fn(), &[first.into(), cnt.into(), at.into()]);
                    f.if_(f.err_pending(), |f| f.ret(f.i64(0)));
                    f.store(pos, f.add(at, cnt));
                },
            );
            f.store(owner, conv);
            f.store(held, n);
            let next = f.call_int(cx.infer_argmax_fn(), &[]);
            let mut stop = f.bool(false);
            for s in &p.stops {
                stop = f.or(stop, f.eq(next, f.i64(*s)));
            }
            f.ret(f.select_int(stop, f.i64(-1), next));
        })
    }
}
