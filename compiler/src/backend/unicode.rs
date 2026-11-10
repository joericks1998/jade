//! Unicode case mapping for `upper`, `lower` and `capitalize`.
//!
//! v1 mapped case with Rust's `char::to_uppercase` and `to_lowercase`, which
//! follow the full Unicode tables, including mappings that change length: `ß`
//! uppercases to `SS`. A compiled program has no Rust standard library, so the
//! compiler builds the same tables from Rust's own when it compiles a program
//! that uses one of those methods, and emits them as constant data. A program
//! that never changes case carries no table.
//!
//! Each table is sorted by code point, one entry per scalar whose mapping is
//! not itself: `{ i32 cp, i32 count, i32 m0, i32 m1, i32 m2 }`. Lookup is a
//! binary search, with ASCII answered before it.

use std::sync::OnceLock;

use inkwell::module::Linkage;
use inkwell::values::FunctionValue;

use super::Cx;

/// Purpose: This function builds one case table from Rust's.
///
/// Inputs:
/// - `upper`: true for the uppercase table.
///
/// Outputs: every scalar that maps to something else, with its mapping.
fn table(upper: bool) -> Vec<[u32; 5]> {
    let mut out = Vec::new();
    for cp in 0..=0x10FFFFu32 {
        let Some(c) = char::from_u32(cp) else { continue };
        let mapped: Vec<char> =
            if upper { c.to_uppercase().collect() } else { c.to_lowercase().collect() };
        if mapped.len() == 1 && mapped[0] == c {
            continue;
        }
        let mut e = [cp, mapped.len().min(3) as u32, 0, 0, 0];
        for (i, m) in mapped.iter().take(3).enumerate() {
            e[2 + i] = *m as u32;
        }
        out.push(e);
    }
    out
}

/// Purpose: This function returns a case table, building it once per process.
fn cached(upper: bool) -> &'static [[u32; 5]] {
    static UPPER: OnceLock<Vec<[u32; 5]>> = OnceLock::new();
    static LOWER: OnceLock<Vec<[u32; 5]>> = OnceLock::new();
    if upper { UPPER.get_or_init(|| table(true)) } else { LOWER.get_or_init(|| table(false)) }
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns `jade.case.<upper|lower>(cp, out) -> i64`:
    /// the mapping of one scalar written to `out` (room for three), and its
    /// length.
    ///
    /// Inputs:
    /// - `upper`: true to map to uppercase.
    ///
    /// Outputs: the function.
    pub fn case_fn(&self, upper: bool) -> FunctionValue<'ctx> {
        let name = if upper { "jade.case.upper" } else { "jade.case.lower" };
        let ptr = self.ptrt();
        self.support(name, Some(self.i64t().into()), &[self.i32t().into(), ptr.into()], |f| {
            let (cp, out) = (f.param_int(0), f.param_ptr(1));
            let i32t = f.cx.i32t();
            let entries = cached(upper);
            let row_ty = i32t.array_type(5);
            let rows: Vec<_> = entries
                .iter()
                .map(|e| {
                    let vals: Vec<_> = e.iter().map(|v| i32t.const_int(*v as u64, false)).collect();
                    i32t.const_array(&vals)
                })
                .collect();
            let tbl_ty = row_ty.array_type(rows.len() as u32);
            let g = f.cx.module.add_global(tbl_ty, None, &format!("{name}.table"));
            g.set_initializer(&row_ty.const_array(&rows));
            g.set_constant(true);
            g.set_linkage(Linkage::Private);
            let tbl = g.as_pointer_value();

            // ASCII needs no table.
            let ult = |a, b| f.icmp(inkwell::IntPredicate::ULT, a, b);
            f.if_(ult(cp, f.i32(128)), |f| {
                let (lo, hi, delta) = if upper { (97, 122, -32) } else { (65, 90, 32) };
                let inside = f.and(
                    f.icmp(inkwell::IntPredicate::UGE, cp, f.i32(lo)),
                    f.icmp(inkwell::IntPredicate::ULE, cp, f.i32(hi)),
                );
                let m = f.select_int(inside, f.add(cp, f.i32(delta)), cp);
                f.store(out, m);
                f.ret(f.i64(1));
            });
            let lo = f.var(f.i64(0));
            let hi = f.var(f.i64(entries.len() as i64));
            f.while_(
                |f| f.slt(f.load_i64(lo), f.load_i64(hi)),
                |f, _| {
                    let (l, h) = (f.load_i64(lo), f.load_i64(hi));
                    let mid = f.sdiv(f.add(l, h), f.i64(2));
                    let row = f.elem(row_ty, tbl, mid);
                    let key = f.load_i32(row);
                    f.if_(f.eq(key, cp), |f| {
                        let n = f.load_i32(f.at_c(row, 4));
                        for k in 0..3 {
                            let m = f.load_i32(f.at_c(row, 8 + 4 * k));
                            f.store(f.at_c(out, 4 * k), m);
                        }
                        f.ret(f.zext(n, f.cx.i64t()));
                    });
                    f.if_else(
                        ult(key, cp),
                        |f| f.store(lo, f.add(mid, f.i64(1))),
                        |f| f.store(hi, mid),
                    );
                },
            );
            f.store(out, cp);
            f.ret(f.i64(1));
        })
    }
}
