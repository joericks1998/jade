//! Author: Joe Ricks
//! File: src/host/cuda.rs
//! Description: Opens NVIDIA's driver when a program first needs it and makes the few
//! calls dovata needs: VRAM allocated, filled and shared with other processes by IPC
//! handle, and those handles opened in another process. The host's unsafe CUDA calls are
//! all in this file.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, OnceLock};

/// The bytes in a CUDA IPC memory handle: `CU_IPC_HANDLE_SIZE` in `cuda.h`.
pub(crate) const HANDLE_BYTES: usize = 64;

/// Each VRAM region is allocated in a multiple of this many bytes, the size NVIDIA
/// suggests for memory shared between processes.
pub(crate) const ALLOC_GRAIN: u64 = 2 << 20;

/// `CUDA_SUCCESS`.
const SUCCESS: c_int = 0;

/// `CU_IPC_MEM_LAZY_ENABLE_PEER_ACCESS`, the one flag `cuIpcOpenMemHandle` takes.
const LAZY_ENABLE_PEER_ACCESS: c_uint = 1;

/// Purpose: This type alias names a CUDA device pointer, `CUdeviceptr`, which is 64 bits
/// on every 64-bit system.
type DevicePtr = u64;

/// Purpose: This type alias names a CUDA context, `CUcontext`.
type Context = *mut c_void;

/// Purpose: This struct holds a CUDA IPC memory handle, `CUipcMemHandle`, as the driver
/// passes it: 64 opaque bytes, by value.
#[repr(C)]
#[derive(Clone, Copy)]
struct IpcHandle {
    /// The handle's bytes.
    reserved: [c_char; HANDLE_BYTES],
}

/// Purpose: This struct holds the driver functions dovata calls, found in `libcuda.so.1`
/// by name. The names are the ones `cuda.h` maps each call to, such as `cuMemAlloc_v2`.
struct Driver {
    /// `cuInit`.
    init: unsafe extern "C" fn(c_uint) -> c_int,
    /// `cuDeviceGetCount`.
    device_count: unsafe extern "C" fn(*mut c_int) -> c_int,
    /// `cuDeviceGet`.
    device_get: unsafe extern "C" fn(*mut c_int, c_int) -> c_int,
    /// `cuDeviceGetName`.
    device_name: unsafe extern "C" fn(*mut c_char, c_int, c_int) -> c_int,
    /// `cuDeviceGetPCIBusId`.
    device_pci_bus_id: unsafe extern "C" fn(*mut c_char, c_int, c_int) -> c_int,
    /// `cuDeviceGetByPCIBusId`.
    device_by_pci_bus_id: unsafe extern "C" fn(*mut c_int, *const c_char) -> c_int,
    /// `cuDevicePrimaryCtxRetain`.
    primary_retain: unsafe extern "C" fn(*mut Context, c_int) -> c_int,
    /// `cuDevicePrimaryCtxRelease_v2`.
    primary_release: unsafe extern "C" fn(c_int) -> c_int,
    /// `cuCtxSetCurrent`.
    set_current: unsafe extern "C" fn(Context) -> c_int,
    /// `cuMemGetInfo_v2`.
    mem_info: unsafe extern "C" fn(*mut usize, *mut usize) -> c_int,
    /// `cuMemAlloc_v2`.
    alloc: unsafe extern "C" fn(*mut DevicePtr, usize) -> c_int,
    /// `cuMemFree_v2`.
    free: unsafe extern "C" fn(DevicePtr) -> c_int,
    /// `cuMemAllocHost_v2`.
    alloc_host: unsafe extern "C" fn(*mut *mut c_void, usize) -> c_int,
    /// `cuMemFreeHost`.
    free_host: unsafe extern "C" fn(*mut c_void) -> c_int,
    /// `cuMemcpyHtoD_v2`.
    to_device: unsafe extern "C" fn(DevicePtr, *const c_void, usize) -> c_int,
    /// `cuMemcpyDtoH_v2`.
    to_host: unsafe extern "C" fn(*mut c_void, DevicePtr, usize) -> c_int,
    /// `cuMemsetD8_v2`.
    set_bytes: unsafe extern "C" fn(DevicePtr, u8, usize) -> c_int,
    /// `cuIpcGetMemHandle`.
    ipc_get: unsafe extern "C" fn(*mut IpcHandle, DevicePtr) -> c_int,
    /// `cuIpcOpenMemHandle_v2`, or `cuIpcOpenMemHandle` from a driver older than CUDA 11.
    ipc_open: unsafe extern "C" fn(*mut DevicePtr, IpcHandle, c_uint) -> c_int,
    /// `cuIpcCloseMemHandle`.
    ipc_close: unsafe extern "C" fn(DevicePtr) -> c_int,
    /// `cuCtxSynchronize`.
    synchronize: unsafe extern "C" fn() -> c_int,
    /// `cuGetErrorName`.
    error_name: unsafe extern "C" fn(c_int, *mut *const c_char) -> c_int,
    /// `cuGetErrorString`.
    error_string: unsafe extern "C" fn(c_int, *mut *const c_char) -> c_int,
}

