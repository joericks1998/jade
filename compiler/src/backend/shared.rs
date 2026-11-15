//! What a program shares with the library objects it links: mutable state,
//! and the few support functions whose code depends on the program's classes.
//!
//! A build with the library cache (`libcache.rs`) compiles each cached std
//! library instance in a module of its own and links the objects with the
//! program's. Most support code, such as joining two strings, has no state
//! and may appear in every object as a private copy. Two things may not:
//!
//! - *Mutable state.* The thread's error slot, the hooks' error-code slot, the
//!   task queue and its lock, the random generator, the time-zone lock and
//!   `argc` and `argv` must each exist once, or a library object would raise
//!   into a slot the program never reads. Every such global is made through
//!   [`Cx::state`], so none can skip the rule: the program's module defines
//!   it, and a library module only declares it.
//! - *Code that reads the program's classes.* Raising builds a `RuntimeError`,
//!   whose type id is its place among the program's classes, and releasing an
//!   error frees whatever class it holds. Those functions, [`SharedFn`], are
//!   defined by the program and declared by library modules, through
//!   [`Cx::shared_fn`].
//!
//! In the program's module a shared name gets external linkage and hidden
//! visibility only when a linked library object refers to it, and internal
//! linkage otherwise, so a program built in one module, or one whose library
//! objects need none of it, compiles to exactly the IR it did before the
//! cache. A thread-local that crosses modules uses the initial-exec model on
//! both sides: valid in any executable, it needs no `__tls_get_addr`, and LLD
//! turns it into local-exec when the variable is defined in the executable,
//! as it always is here. On macOS every thread-local goes through Mach-O's
//! thread-local descriptors whatever the model says.
//!
//! A library module records each shared name it declares in `Cx::refs`, with
//! enough to define it, and the build gives the union of those records to the
//! program's module as its exports. It records them whether its object came
//! from the cache or was just compiled, since the build makes every library
//! module's IR either way.

use std::collections::BTreeMap;

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FunctionValue, GlobalValue};
use inkwell::{GlobalVisibility, ThreadLocalMode};

use super::{Cx, F};

/// Purpose: This enum says which kind of module a [`Cx`] builds.
#[derive(Debug, Clone, Default)]
pub enum Role {
    /// The program's module. It defines every piece of shared state it or a
    /// linked library object uses. `exports` names what the library objects
    /// refer to, which gets external linkage and hidden visibility.
    Program { exports: BTreeMap<String, Shared> },
    /// One cached library instance's module. It defines no shared state and
    /// no [`SharedFn`]; it declares what it uses, for the program to define.
    Library,
    /// A program built in one module, with nothing exported: the IR a build
    /// without the cache makes, and what `--emit ir` shows.
    #[default]
    Whole,
}

/// Purpose: This enum describes a piece of shared state's LLVM type and
/// initial value, without an LLVM context, so a record made in a library
/// module's context can define the global in the program's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// An `i64` holding this value at start.
    Word(i64),
    /// A pointer, null at start.
    Ptr,
    /// `n` zeroed bytes, for a C mutex or condition variable.
    Bytes(u32),
    /// `n` zeroed `i64`s.
    Words(u32),
    /// `n` null pointers.
    Ptrs(u32),
}

impl Slot {
    /// Purpose: This function returns the slot's LLVM type.
    fn ty<'ctx>(self, cx: &Cx<'ctx>) -> BasicTypeEnum<'ctx> {
        match self {
            Slot::Word(_) => cx.i64t().into(),
            Slot::Ptr => cx.ptrt().into(),
            Slot::Bytes(n) => cx.i8t().array_type(n).into(),
            Slot::Words(n) => cx.i64t().array_type(n).into(),
            Slot::Ptrs(n) => cx.ptrt().array_type(n).into(),
        }
    }

    /// Purpose: This function returns the slot's initial value.
    fn init<'ctx>(self, cx: &Cx<'ctx>) -> BasicValueEnum<'ctx> {
        match self {
            Slot::Word(v) => cx.i64t().const_int(v as u64, true).into(),
            other => super::layout::zero(other.ty(cx)),
        }
    }
}

/// Purpose: This struct describes one global holding mutable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub slot: Slot,
    /// One per thread, such as the error slot.
    pub tls: bool,
    /// The alignment it needs, when more than its type's.
    pub align: Option<u32>,
}

impl State {
    /// Purpose: This function describes process-wide state.
    pub fn global(slot: Slot) -> Self {
        State { slot, tls: false, align: None }
    }

    /// Purpose: This function describes state each thread has its own of.
    pub fn thread(slot: Slot) -> Self {
        State { slot, tls: true, align: None }
    }

    /// Purpose: This function describes process-wide state aligned to 16
    /// bytes, as the task queue and the model's state are.
    pub fn aligned(slot: Slot) -> Self {
        State { slot, tls: false, align: Some(16) }
    }
}

/// Purpose: This enum lists the support functions whose code reads the
/// program's classes, so only the program's module can define them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SharedFn {
    /// `jade.set_err(e)`: raise, releasing the error already raised.
    SetErr,
    /// `jade.raise_msg(msg)`: raise a `RuntimeError` built from a message.
    RaiseMsg,
    /// `jade.release.struct(p)`: release a class value or an error.
    ReleaseStruct,
    /// `jade.new_error(msg) -> ptr`: a `RuntimeError` holding the message,
    /// which it takes the reference of. Library code's `raise "text"` calls
    /// it, where the program's own builds the object inline.
    NewError,
}

