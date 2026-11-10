//! `std::random`: one xoshiro256** generator for the whole program.
//!
//! It is v1's generator, number for number: the state is seeded through
//! splitmix64, from the clock and the process id until `random.seed(n)` fixes
//! it, so a seeded program draws the same values it drew under v1. A spin lock
//! guards the state, since tasks may draw at the same time.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use super::layout::{ARR_DATA, LEN};
use super::{Cx, F};
use crate::mir::RandomFn;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the generator's state: four words, a
    /// seeded flag and a lock word.
    fn rng_state(&self) -> PointerValue<'ctx> {
        if let Some(g) = self.module.get_global("jade.rng") {
            return g.as_pointer_value();
        }
        let t = self.i64t().array_type(6);
        let g = self.module.add_global(t, None, "jade.rng");
        g.set_initializer(&t.const_zero());
        g.set_linkage(inkwell::module::Linkage::Internal);
        g.as_pointer_value()
    }

    /// Purpose: This function returns `jade.rng.reseed(seed)`, which must be
    /// called with the lock held.
    fn rng_reseed_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.rng.reseed", None, &[self.i64t().into()], |f| {
            let st = f.cx.rng_state();
            let x = f.var(f.param_int(0));
            let mut any = f.i64(0);
            for k in 0..4 {
                let v = f.splitmix(x);
                f.store(f.at_c(st, k * 8), v);
                any = f.or(any, v);
            }
            f.if_(f.eq(any, f.i64(0)), |f| f.store(st, f.i64(0x9E37_79B9_7F4A_7C15u64 as i64)));
            f.store(f.at_c(st, 32), f.i64(1));
            f.ret_void();
        })
    }

    /// Purpose: This function returns `jade.rng.next() -> int`: the next 64
    /// bits, which must be called with the lock held.
    fn rng_next_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.rng.next", Some(self.i64t().into()), &[], |f| {
            let st = f.cx.rng_state();
            let seeded = f.ne(f.load_i64(f.at_c(st, 32)), f.i64(0));
            f.if_(f.not(seeded), |f| {
                let now = f.clock(false, 1);
                let pid = f.sext(f.call_int(f.cx.libc("getpid"), &[]), f.cx.i64t());
                let seed = f.xor(now, f.shl(pid, f.i64(16)));
                f.call(f.cx.rng_reseed_fn(), &[seed.into()]);
            });
            let s = |k: i64| f.at_c(st, k * 8);
            let (s0, s1, s2, s3) =
                (f.load_i64(s(0)), f.load_i64(s(1)), f.load_i64(s(2)), f.load_i64(s(3)));
            let result = f.mul(f.rotl(f.mul(s1, f.i64(5)), 7), f.i64(9));
            let t = f.shl(s1, f.i64(17));
            let s2 = f.xor(s2, s0);
            let s3 = f.xor(s3, s1);
            let s1 = f.xor(s1, s2);
            let s0 = f.xor(s0, s3);
            let s2 = f.xor(s2, t);
            let s3 = f.rotl(s3, 45);
            for (k, v) in [(0, s0), (1, s1), (2, s2), (3, s3)] {
                f.store(s(k), v);
            }
            f.ret(result);
        })
    }

    /// Purpose: This function returns `jade.rng.bounded(n) -> int`: a value in
    /// `[0, n)` as an unsigned number without modulo bias, or any 64 bits for
    /// `n = 0`. The lock must be held.
    fn rng_bounded_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        self.support("jade.rng.bounded", Some(i.into()), &[i.into()], |f| {
            let n = f.param_int(0);
            f.if_(f.eq(n, f.i64(0)), |f| f.ret(f.call_int(f.cx.rng_next_fn(), &[])));
            let max = f.i64(-1);
            let limit = f.sub(max, f.urem(max, n));
            f.while_(
                |f| f.bool(true),
                |f, _| {
                    let r = f.call_int(f.cx.rng_next_fn(), &[]);
                    f.if_(f.ult(r, limit), |f| f.ret(f.urem(r, n)));
                },
            );
            f.ret(f.i64(0));
        })
    }

    /// Purpose: This function returns the support function behind a
    /// `std::random` operation.
    pub fn random_fn(&self, m: RandomFn) -> FunctionValue<'ctx> {
        let i = self.i64t();
        let name = format!("jade.random.{m:?}").to_lowercase();
        match m {
            RandomFn::Int => self.support(&name, Some(i.into()), &[i.into(), i.into()], |f| {
                let (lo, hi) = (f.param_int(0), f.param_int(1));
                f.if_(f.sgt(lo, hi), |f| {
                    let m = f.format("random.int: min (%lld) > max (%lld)", &[lo, hi]);
                    f.fail_str(m);
                });
                f.ret(f.draw(lo, hi));
            }),
            RandomFn::Index => self.support(&name, Some(i.into()), &[i.into()], |f| {
                let n = f.param_int(0);
                f.fail_if(f.sle(n, f.i64(0)), "random.choice: the array is empty");
                f.ret(f.draw(f.i64(0), f.sub(n, f.i64(1))));
            }),
            RandomFn::Float => self.support(&name, Some(self.f64t().into()), &[], |f| {
                f.rng_lock();
                let r = f.call_int(f.cx.rng_bounded_fn(), &[f.i64(0).into()]);
                f.rng_unlock();
                let bits = f.lshr(r, f.i64(11));
                let x = f.b.build_unsigned_int_to_float(bits, f.cx.f64t(), "").expect("positioned");
                f.ret(f.fdiv(x, f.f64((1u64 << 53) as f64)));
            }),
            RandomFn::Seed => self.support(&name, None, &[i.into()], |f| {
                f.rng_lock();
                f.call(f.cx.rng_reseed_fn(), &[f.param(0)]);
                f.rng_unlock();
                f.ret_void();
            }),
            RandomFn::Shuffle => {
                // Elements move without changing their counts, so one function
                // serves every element type, given the element size.
                self.support("jade.random.shuffle", None, &[self.ptrt().into(), i.into()], |f| {
                    let (a, size) = (f.param_ptr(0), f.param_int(1));
                    let n = f.load_i64(f.at_c(a, LEN));
                    let data = f.load_ptr(f.at_c(a, ARR_DATA));
                    let tmp = f.alloca(f.cx.i8t().array_type(64));
                    let k = f.var(f.sub(n, f.i64(1)));
                    f.while_(
                        |f| f.sgt(f.load_i64(k), f.i64(0)),
                        |f, _| {
                            let i = f.load_i64(k);
                            let j = f.draw(f.i64(0), i);
                            let (pi, pj) = (f.at(data, f.mul(i, size)), f.at(data, f.mul(j, size)));
                            f.memcpy(tmp, pi, size);
                            f.memmove(pi, pj, size);
                            f.memcpy(pj, tmp, size);
                            f.store(k, f.sub(i, f.i64(1)));
                        },
                    );
                    f.ret_void();
                })
            }
        }
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
    /// Purpose: This function draws an int in `[lo, hi]`, as v1's `draw` did.
    fn draw(&self, lo: IntValue<'ctx>, hi: IntValue<'ctx>) -> IntValue<'ctx> {
        let range = self.add(self.sub(hi, lo), self.i64(1));
        self.rng_lock();
        let r = self.call_int(self.cx.rng_bounded_fn(), &[range.into()]);
        self.rng_unlock();
        self.add(lo, r)
    }

    /// Purpose: This function advances a splitmix64 state and returns its output.
    fn splitmix(&self, x: PointerValue<'ctx>) -> IntValue<'ctx> {
        let v = self.add(self.load_i64(x), self.i64(0x9E37_79B9_7F4A_7C15u64 as i64));
        self.store(x, v);
        let z = self.mul(
            self.xor(v, self.lshr(v, self.i64(30))),
            self.i64(0xBF58_476D_1CE4_E5B9u64 as i64),
        );
        let z = self.mul(
            self.xor(z, self.lshr(z, self.i64(27))),
            self.i64(0x94D0_49BB_1331_11EBu64 as i64),
        );
        self.xor(z, self.lshr(z, self.i64(31)))
    }

    /// Purpose: This function rotates 64 bits left.
    fn rotl(&self, x: IntValue<'ctx>, k: i64) -> IntValue<'ctx> {
        self.or(self.shl(x, self.i64(k)), self.lshr(x, self.i64(64 - k)))
    }

    /// Purpose: This function takes the generator's spin lock.
    fn rng_lock(&self) {
        self.spin_lock(self.at_c(self.cx.rng_state(), 40));
    }

    /// Purpose: This function gives the generator's lock back.
    fn rng_unlock(&self) {
        self.spin_unlock(self.at_c(self.cx.rng_state(), 40));
    }
}
