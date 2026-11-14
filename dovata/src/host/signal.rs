//! Author: Joe Ricks
//! File: src/host/signal.rs
//! Description: The dovata process's signal handlers. SIGUSR1 says a request is waiting
//! in the run directory; SIGTERM and SIGINT say stop. Each handler writes one byte to a
//! pipe the process waits on, which is all a signal handler can safely do.

use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Once;
use std::time::Duration;

/// How many dovata processes one program can run at once. A real one runs one; the
/// tests run several on threads of one program.
const SLOTS: usize = 16;

/// The write end of each running process's pipe, or -1 for a free slot. The handlers
/// read it; nothing else in a handler is safe to touch.
static PIPES: [AtomicI32; SLOTS] = [const { AtomicI32::new(-1) }; SLOTS];

/// The byte SIGUSR1 writes.
const REQUEST: u8 = b'r';

/// The byte SIGTERM and SIGINT write.
const STOP: u8 = b's';

/// Purpose: This enum lists what waking up can mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wake {
    /// A stop signal came: SIGTERM or SIGINT.
    Stop,
    /// A request may be waiting: SIGUSR1 came, or the wait ran out.
    Look,
}

/// Purpose: This struct holds one running process's pipe, which its signal handlers
/// write to.
pub(crate) struct Wakeup {
    /// The read end, which [`Wakeup::wait`] reads.
    read: RawFd,
    /// The slot in [`PIPES`] that holds the write end.
    slot: usize,
}

/// Purpose: This function gives the calling thread's `errno`.
///
/// Inputs: none.
///
/// Outputs: a pointer to it.
fn errno() -> *mut libc::c_int
{
    #[cfg(target_vendor = "apple")]
    // SAFETY: __error returns the thread's own errno, valid for the thread's life.
    return unsafe { libc::__error() };
    #[cfg(not(target_vendor = "apple"))]
    // SAFETY: as above, under the name glibc and musl give it.
    unsafe {
        libc::__errno_location()
    }
}

/// Purpose: This function handles SIGUSR1, SIGTERM and SIGINT: it writes one byte to
/// every running process's pipe. With none running, a stop signal does what it would
/// have done without dovata, and SIGUSR1 is dropped.
///
/// Inputs:
/// - `sig`: the signal.
///
/// Outputs: none.
extern "C" fn on_signal(
    sig: libc::c_int,
)
{
    let byte = if sig == libc::SIGUSR1 { REQUEST } else { STOP };
    // SAFETY: errno() points at this thread's errno. A handler must leave it as it found
    // it, since the code it interrupted may be about to read it.
    let saved = unsafe { *errno() };
    let mut told = false;
    for slot in &PIPES {
        let fd = slot.load(Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: write is safe in a signal handler, the byte lives through the call,
            // and the pipe is never closed (see `Wakeup`'s drop). It does not block.
            unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
            told = true;
        }
    }
    if !told && byte == STOP {
        // SAFETY: signal and raise are both safe in a signal handler.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
    // SAFETY: as above.
    unsafe { *errno() = saved };
}

/// Purpose: This function installs the handlers, once per program.
///
/// Inputs: none.
///
/// Outputs: `Ok(())`, or the system's error.
fn install() -> io::Result<()>
{
    static ONCE: Once = Once::new();
    static FAILED: AtomicI32 = AtomicI32::new(0);
    ONCE.call_once(|| {
        for sig in [libc::SIGUSR1, libc::SIGTERM, libc::SIGINT] {
            // SAFETY: a zeroed sigaction is a valid empty one, and the handler is an
            // `extern "C" fn(c_int)`, the form sa_sigaction takes without SA_SIGINFO.
            // SA_RESTART keeps the signal from failing system calls in other threads.
            unsafe {
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                act.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut act.sa_mask);
                if libc::sigaction(sig, &act, std::ptr::null_mut()) != 0 {
                    FAILED.store(*errno(), Ordering::Relaxed);
                }
            }
        }
    });
    match FAILED.load(Ordering::Relaxed) {
        0 => Ok(()),
        e => Err(io::Error::from_raw_os_error(e)),
    }
}

impl Wakeup {
    /// Purpose: This function makes a pipe for one process and installs the handlers that
    /// write to it.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the [`Wakeup`], or the system's error.
    pub(crate) fn new() -> io::Result<Wakeup>
    {
        install()?;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` holds the two descriptors pipe writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            // SAFETY: `fd` is a descriptor this function just made. A handler's write must
            // never block, and a program dovata starts must not inherit the pipe.
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        for (slot, p) in PIPES.iter().enumerate() {
            if p.compare_exchange(-1, fds[1], Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                return Ok(Wakeup { read: fds[0], slot });
            }
        }
        // SAFETY: both descriptors are this function's, and no handler knows them.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        Err(io::Error::other(format!("one program runs at most {SLOTS} dovata processes")))
    }

    /// Purpose: This function waits for a signal, at most for a time.
    ///
    /// Inputs:
    /// - `timeout`: the longest to wait.
    ///
    /// Outputs: [`Wake::Stop`] when a stop signal came, else [`Wake::Look`].
    pub(crate) fn wait(
        &self,
        timeout: Duration,
    ) -> Wake
    {
        let mut pfd = libc::pollfd { fd: self.read, events: libc::POLLIN, revents: 0 };
        let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: `pfd` is one pollfd that lives through the call.
        unsafe { libc::poll(&mut pfd, 1, ms) };
        let mut buf = [0u8; 64];
        let mut wake = Wake::Look;
        loop {
            // SAFETY: `buf` lives through the call, and the read end does not block.
            let n = unsafe { libc::read(self.read, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            if buf[..n as usize].contains(&STOP) {
                wake = Wake::Stop;
            }
        }
        wake
    }
}

impl Drop for Wakeup {
    /// Purpose: This function frees the process's slot. The pipe itself stays open: a
    /// handler running on another thread may have read the write end just before the
    /// slot was freed, and closing it could make that handler write into whatever file
    /// next takes the same number.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn drop(&mut self)
    {
        PIPES[self.slot].store(-1, Ordering::Release);
    }
}

/// Purpose: This function tells a dovata process a request is waiting.
///
/// Inputs:
/// - `pid`: the process.
///
/// Outputs: `Ok(())`, or the system's error, such as no such process.
pub(crate) fn notify(
    pid: u32,
) -> io::Result<()>
{
    let pid = libc::pid_t::try_from(pid).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "not a process id"))?;
    // SAFETY: kill sends a signal and touches no memory of this program's.
    if unsafe { libc::kill(pid, libc::SIGUSR1) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