impl SharedFn {
    /// Every shared function, for tests.
    pub const ALL: [SharedFn; 4] =
        [SharedFn::SetErr, SharedFn::RaiseMsg, SharedFn::ReleaseStruct, SharedFn::NewError];

    /// Purpose: This function returns the function's symbol.
    pub fn name(self) -> &'static str {
        match self {
            SharedFn::SetErr => "jade.set_err",
            SharedFn::RaiseMsg => "jade.raise_msg",
            SharedFn::ReleaseStruct => "jade.release.struct",
            SharedFn::NewError => "jade.new_error",
        }
    }
}

/// Purpose: This enum holds one name a library module declares for the
/// program to define.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shared {
    State(State),
    Func(SharedFn),
}

impl<'ctx> Cx<'ctx> {
    /// Purpose: This function says whether this module is a library
    /// instance's.
    pub fn is_library(&self) -> bool {
        matches!(self.role, Role::Library)
    }

    /// Purpose: This function says whether a linked library object refers
    /// to a shared name, so the program's module must export it.
    pub fn exported(&self, name: &str) -> bool {
        matches!(&self.role, Role::Program { exports } if exports.contains_key(name))
    }

    /// Purpose: This function records that a library module asked for
    /// something that depends on the program, such as a class's layout. The
    /// build then compiles that instance with the program instead.
    pub fn mark_program_dependent(&self) {
        if self.is_library() {
            self.dependent.set(true);
        }
    }

    /// Purpose: This function returns a global holding mutable state, made
    /// the first time: defined in the program's module, declared in a
    /// library module.
    ///
    /// Inputs:
    /// - `name`: the global's symbol.
    /// - `st`: its type, initial value, alignment and whether each thread
    ///   has its own.
    ///
    /// Outputs: the global.
    pub fn state(&self, name: &str, st: State) -> GlobalValue<'ctx> {
        if let Some(g) = self.module.get_global(name) {
            return g;
        }
        let g = self.module.add_global(st.slot.ty(self), None, name);
        if !self.is_library() {
            g.set_initializer(&st.slot.init(self));
        }
        if st.tls {
            g.set_thread_local(true);
        }
        if self.is_library() || self.exported(name) {
            g.set_linkage(Linkage::External);
            g.set_visibility(GlobalVisibility::Hidden);
            if st.tls {
                g.set_thread_local_mode(Some(ThreadLocalMode::InitialExecTLSModel));
            }
        } else {
            g.set_linkage(Linkage::Internal);
        }
        if let Some(a) = st.align {
            g.set_alignment(a);
        }
        if self.is_library() {
            self.refs.borrow_mut().insert(name.to_string(), Shared::State(st));
        }
        g
    }

    /// Purpose: This function returns a support function that reads the
    /// program's classes: built in the program's module, declared in a
    /// library module.
    ///
    /// Inputs:
    /// - `which`: the function.
    /// - `ret`, `params`: its signature.
    /// - `build`: builds its body, in the program's module only.
    ///
    /// Outputs: the function.
    pub fn shared_fn(
        &self,
        which: SharedFn,
        ret: Option<BasicTypeEnum<'ctx>>,
        params: &[BasicTypeEnum<'ctx>],
        build: impl FnOnce(&F<'_, 'ctx>),
    ) -> FunctionValue<'ctx> {
        let name = which.name();
        if let Some(f) = self.support.borrow().get(name) {
            return *f;
        }
        if self.is_library() {
            let ps: Vec<BasicMetadataTypeEnum> = params.iter().map(|p| (*p).into()).collect();
            let fn_ty = match ret {
                Some(r) => r.fn_type(&ps, false),
                None => self.ctx.void_type().fn_type(&ps, false),
            };
            let f = self.module.add_function(name, fn_ty, Some(Linkage::External));
            f.as_global_value().set_visibility(GlobalVisibility::Hidden);
            self.support.borrow_mut().insert(name.to_string(), f);
            self.refs.borrow_mut().insert(name.to_string(), Shared::Func(which));
            return f;
        }
        let exported = self.exported(name);
        let linkage = if exported { Linkage::External } else { Linkage::Internal };
        let f = self.define_fn(name, ret, params, linkage, build);
        if exported {
            f.as_global_value().set_visibility(GlobalVisibility::Hidden);
        }
        f
    }

    /// Purpose: This function defines, in the program's module, everything
    /// the linked library objects refer to that the program's own code did
    /// not already make: `env.args()` in a cached object still needs the
    /// globals the C `main` stores the arguments in.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    pub fn define_exports(&self) {
        let exports = match &self.role {
            Role::Program { exports } => exports.clone(),
            Role::Library | Role::Whole => return,
        };
        for (name, s) in exports {
            match s {
                Shared::State(st) => {
                    self.state(&name, st);
                }
                Shared::Func(SharedFn::SetErr) => {
                    self.set_err_fn();
                }
                Shared::Func(SharedFn::RaiseMsg) => {
                    self.raise_msg_fn();
                }
                Shared::Func(SharedFn::ReleaseStruct) => {
                    self.release_fn(&crate::mir::Ty::Error);
                }
                Shared::Func(SharedFn::NewError) => {
                    self.new_error_fn();
                }
            }
        }
    }

    /// Purpose: This function takes the shared names this library module
    /// declared.
    pub fn take_refs(&self) -> BTreeMap<String, Shared> {
        std::mem::take(&mut *self.refs.borrow_mut())
    }
}