/// The driver, opened and initialized the first time a program needs it. A failure is
/// not kept, so a driver installed or loaded later is found on the next try.
static DRIVER: OnceLock<Driver> = OnceLock::new();

/// Every GPU this program has opened, kept for the program's life so each device's
/// context is retained once.
static GPUS: Mutex<Vec<Arc<Gpu>>> = Mutex::new(Vec::new());

/// Purpose: This function gives the driver, opening and initializing it on first use.
///
/// Inputs: none.
///
/// Outputs: the driver, or a message saying why it cannot be opened.
fn driver() -> Result<&'static Driver, String>
{
    if let Some(d) = DRIVER.get() {
        return Ok(d);
    }
    // Two threads may both open it; one table is kept, and the other holds only
    // function pointers into the same library, which is never closed.
    let d = open_driver()?;
    Ok(DRIVER.get_or_init(|| d))
}

/// Purpose: This function opens `libcuda.so.1`, finds every function dovata calls, and
/// initializes the driver.
///
/// Inputs: none.
///
/// Outputs: the driver, or a message naming what is missing.
fn open_driver() -> Result<Driver, String>
{
    // SAFETY: the name is a NUL-terminated string. The library is never closed, so every
    // function found in it stays valid for the program's life.
    let lib = unsafe { libc::dlopen(c"libcuda.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if lib.is_null() {
        return Err(format!(
            "NVIDIA's driver library, libcuda.so.1, cannot be opened ({}); install NVIDIA's driver, or add the model with --cpu",
            dl_error()
        ));
    }
    // SAFETY: each name below is the symbol `cuda.h` maps the call to, and each field's
    // type is that call's C signature, so every pointer is called as what it is.
    let d = unsafe {
        Driver {
            init: symbol(lib, &[c"cuInit"])?,
            device_count: symbol(lib, &[c"cuDeviceGetCount"])?,
            device_get: symbol(lib, &[c"cuDeviceGet"])?,
            device_name: symbol(lib, &[c"cuDeviceGetName"])?,
            device_pci_bus_id: symbol(lib, &[c"cuDeviceGetPCIBusId"])?,
            device_by_pci_bus_id: symbol(lib, &[c"cuDeviceGetByPCIBusId"])?,
            primary_retain: symbol(lib, &[c"cuDevicePrimaryCtxRetain"])?,
            primary_release: symbol(lib, &[c"cuDevicePrimaryCtxRelease_v2", c"cuDevicePrimaryCtxRelease"])?,
            set_current: symbol(lib, &[c"cuCtxSetCurrent"])?,
            mem_info: symbol(lib, &[c"cuMemGetInfo_v2"])?,
            alloc: symbol(lib, &[c"cuMemAlloc_v2"])?,
            free: symbol(lib, &[c"cuMemFree_v2"])?,
            alloc_host: symbol(lib, &[c"cuMemAllocHost_v2"])?,
            free_host: symbol(lib, &[c"cuMemFreeHost"])?,
            to_device: symbol(lib, &[c"cuMemcpyHtoD_v2"])?,
            to_host: symbol(lib, &[c"cuMemcpyDtoH_v2"])?,
            set_bytes: symbol(lib, &[c"cuMemsetD8_v2"])?,
            ipc_get: symbol(lib, &[c"cuIpcGetMemHandle"])?,
            ipc_open: symbol(lib, &[c"cuIpcOpenMemHandle_v2", c"cuIpcOpenMemHandle"])?,
            ipc_close: symbol(lib, &[c"cuIpcCloseMemHandle"])?,
            synchronize: symbol(lib, &[c"cuCtxSynchronize"])?,
            error_name: symbol(lib, &[c"cuGetErrorName"])?,
            error_string: symbol(lib, &[c"cuGetErrorString"])?,
        }
    };
    // SAFETY: `cuInit` takes flags, which must be 0.
    let rc = unsafe { (d.init)(0) };
    check(&d, rc, "cuInit")?;
    Ok(d)
}

/// Purpose: This function finds a function in an opened library by the first of its
/// names the library has.
///
/// Inputs:
/// - `lib`: the library, from `dlopen`.
/// - `names`: the function's names, newest first.
///
/// Outputs: the function, or a message naming it.
///
/// # Safety
///
/// `F` must be a function pointer type matching the function's C signature.
unsafe fn symbol<F: Copy>(
    lib: *mut c_void,
    names: &[&CStr],
) -> Result<F, String>
{
    assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*mut c_void>(), "a symbol is a function pointer");
    for name in names {
        // SAFETY: `lib` came from `dlopen` and `name` is NUL-terminated.
        let p = unsafe { libc::dlsym(lib, name.as_ptr()) };
        if !p.is_null() {
            // SAFETY: `F` is a function pointer of the same size, and the function has
            // the signature `F` names, as this function's caller promises.
            return Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) });
        }
    }
    let name = names.first().map(|n| n.to_string_lossy()).unwrap_or_default();
    Err(format!("NVIDIA's driver has no {name}; update the driver"))
}

