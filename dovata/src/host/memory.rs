//! Author: Joe Ricks
//! File: src/host/memory.rs
//! Description: Maps files and shared memory, reads a mapping into physical memory, and
//! reports how much of it is there. The host's unsafe memory calls are all in this file.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;

/// Purpose: This struct holds one memory mapping, unmapped when it is dropped.
#[derive(Debug)]
pub(crate) struct Map {
    /// The mapping's first byte. Always a multiple of the page size.
    ptr: NonNull<u8>,
    /// The mapping's length in bytes, as asked for.
    len: usize,
    /// Whether it was mapped read-write.
    writable: bool,
}

// SAFETY: a `Map` owns its mapping, and nothing in it is tied to the thread that made
// it. Its methods only read page state or touch bytes one at a time.
unsafe impl Send for Map {}
// SAFETY: as for `Send` above.
unsafe impl Sync for Map {}

impl Drop for Map {
    /// Purpose: This function unmaps the mapping.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn drop(&mut self)
    {
        // SAFETY: `ptr` and `len` are exactly what mmap returned and was given, and a
        // map is dropped once, so it is unmapped once.
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len.max(1)) };
    }
}

impl Map {
    /// Purpose: This function maps part of a file or a shared memory object.
    ///
    /// Inputs:
    /// - `fd`: the file or object.
    /// - `offset`: where the mapping starts in it; a multiple of the page size.
    /// - `len`: the mapping's length in bytes.
    /// - `writable`: map it read-write rather than read-only. Either way the mapping is
    ///   shared, so every process that maps the object sees the same bytes.
    ///
    /// Outputs: the [`Map`], or the system's error.
    pub(crate) fn new(
        fd: BorrowedFd<'_>,
        offset: u64,
        len: usize,
        writable: bool,
    ) -> io::Result<Map>
    {
        let prot = if writable { libc::PROT_READ | libc::PROT_WRITE } else { libc::PROT_READ };
        let off = libc::off_t::try_from(offset).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the offset is too large"))?;
        // SAFETY: a new mapping at an address the system picks touches no existing
        // memory. An empty one is asked for as one byte, since mmap refuses length 0.
        let p = unsafe { libc::mmap(std::ptr::null_mut(), len.max(1), prot, libc::MAP_SHARED, fd.as_raw_fd(), off) };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let ptr = NonNull::new(p.cast::<u8>()).ok_or_else(|| io::Error::other("mmap returned address 0"))?;
        Ok(Map { ptr, len, writable })
    }

    /// Purpose: This function maps a range of a file whose start need not be a multiple
    /// of the page size.
    ///
    /// Inputs:
    /// - `fd`: the file or object.
    /// - `offset`: where the range starts in it.
    /// - `len`: the range's length in bytes.
    /// - `writable`: map it read-write rather than read-only.
    ///
    /// Outputs: the [`Map`] and how far into it the range starts, or the system's error.
    pub(crate) fn range(
        fd: BorrowedFd<'_>,
        offset: u64,
        len: usize,
        writable: bool,
    ) -> io::Result<(Map, usize)>
    {
        let page = page_size() as u64;
        let start = offset - offset % page;
        let delta = (offset - start) as usize;
        let total = delta.checked_add(len).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the range is too large"))?;
        let map = Map::new(fd, start, total, writable)?;
        Ok((map, delta))
    }

    /// Purpose: This function gives the mapping's length.
    ///
    /// Inputs: none.
    ///
    /// Outputs: its length in bytes, as asked for.
    pub(crate) fn len(&self) -> usize
    {
        self.len
    }

    /// Purpose: This function gives the mapping's first byte.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the address.
    pub(crate) fn as_ptr(&self) -> NonNull<u8>
    {
        self.ptr
    }

