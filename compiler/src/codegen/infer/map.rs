//! Mapping the model's memory: `map` opens each region by the name dovata's
//! record gave at build time, a part file by its path or a shared memory
//! object with `shm_open`, and maps it. No message goes to dovata: the names
//! are constants of the program.

use inkwell::values::FunctionValue;

use crate::codegen::Cx;

/// Where macOS's `struct stat` keeps a file's mode, its owner, its modification
/// time, seconds then nanoseconds, and its size. Inference compiles for Apple
/// Silicon only.
const STAT_MODE: i64 = 4;
const STAT_UID: i64 = 16;
const STAT_MTIME_SEC: i64 = 48;
const STAT_MTIME_NSEC: i64 = 56;
const STAT_SIZE: i64 = 96;

/// macOS's open flags: read-only, read-write, write-only, create, append and
/// close-on-exec.
const O_RDONLY: i64 = 0;
const O_RDWR: i64 = 2;
const O_WRONLY: i64 = 1;
const O_CREAT: i64 = 0x200;
const O_APPEND: i64 = 0x8;
const O_CLOEXEC: i64 = 0x0100_0000;

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function returns the body of `__infer.map()`, called with
    /// the lock held: it opens and maps every region of the model's memory, a
    /// region already mapped staying as it is, then registers the program with
    /// dovata, so `dovata status` lists it.
    pub(super) fn infer_map_fn(&self) -> FunctionValue<'ctx> {
        self.support("jade.infer.map.body", None, &[], |f| {
            let cx = f.cx;
            let (i64t, i8t, ptr) = (cx.i64t(), cx.i8t(), cx.ptrt());
            let plan = cx.plan();
            let model = &plan.model;
            // `struct stat` is 144 bytes on macOS; eight-byte words keep its fields aligned.
            let st = f.alloca(i64t.array_type(32));
            let page = f.sext(f.call_int(cx.objc_fn("getpagesize"), &[]), i64t);
            for (id, r) in plan.regions.iter().enumerate() {
                let base = f.elem(ptr, cx.infer_regions("base"), f.i64(id as i64));
                f.if_(f.is_null(f.load_ptr(base)), |f| {
                    let name = cx.cstr(&r.name);
                    let fd = if r.shared {
                        let flags = if r.writable { O_RDWR } else { O_RDONLY };
                        f.call_int(
                            cx.objc_fn("shm_open"),
                            &[name.into(), f.i32(flags).into(), f.i32(0).into()],
                        )
                    } else {
                        f.call_int(
                            cx.objc_fn("open"),
                            &[name.into(), f.i32(O_RDONLY | O_CLOEXEC).into(), f.i32(0).into()],
                        )
                    };
                    f.fail_if(
                        f.slt(fd, f.i32(0)),
                        &if r.shared {
                            format!(
                                "dovata does not hold the memory this program was built for \
                                 ({}): add the model {model} to dovata with the options the \
                                 program was built against, or build the program again",
                                r.name
                            )
                        } else {
                            format!("cannot open the model file {}", r.name)
                        },
                    );
                    let stat_ok = f.call_int(cx.objc_fn("fstat"), &[fd.into(), st.into()]);
                    let size = f.load_i64(f.at_c(st, STAT_SIZE));
                    let bad = if r.shared {
                        // Names are shared by every user of the machine, so another
                        // user could have made one before dovata did: the object must
                        // be this user's, and open to no one else.
                        let me = f.call_int(cx.objc_fn("geteuid"), &[]);
                        let owner = f.load_i32(f.at_c(st, STAT_UID));
                        let others = f.and(f.load_i32(f.at_c(st, STAT_MODE)), f.i32(0o077));
                        let theirs = f.or(f.ne(owner, me), f.ne(others, f.i32(0)));
                        let short = f.slt(size, f.i64(r.offset + r.bytes));
                        f.or(f.ne(stat_ok, f.i32(0)), f.or(short, theirs))
                    } else {
                        let sec = f.load_i64(f.at_c(st, STAT_MTIME_SEC));
                        let nsec = f.load_i64(f.at_c(st, STAT_MTIME_NSEC));
                        let same = f.and(
                            f.eq(size, f.i64(r.file_bytes)),
                            f.and(f.eq(sec, f.i64(r.modified_sec)), f.eq(nsec, f.i64(r.modified_nsec))),
                        );
                        f.or(f.ne(stat_ok, f.i32(0)), f.not(same))
                    };
                    f.if_(bad, |f| {
                        f.call(cx.libc("close"), &[fd.into()]);
                        f.fail(&if r.shared {
                            format!(
                                "dovata's memory for {model} ({}) is not the size this program \
                                 was built for, or does not belong to this user alone; add the \
                                 model to dovata again and build the program again",
                                r.name
                            )
                        } else {
                            format!(
                                "{} changed on disk after dovata loaded it; remove the model with \
                                 `dovata remove {model}`, add it again, and build the program again",
                                r.name
                            )
                        });
                    });
                    // A mapping starts on a page; the region's base is as far past
                    // that as its offset is past a page.
                    let offset = f.i64(r.offset);
                    let start = f.sub(offset, f.srem(offset, page));
                    let delta = f.sub(offset, start);
                    let len = f.add(delta, f.i64(r.bytes));
                    let len = f.mul(f.sdiv(f.add(len, f.sub(page, f.i64(1))), page), page);
                    let prot = if r.writable { 3 } else { 1 };
                    let p = f.call_ptr(
                        cx.objc_fn("mmap"),
                        &[
                            f.null().into(),
                            len.into(),
                            f.i32(prot).into(),
                            f.i32(1).into(),
                            fd.into(),
                            start.into(),
                        ],
                    );
                    // The mapping keeps the file or object; the descriptor is not needed.
                    f.call(cx.libc("close"), &[fd.into()]);
                    let failed =
                        f.eq(f.b.build_ptr_to_int(p, i64t, "").expect("positioned"), f.i64(-1));
                    f.fail_if(failed, &format!("could not map the model's memory ({})", r.name));
                    f.store(f.elem(ptr, cx.infer_regions("map"), f.i64(id as i64)), p);
                    f.store(f.elem(i64t, cx.infer_regions("maplen"), f.i64(id as i64)), len);
                    f.store(f.elem(i64t, cx.infer_regions("delta"), f.i64(id as i64)), delta);
                    f.store(base, f.at(p, delta));
                });
            }
            // An empty file named for this process, in the directory dovata
            // lists a model's programs from. A program that cannot make it
            // still runs.
            let path = f.alloca(i8t.array_type(1024));
            let pid = f.call_int(cx.libc("getpid"), &[]);
            let n = f.call_int(
                cx.libc("snprintf"),
                &[path.into(), f.i64(1024).into(), cx.cstr("%s/%d").into(), cx.cstr(&plan.clients).into(), pid.into()],
            );
            f.if_(f.and(f.sgt(n, f.i32(0)), f.slt(n, f.i32(1024))), |f| {
                let flags = O_WRONLY | O_CREAT | O_APPEND | O_CLOEXEC;
                let fd = f.call_int(
                    cx.objc_fn("open"),
                    &[path.into(), f.i32(flags).into(), f.i32(0o600).into()],
                );
                f.if_(f.sge(fd, f.i32(0)), |f| {
                    f.call(cx.libc("close"), &[fd.into()]);
                });
            });
            f.ret_void();
        })
    }
}