/// Purpose: This function gives the reason the last `dlopen` failed.
///
/// Inputs: none.
///
/// Outputs: the system's message, or a plain one when it gives none.
fn dl_error() -> String
{
    // SAFETY: `dlerror` returns NULL or a NUL-terminated string owned by the system,
    // copied out at once.
    let p = unsafe { libc::dlerror() };
    if p.is_null() {
        return "no reason given".into();
    }
    // SAFETY: as above.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Purpose: This function turns a driver call's result into an error naming the call
/// and the driver's reason.
///
/// Inputs:
/// - `d`: the driver.
/// - `rc`: the call's result.
/// - `call`: the call's name, such as `cuMemAlloc`.
///
/// Outputs: `Ok(())` for `CUDA_SUCCESS`, or the message.
fn check(
    d: &Driver,
    rc: c_int,
    call: &str,
) -> Result<(), String>
{
    if rc == SUCCESS {
        return Ok(());
    }
    let text = |f: unsafe extern "C" fn(c_int, *mut *const c_char) -> c_int| {
        let mut p: *const c_char = std::ptr::null();
        // SAFETY: the driver writes a pointer to a static NUL-terminated string, or leaves
        // NULL for a result it does not know.
        let ok = unsafe { f(rc, &mut p) } == SUCCESS && !p.is_null();
        // SAFETY: as above.
        ok.then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    };
    let name = text(d.error_name).unwrap_or_else(|| format!("CUDA error {rc}"));
    match text(d.error_string) {
        Some(s) => Err(format!("{call} failed: {name}, {s}")),
        None => Err(format!("{call} failed: {name}")),
    }
}

/// Purpose: This function reads a string the driver writes into a buffer.
///
/// Inputs:
/// - `fill`: writes into the buffer it is given, of the length it is given, and returns
///   the driver's result.
/// - `d`: the driver.
/// - `call`: the call's name, for the error.
///
/// Outputs: the string, or the driver's error.
fn read_string(
    fill: impl FnOnce(*mut c_char, c_int) -> c_int,
    d: &Driver,
    call: &str,
) -> Result<String, String>
{
    let mut buf = [0 as c_char; 256];
    check(d, fill(buf.as_mut_ptr(), buf.len() as c_int), call)?;
    // The driver writes at most the length it is given; the last byte stays NUL.
    buf[buf.len() - 1] = 0;
    // SAFETY: the buffer is NUL-terminated, set just above.
    Ok(unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

/// Purpose: This struct holds one NVIDIA GPU this program uses, with its primary context
/// retained for as long as the program runs.
pub(crate) struct Gpu {
    /// The driver.
    driver: &'static Driver,
    /// The device's number in this process.
    device: c_int,
    /// The device's primary context.
    context: Context,
    /// The device's name, such as `NVIDIA GeForce RTX 3090`.
    pub(crate) name: String,
    /// The device's PCI bus id, such as `0000:01:00.0`, which names the same GPU in every
    /// process, whatever `CUDA_VISIBLE_DEVICES` each one sets.
    pub(crate) pci_bus_id: String,
}

// SAFETY: a CUDA context may be made current on any thread, and the driver's calls are
// safe to make from several threads at once; `Gpu` holds nothing else.
unsafe impl Send for Gpu {}
// SAFETY: as for `Send` above.
unsafe impl Sync for Gpu {}

impl Drop for Gpu {
    /// Purpose: This function releases the device's primary context.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn drop(&mut self)
    {
        // SAFETY: the context was retained once, when this `Gpu` was made.
        unsafe { (self.driver.primary_release)(self.device) };
    }
}

impl Gpu {
    /// Purpose: This function opens the first NVIDIA GPU, the one dovata plans for.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the GPU, or a message saying why there is none.
    pub(crate) fn first() -> Result<Arc<Gpu>, String>
    {
        let d = driver()?;
        let mut count: c_int = 0;
        // SAFETY: the driver writes one int.
        check(d, unsafe { (d.device_count)(&mut count) }, "cuDeviceGetCount")?;
        if count < 1 {
            return Err("NVIDIA's driver sees no GPU; check `nvidia-smi`".into());
        }
        let mut device: c_int = 0;
        // SAFETY: the driver writes one int, for device 0, which exists.
        check(d, unsafe { (d.device_get)(&mut device, 0) }, "cuDeviceGet")?;
        Gpu::open(d, device)
    }

    /// Purpose: This function opens the NVIDIA GPU at a PCI bus id.
    ///
    /// Inputs:
    /// - `pci_bus_id`: the id, such as `0000:01:00.0`.
    ///
    /// Outputs: the GPU, or a message saying why it cannot be opened.
    pub(crate) fn at(
        pci_bus_id: &str,
    ) -> Result<Arc<Gpu>, String>
    {
        let d = driver()?;
        let id = CString::new(pci_bus_id).map_err(|_| format!("{pci_bus_id:?} is not a PCI bus id"))?;
        let mut device: c_int = 0;
        // SAFETY: the id is NUL-terminated, and the driver writes one int.
        check(d, unsafe { (d.device_by_pci_bus_id)(&mut device, id.as_ptr()) }, "cuDeviceGetByPCIBusId")?;
        Gpu::open(d, device)
    }

    /// Purpose: This function gives the GPU for a device, retaining its primary context
    /// the first time this program asks for it.
    ///
    /// Inputs:
    /// - `d`: the driver.
    /// - `device`: the device's number.
    ///
    /// Outputs: the GPU, or the driver's error.
    fn open(
        d: &'static Driver,
        device: c_int,
    ) -> Result<Arc<Gpu>, String>
    {
        let mut gpus = GPUS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(g) = gpus.iter().find(|g| g.device == device) {
            return Ok(Arc::clone(g));
        }
        // SAFETY: the driver writes the name into the buffer, at most `len` bytes.
        let name = read_string(|p, len| unsafe { (d.device_name)(p, len, device) }, d, "cuDeviceGetName")?;
        // SAFETY: as above, for the PCI bus id.
        let pci_bus_id = read_string(|p, len| unsafe { (d.device_pci_bus_id)(p, len, device) }, d, "cuDeviceGetPCIBusId")?;
        let mut context: Context = std::ptr::null_mut();
        // SAFETY: the driver writes one context pointer.
        check(d, unsafe { (d.primary_retain)(&mut context, device) }, "cuDevicePrimaryCtxRetain")?;
        let gpu = Arc::new(Gpu { driver: d, device, context, name, pci_bus_id });
        gpus.push(Arc::clone(&gpu));
        Ok(gpu)
    }

    /// Purpose: This function makes the GPU's context current on the calling thread, which
    /// every other driver call needs.
    ///
    /// Inputs: none.
    ///
    /// Outputs: `Ok(())`, or the driver's error.
    fn bind(&self) -> Result<(), String>
    {
        // SAFETY: the context is retained for as long as `self` lives.
        check(self.driver, unsafe { (self.driver.set_current)(self.context) }, "cuCtxSetCurrent")
    }

    /// Purpose: This function reads how much of the GPU's memory is free right now.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the free and total bytes, or the driver's error.
    pub(crate) fn memory(&self) -> Result<(u64, u64), String>
    {
        self.bind()?;
        let (mut free, mut total) = (0usize, 0usize);
        // SAFETY: the driver writes two sizes.
        check(self.driver, unsafe { (self.driver.mem_info)(&mut free, &mut total) }, "cuMemGetInfo")?;
        Ok((free as u64, total as u64))
    }

    /// Purpose: This function allocates VRAM, rounded up to a whole [`ALLOC_GRAIN`].
    ///
    /// Inputs:
    /// - `bytes`: the bytes wanted; 0 still allocates one grain.
    ///
    /// Outputs: the allocation, or the driver's error.
    pub(crate) fn alloc(
        self: &Arc<Self>,
        bytes: u64,
    ) -> Result<Vram, String>
    {
        let len = bytes.max(1).div_ceil(ALLOC_GRAIN) * ALLOC_GRAIN;
        let size = usize::try_from(len).map_err(|_| format!("{len} bytes of VRAM do not fit in this program's address space"))?;
        self.bind()?;
        let mut ptr: DevicePtr = 0;
        // SAFETY: the driver writes one device pointer.
        check(self.driver, unsafe { (self.driver.alloc)(&mut ptr, size) }, "cuMemAlloc")?;
        Ok(Vram { gpu: Arc::clone(self), ptr, len })
    }

    /// Purpose: This function waits until the GPU has finished everything this program
    /// asked of it, such as a memset, which returns before it is done.
    ///
    /// Inputs: none.
    ///
    /// Outputs: `Ok(())`, or the driver's error, including one from earlier work.
    fn synchronize(&self) -> Result<(), String>
    {
        self.bind()?;
        // SAFETY: the call takes no arguments.
        check(self.driver, unsafe { (self.driver.synchronize)() }, "cuCtxSynchronize")
    }

    /// Purpose: This function allocates page-locked host memory, which the GPU copies
    /// from at full speed.
    ///
    /// Inputs:
    /// - `len`: the bytes wanted.
    ///
    /// Outputs: the buffer, or the driver's error.
    pub(crate) fn pinned(
        self: &Arc<Self>,
        len: usize,
    ) -> Result<Pinned, String>
    {
        self.bind()?;
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: the driver writes one host pointer to `len` bytes.
        check(self.driver, unsafe { (self.driver.alloc_host)(&mut p, len.max(1)) }, "cuMemAllocHost")?;
        let ptr = NonNull::new(p.cast::<u8>()).ok_or("cuMemAllocHost returned no memory")?;
        Ok(Pinned { gpu: Arc::clone(self), ptr, len })
    }

    /// Purpose: This function opens VRAM another process shared, by its IPC handle.
    ///
    /// Inputs:
    /// - `handle`: the handle's bytes, as the other process made them.
    ///
    /// Outputs: the opened memory, or the driver's error.
    pub(crate) fn import(
        self: &Arc<Self>,
        handle: &[u8; HANDLE_BYTES],
    ) -> Result<Imported, String>
    {
        self.bind()?;
        let mut h = IpcHandle { reserved: [0; HANDLE_BYTES] };
        for (to, from) in h.reserved.iter_mut().zip(handle) {
            *to = *from as c_char;
        }
        let mut ptr: DevicePtr = 0;
        // SAFETY: the driver writes one device pointer; the handle is passed by value.
        check(self.driver, unsafe { (self.driver.ipc_open)(&mut ptr, h, LAZY_ENABLE_PEER_ACCESS) }, "cuIpcOpenMemHandle")?;
        Ok(Imported { gpu: Arc::clone(self), ptr })
    }

    /// Purpose: This function copies host bytes into VRAM.
    ///
    /// Inputs:
    /// - `to`: the device address to copy to.
    /// - `from`: the bytes.
    ///
    /// Outputs: `Ok(())`, or the driver's error.
    fn copy_in(
        &self,
        to: DevicePtr,
        from: &[u8],
    ) -> Result<(), String>
    {
        if from.is_empty() {
            return Ok(());
        }
        self.bind()?;
        // SAFETY: `from` is valid for its length, and every caller checks that the device
        // range lies inside one allocation.
        check(self.driver, unsafe { (self.driver.to_device)(to, from.as_ptr().cast(), from.len()) }, "cuMemcpyHtoD")
    }

    /// Purpose: This function copies VRAM into host bytes.
    ///
    /// Inputs:
    /// - `from`: the device address to copy from.
    /// - `to`: the bytes to fill, all of them.
    ///
    /// Outputs: `Ok(())`, or the driver's error.
    fn copy_out(
        &self,
        from: DevicePtr,
        to: &mut [u8],
    ) -> Result<(), String>
    {
        if to.is_empty() {
            return Ok(());
        }
        self.bind()?;
        // SAFETY: `to` is valid for writes of its length, and every caller checks that the
        // device range lies inside one allocation.
        check(self.driver, unsafe { (self.driver.to_host)(to.as_mut_ptr().cast(), from, to.len()) }, "cuMemcpyDtoH")
    }
}

/// Purpose: This struct holds one VRAM allocation this process made, freed when it is
/// dropped.
pub(crate) struct Vram {
    /// The GPU it is on.
    gpu: Arc<Gpu>,
    /// Its device address.
    ptr: DevicePtr,
    /// Its size in bytes, a whole number of [`ALLOC_GRAIN`]s.
    len: u64,
}

impl Drop for Vram {
    /// Purpose: This function frees the allocation.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn drop(&mut self)
    {
        if self.gpu.bind().is_ok() {
            // SAFETY: the pointer came from `cuMemAlloc` and is freed once, here.
            unsafe { (self.gpu.driver.free)(self.ptr) };
        }
    }
}

impl Vram {
    /// Purpose: This function gives the allocation's size.
    ///
    /// Inputs: none.
    ///
    /// Outputs: its size in bytes.
    pub(crate) fn len(&self) -> u64
    {
        self.len
    }

    /// Purpose: This function gives the allocation's device address.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the address.
    pub(crate) fn device_ptr(&self) -> u64
    {
        self.ptr
    }

    /// Purpose: This function allocates page-locked host memory on the allocation's GPU,
    /// for copying into it at full speed.
    ///
    /// Inputs:
    /// - `len`: the bytes wanted.
    ///
    /// Outputs: the buffer, or the driver's error.
    pub(crate) fn pinned(
        &self,
        len: usize,
    ) -> Result<Pinned, String>
    {
        self.gpu.pinned(len)
    }

    /// Purpose: This function waits until the GPU has finished every copy and memset
    /// asked of it.
    ///
    /// Inputs: none.
    ///
    /// Outputs: `Ok(())`, or the driver's error, including one from earlier work.
    pub(crate) fn synchronize(&self) -> Result<(), String>
    {
        self.gpu.synchronize()
    }

    /// Purpose: This function makes the IPC handle another process opens the allocation
    /// by.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the handle's 64 bytes, or the driver's error.
    pub(crate) fn handle(&self) -> Result<[u8; HANDLE_BYTES], String>
    {
        self.gpu.bind()?;
        let mut h = IpcHandle { reserved: [0; HANDLE_BYTES] };
        // SAFETY: the pointer is the start of a `cuMemAlloc` allocation, and the driver
        // writes one handle.
        check(self.gpu.driver, unsafe { (self.gpu.driver.ipc_get)(&mut h, self.ptr) }, "cuIpcGetMemHandle")?;
        Ok(h.reserved.map(|b| b as u8))
    }

    /// Purpose: This function sets every byte of the allocation to zero.
    ///
    /// Inputs: none.
    ///
    /// Outputs: `Ok(())`, or the driver's error.
    pub(crate) fn zero(&self) -> Result<(), String>
    {
        self.gpu.bind()?;
        let len = usize::try_from(self.len).map_err(|_| "the allocation is larger than this program's address space".to_string())?;
        // SAFETY: the range is exactly the allocation.
        check(self.gpu.driver, unsafe { (self.gpu.driver.set_bytes)(self.ptr, 0, len) }, "cuMemsetD8")
    }

    /// Purpose: This function copies host bytes into the allocation.
    ///
    /// Inputs:
    /// - `offset`: where to start, in bytes from the allocation's start.
    /// - `from`: the bytes.
    ///
    /// Outputs: `Ok(())`, or an error for a range past the end or from the driver.
    pub(crate) fn write(
        &self,
        offset: u64,
        from: &[u8],
    ) -> Result<(), String>
    {
        if offset.checked_add(from.len() as u64).is_none_or(|end| end > self.len) {
            return Err(format!("{} bytes at {offset} run past the end of a {}-byte VRAM allocation", from.len(), self.len));
        }
        self.gpu.copy_in(self.ptr + offset, from)
    }
}

/// Purpose: This struct holds page-locked host memory, freed when it is dropped.
pub(crate) struct Pinned {
    /// The GPU whose context allocated it.
    gpu: Arc<Gpu>,
    /// Its first byte.
    ptr: NonNull<u8>,
    /// Its size in bytes.
    len: usize,
}

impl Drop for Pinned {
    /// Purpose: This function frees the memory.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn drop(&mut self)
    {
        if self.gpu.bind().is_ok() {
            // SAFETY: the pointer came from `cuMemAllocHost` and is freed once, here.
            unsafe { (self.gpu.driver.free_host)(self.ptr.as_ptr().cast()) };
        }
    }
}

impl Pinned {
    /// Purpose: This function gives the memory as bytes to fill.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the bytes.
    pub(crate) fn bytes(&mut self) -> &mut [u8]
    {
        // SAFETY: the memory is `len` bytes this struct owns alone, and `&mut self` keeps
        // any other borrow out while the slice lives.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

/// Purpose: This struct holds VRAM another process shared and this one opened, closed
/// when it is dropped.
#[derive(Debug)]
pub(crate) struct Imported {
    /// The GPU it is on.
    gpu: Arc<Gpu>,
    /// Its device address in this process, which may differ from the address in the
    /// process that shared it.
    ptr: DevicePtr,
}

impl std::fmt::Debug for Gpu {
    /// Purpose: This function formats a GPU by its name and PCI bus id.
    ///
    /// Inputs:
    /// - `f`: the formatter Rust writes the text into.
    ///
    /// Outputs: `Ok(())`, or an error if writing fails.
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result
    {
        write!(f, "{} at {}", self.name, self.pci_bus_id)
    }
}

impl Drop for Imported {
    /// Purpose: This function closes the opened memory.
    ///
    /// Inputs: none.
    ///
    /// Outputs: none.
    fn drop(&mut self)
    {
        if self.gpu.bind().is_ok() {
            // SAFETY: the pointer came from `cuIpcOpenMemHandle` and is closed once, here.
            unsafe { (self.gpu.driver.ipc_close)(self.ptr) };
        }
    }
}

impl Imported {
    /// Purpose: This function gives the memory's device address in this process.
    ///
    /// Inputs: none.
    ///
    /// Outputs: the address.
    pub(crate) fn device_ptr(&self) -> u64
    {
        self.ptr
    }

    /// Purpose: This function copies bytes out of the memory. The range is checked by
    /// the region that owns this memory.
    ///
    /// Inputs:
    /// - `offset`: where to start, in bytes from the memory's start.
    /// - `to`: the bytes to fill.
    ///
    /// Outputs: `Ok(())`, or the driver's error.
    pub(crate) fn read(
        &self,
        offset: u64,
        to: &mut [u8],
    ) -> Result<(), String>
    {
        self.gpu.copy_out(self.ptr + offset, to)
    }

    /// Purpose: This function copies bytes into the memory. The range is checked by the
    /// region that owns this memory.
    ///
    /// Inputs:
    /// - `offset`: where to start, in bytes from the memory's start.
    /// - `from`: the bytes.
    ///
    /// Outputs: `Ok(())`, or the driver's error.
    pub(crate) fn write(
        &self,
        offset: u64,
        from: &[u8],
    ) -> Result<(), String>
    {
        self.gpu.copy_in(self.ptr + offset, from)
    }
}

/// Purpose: This function writes an IPC handle as lowercase hex, for a model's record.
///
/// Inputs:
/// - `handle`: the handle's bytes.
///
/// Outputs: 128 hex digits.
pub(crate) fn handle_hex(
    handle: &[u8; HANDLE_BYTES],
) -> String
{
    handle.iter().map(|b| format!("{b:02x}")).collect()
}

/// Purpose: This function reads an IPC handle back from a model's record.
///
/// Inputs:
/// - `hex`: the handle as 128 hex digits.
///
/// Outputs: the handle's bytes, or `None` for text that is not one.
pub(crate) fn handle_from_hex(
    hex: &str,
) -> Option<[u8; HANDLE_BYTES]>
{
    if hex.len() != HANDLE_BYTES * 2 || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; HANDLE_BYTES];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_round_trips_through_hex()
    {
        let h: [u8; HANDLE_BYTES] = std::array::from_fn(|i| (i * 37 % 256) as u8);
        let hex = handle_hex(&h);
        assert_eq!(hex.len(), 128);
        assert_eq!(handle_from_hex(&hex), Some(h));
        assert_eq!(handle_from_hex(&hex[..126]), None, "too short");
        assert_eq!(handle_from_hex(&format!("{}zz", &hex[..126])), None, "not hex");
    }

    #[test]
    fn a_machine_without_nvidias_driver_says_so()
    {
        // On a Mac, or Linux with no NVIDIA driver, the error names the library; on an
        // NVIDIA machine the first GPU opens.
        match Gpu::first() {
            Ok(g) => assert!(!g.pci_bus_id.is_empty() && !g.name.is_empty(), "{g:?}"),
            Err(e) => assert!(e.contains("libcuda.so.1") || e.contains("no GPU") || e.contains("failed"), "{e}"),
        }
    }
}