    /// Purpose: This function counts the bytes of part of the mapping that are in
    /// physical memory.
    ///
    /// Inputs:
    /// - `start`: where the part starts, in bytes from the mapping's start.
    /// - `len`: the part's length.
    ///
    /// Outputs: the resident bytes, counting each resident page's bytes that lie in the
    /// part; or the system's error.
    pub(crate) fn resident(
        &self,
        start: usize,
        len: usize,
    ) -> io::Result<u64>
    {
        let end = start.checked_add(len).filter(|e| *e <= self.len).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the range is outside the mapping"))?;
        if len == 0 {
            return Ok(0);
        }
        let page = page_size();
        let first = start / page * page;
        let last = end.div_ceil(page) * page;
        let mut vec = vec![0u8; (last - first) / page];
        // SAFETY: `first..last` is whole pages of this mapping: `first` is a page
        // multiple at or after its start, and `last` is at most its length rounded up to
        // a page, which the system maps in whole pages. `vec` has one byte per page.
        let rc = unsafe { libc::mincore(self.ptr.as_ptr().add(first).cast(), last - first, vec.as_mut_ptr().cast()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut bytes = 0u64;
        for (i, v) in vec.iter().enumerate() {
            if v & 1 != 0 {
                let lo = (first + i * page).max(start);
                let hi = (first + (i + 1) * page).min(end);
                bytes += (hi - lo) as u64;
            }
        }
        Ok(bytes)
    }

    /// Purpose: This function brings every page of a mapping into physical memory: a
    /// read of each page for a read-only mapping, and for a writable one an atomic add of
    /// nothing, which makes the page without changing a byte a program may already have
    /// written there.
    ///
    /// Inputs:
    /// - `progress`: called with the bytes done since its last call, every 64 MiB and at
    ///   the end.
    ///
    /// Outputs: none.
    pub(crate) fn touch(
        &self,
        mut progress: impl FnMut(u64),
    )
    {
        const CHUNK: usize = 64 << 20;
        let page = page_size();
        // A hint to read ahead; a system that ignores it only loads more slowly.
        // SAFETY: the range is this whole mapping.
        unsafe { libc::madvise(self.ptr.as_ptr().cast(), self.len.max(1), libc::MADV_WILLNEED) };
        let mut done = 0usize;
        while done < self.len {
            let end = (done + CHUNK).min(self.len);
            let mut at = done;
            while at < end {
                // SAFETY: `at` is less than the mapping's length, so the byte is mapped.
                // Shared memory has a name as soon as it is made, so a program may map it
                // and write it while it loads; an atomic `or` of zero is a write the page
                // needs and leaves the byte as that program left it.
                unsafe {
                    let p = self.ptr.as_ptr().add(at);
                    if self.writable {
                        std::sync::atomic::AtomicU8::from_ptr(p).fetch_or(0, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        std::ptr::read_volatile(p);
                    }
                }
                at += page;
            }
            progress((end - done) as u64);
            done = end;
        }
    }
}

/// Purpose: This function gives the system's page size.
///
/// Inputs: none.
///
/// Outputs: the page size in bytes, such as 16384 on Apple silicon.
pub(crate) fn page_size() -> usize
{
    // SAFETY: sysconf reads a constant of the system.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(p).ok().filter(|p| p.is_power_of_two()).unwrap_or(4096)
}

/// Purpose: This function makes a named shared memory object, readable and writable by
/// this user only, replacing one a stopped process left under the same name. The object
/// stays until its name is removed and every process that holds or maps it is done.
///
/// Inputs:
/// - `name`: its name, such as `/dovata.1f2e3d4c5b6a7980.2`.
/// - `len`: its size in bytes.
///
/// Outputs: the object's file descriptor, or the system's error.
pub(crate) fn create_shared(
    name: &str,
    len: u64,
) -> io::Result<OwnedFd>
{
    unlink_shared(name);
    let fd = shm_open(name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL)?;
    let size = libc::off_t::try_from(len).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the size is too large"))?;
    // SAFETY: `fd` is a valid descriptor that this function owns.
    if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } != 0 {
        let e = io::Error::last_os_error();
        unlink_shared(name);
        return Err(e);
    }
    // On Linux a named object lives on the /dev/shm file system, which holds half the
    // machine's memory by default and 64 MiB in a default container. A size past that is
    // allowed, and the first write past it would end dovata with SIGBUS; taking every page
    // now refuses the model instead.
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `fd` is a valid descriptor that this function owns. posix_fallocate
        // returns the error rather than setting errno.
        let rc = unsafe { libc::posix_fallocate(fd.as_raw_fd(), 0, size) };
        if rc != 0 {
            unlink_shared(name);
            let e = io::Error::from_raw_os_error(rc);
            if rc == libc::ENOSPC {
                return Err(io::Error::new(e.kind(), format!("{e}; /dev/shm has no room for {len} more bytes, so make it larger or hold the model with less KV cache")));
            }
            return Err(e);
        }
    }
    Ok(fd)
}

/// Purpose: This function checks that a shared memory object belongs to this user and
/// that no one else may read or write it. Names are shared by every user of a machine, so
/// another user could make an object under a name before dovata does.
///
/// Inputs:
/// - `fd`: the object.
///
/// Outputs: `Ok(true)` when the object is this user's alone, or the system's error.
pub(crate) fn private_to_me(
    fd: BorrowedFd<'_>,
) -> io::Result<bool>
{
    // SAFETY: a zeroed stat is a valid one to fill in.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a valid descriptor and `st` lives through the call.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid only reads this process's user id.
    let me = unsafe { libc::geteuid() };
    Ok(st.st_uid == me && st.st_mode & 0o077 == 0)
}

/// Purpose: This function opens a named shared memory object the dovata process made.
///
/// Inputs:
/// - `name`: its name.
/// - `writable`: open it for writing as well as reading.
///
/// Outputs: the object's file descriptor, or the system's error, such as no object of
/// that name.
pub(crate) fn open_shared(
    name: &str,
    writable: bool,
) -> io::Result<OwnedFd>
{
    shm_open(name, if writable { libc::O_RDWR } else { libc::O_RDONLY })
}

/// Purpose: This function removes a shared memory object's name. Programs that opened or
/// mapped it keep it; nobody can open it by name again.
///
/// Inputs:
/// - `name`: its name.
///
/// Outputs: none; a name that does not exist is not an error.
pub(crate) fn unlink_shared(
    name: &str,
)
{
    if let Ok(c) = std::ffi::CString::new(name) {
        // SAFETY: `c` is a NUL-terminated string that lives through the call.
        unsafe { libc::shm_unlink(c.as_ptr()) };
    }
}

/// Purpose: This function gives the size of a file or shared memory object.
///
/// Inputs:
/// - `fd`: the file or object.
///
/// Outputs: its size in bytes, or the system's error.
pub(crate) fn object_size(
    fd: BorrowedFd<'_>,
) -> io::Result<u64>
{
    // SAFETY: a zeroed stat is a valid one to fill in.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is a valid descriptor and `st` lives through the call.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    u64::try_from(st.st_size).map_err(|_| io::Error::other("a negative size"))
}

/// Purpose: This function opens a POSIX shared memory object, making it readable and
/// writable by this user only when it is made. The object's descriptor is closed when a
/// program dovata starts runs: shm_open sets close-on-exec on Linux and macOS alike.
///
/// Inputs:
/// - `name`: its name.
/// - `flags`: the open flags.
///
/// Outputs: the object's file descriptor, or the system's error.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn shm_open(
    name: &str,
    flags: libc::c_int,
) -> io::Result<OwnedFd>
{
    let c = std::ffi::CString::new(name).map_err(io::Error::other)?;
    // SAFETY: `c` is a NUL-terminated string that lives through the call. macOS declares
    // shm_open variadic, so its mode is passed as an unsigned int, the type a variadic
    // argument is promoted to; Linux takes a mode_t.
    #[cfg(target_vendor = "apple")]
    let fd = unsafe { libc::shm_open(c.as_ptr(), flags, 0o600 as libc::c_uint) };
    #[cfg(target_os = "linux")]
    let fd = unsafe { libc::shm_open(c.as_ptr(), flags, 0o600 as libc::mode_t) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Purpose: This function refuses shared memory on a system dovata has not been built
/// for, whose `shm_open` takes its mode differently.
///
/// Inputs:
/// - `name`: unused.
/// - `flags`: unused.
///
/// Outputs: always an error.
#[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
fn shm_open(
    name: &str,
    flags: libc::c_int,
) -> io::Result<OwnedFd>
{
    let _ = (name, flags);
    Err(io::Error::new(io::ErrorKind::Unsupported, "dovata makes shared memory on Linux and macOS only"))
}

/// Purpose: This function says whether a process is still running.
///
/// Inputs:
/// - `pid`: the process id.
///
/// Outputs: `true` when a process with that id exists.
pub(crate) fn alive(
    pid: u32,
) -> bool
{
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: signal 0 sends nothing; it only checks that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
