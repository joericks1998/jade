//! The tokenizer's tables, written into the program as constants: its pieces,
//! merges and added tokens, and the Unicode classes its splitter asks about.
//! `taint` is here too: it marks a copy of text as coming from outside the
//! program, as the tokenizer's output is.

use inkwell::module::Linkage;
use inkwell::values::{FunctionValue, PointerValue};

use crate::codegen::Cx;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns an immortal string object for a large
    /// constant, built in one piece rather than byte by byte.
    fn big_lit(&self, name: &str, s: &str) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g.as_pointer_value();
        }
        let bytes = self.ctx.const_string(s.as_bytes(), true);
        let ty = self.ctx.struct_type(
            &[self.i64t().into(), self.i64t().into(), self.i64t().into(), bytes.get_type().into()],
            false,
        );
        let init = ty.const_named_struct(&[
            self.i64t().const_int(u64::MAX, true).into(),
            self.i64t().const_int(s.len() as u64, false).into(),
            self.i64t().const_zero().into(),
            bytes.into(),
        ]);
        let g = self.module.add_global(ty, None, name);
        g.set_initializer(&init);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.set_alignment(8);
        g.as_pointer_value()
    }

    /// Purpose: This function returns `jade.infer.data(k) -> str`: the
    /// tokenizer's pieces, merges or added tokens.
    pub(super) fn infer_data_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.data", Some(self.ptrt().into()), &[self.i64t().into()], |f| {
            let k = f.param_int(0);
            let p = f.cx.plan();
            let tables = [
                f.cx.big_lit("jade.infer.pieces", &p.pieces),
                f.cx.big_lit("jade.infer.merges", &p.merges),
                f.cx.big_lit("jade.infer.added", &p.added),
            ];
            for (i, t) in tables.iter().enumerate() {
                f.if_(f.eq(k, f.i64(i as i64)), |f| f.ret(*t));
            }
            f.ret(f.cx.lit(""));
        })
    }

    /// Purpose: This function returns a constant table of inclusive ranges.
    fn range_table(&self, name: &str, ranges: &[(u32, u32)]) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g.as_pointer_value();
        }
        let vals: Vec<_> = ranges
            .iter()
            .flat_map(|(a, b)| [*a, *b])
            .map(|x| self.i32t().const_int(x as u64, false))
            .collect();
        let arr = self.i32t().const_array(&vals);
        let g = self.module.add_global(arr.get_type(), None, name);
        g.set_initializer(&arr);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.as_pointer_value()
    }

    /// Purpose: This function returns `jade.infer.in_ranges(table, n, c) ->
    /// bool`: a binary search of a range table.
    fn in_ranges_fn(&self) -> FunctionValue<'ctx> {
        let (ptr, i64t, i32t) = (self.ptrt(), self.i64t(), self.i32t());
        self.support(
            "jade.infer.in_ranges",
            Some(self.i1t().into()),
            &[ptr.into(), i64t.into(), i32t.into()],
            |f| {
                let (t, n) = (f.param_ptr(0), f.param_int(1));
                let c = f.param_int(2);
                let lo = f.var(f.i64(0));
                let hi = f.var(n);
                f.while_(
                    |f| f.slt(f.load_i64(lo), f.load_i64(hi)),
                    |f, _| {
                        let (l, h) = (f.load_i64(lo), f.load_i64(hi));
                        let mid = f.sdiv(f.add(l, h), f.i64(2));
                        let a = f.load_i32(f.elem(i32t, t, f.mul(mid, f.i64(2))));
                        let b = f.load_i32(f.elem(i32t, t, f.add(f.mul(mid, f.i64(2)), f.i64(1))));
                        f.if_else(
                            f.ult(c, a),
                            |f| f.store(hi, mid),
                            |f| {
                                f.if_else(
                                    f.icmp(inkwell::IntPredicate::UGT, c, b),
                                    |f| f.store(lo, f.add(mid, f.i64(1))),
                                    |f| f.ret(f.bool(true)),
                                );
                            },
                        );
                    },
                );
                f.ret(f.bool(false));
            },
        )
    }

    /// Purpose: This function returns `jade.infer.char_class(c) -> int`: 1 for a
    /// letter, 2 a number, 3 white space, else 0.
    pub(super) fn infer_char_class_fn(&self) -> FunctionValue<'ctx> {
        use crate::infer::unicode::{LETTERS, NUMBERS, SPACES};
        self.support(
            "jade.infer.char_class",
            Some(self.i64t().into()),
            &[self.i32t().into()],
            |f| {
                // A tainted char carries its taint in bit 31.
                let c = f.and(f.param_int(0), f.cx.i32t().const_int(0x1F_FFFF, false));
                let tables = [
                    (1, f.cx.range_table("jade.infer.letters", LETTERS), LETTERS.len()),
                    (2, f.cx.range_table("jade.infer.numbers", NUMBERS), NUMBERS.len()),
                    (3, f.cx.range_table("jade.infer.spaces", SPACES), SPACES.len()),
                ];
                for (class, t, n) in tables {
                    let hit = f.call_int(
                        f.cx.in_ranges_fn(),
                        &[t.into(), f.i64(n as i64).into(), c.into()],
                    );
                    f.if_(hit, |f| f.ret(f.i64(class)));
                }
                f.ret(f.i64(0));
            },
        )
    }

    /// Purpose: This function returns `jade.infer.taint(s) -> str`: a copy of
    /// the text marked as coming from outside the program.
    pub(super) fn infer_taint_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.taint", Some(self.ptrt().into()), &[self.ptrt().into()], |f| {
            let s = f.param_ptr(0);
            let len = f.str_len(s);
            let t = f.str_new(len);
            f.memcpy(f.str_data(t), f.str_data(s), len);
            f.set_trust(t, f.i64(1));
            f.ret(t);
        })
    }
}
