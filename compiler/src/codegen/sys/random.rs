//! The generator behind `std::random`, which is written in Jade,
//! `src/lib/random.jde`: one xoshiro256** generator for the whole program.
//!
//! It is v1's generator, number for number: the state is seeded through
//! splitmix64, from the clock and the process id until `random.seed(n)` fixes
//! it, so a seeded program draws the same values it drew under v1. A spin lock
//! guards the state, since tasks may draw at the same time, which is why it
//! stays here: library code cannot hold state that tasks share. Two hooks
//! reach it, `rand_below`, which draws below a bound without modulo bias, and
//! `rand_seed`; ranges, `choice` and `shuffle` are Jade, built on draws.

use inkwell::values::{FunctionValue, IntValue, PointerValue};

use crate::codegen::libcache::shared::{Slot, State};
use crate::codegen::{Cx, F};

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the generator's state: four words, a
    /// seeded flag and a lock word.
    fn rng_state(&self) -> PointerValue<'ctx> {
        self.state("jade.rng", State::global(Slot::Words(6))).as_pointer_value()
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

    /// Purpose: This function returns `jade.sys.rand_below(n) -> int`: one
    /// draw below `n`, or all 64 bits for 0, with the lock held around the
    /// whole draw, retries included.
    pub fn sys_rand_below_fn(&self) -> FunctionValue<'ctx> {
        let i = self.i64t();
        self.support("jade.sys.rand_below", Some(i.into()), &[i.into()], |f| {
            f.rng_lock();
            let r = f.call_int(f.cx.rng_bounded_fn(), &[f.param(0)]);
            f.rng_unlock();
            f.ret(r);
        })
    }

    /// Purpose: This function returns `jade.sys.rand_seed(n)`, which fixes
    /// the sequence.
    pub fn sys_rand_seed_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.sys.rand_seed", None, &[self.i64t().into()], |f| {
            f.rng_lock();
            f.call(f.cx.rng_reseed_fn(), &[f.param(0)]);
            f.rng_unlock();
            f.ret_void();
        })
    }
}

impl<'a, 'ctx> F<'a, 'ctx> {
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
