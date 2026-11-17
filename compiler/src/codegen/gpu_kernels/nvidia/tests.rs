//! Tests of NVIDIA's kernels: that they build into PTX everywhere, and, on a
//! machine with an NVIDIA GPU and its driver, that the driver loads them and
//! each gives what plain math gives.
//!
//! The GPU test opens `libcuda.so.1` itself, the driver's own library, as the
//! programs will: no CUDA toolkit is needed. Loading the PTX runs NVIDIA's
//! own assembler on it inside the driver, so a kernel the assembler refuses
//! fails here with the driver's log. On a machine without the driver or a GPU
//! the test says why and passes.
//!
//! ```sh
//! cargo test gpu_kernels::nvidia -- --nocapture
//! ```

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};

use crate::codegen::gpu_kernels::builder::BLOCK;
use crate::codegen::gpu_kernels::tests::{Runner, run_all};
use crate::infer::plan::KERNELS;

/// Every kernel builds into PTX for `sm_86`, with an entry point of its name,
/// and calls nothing the driver would have to find elsewhere, such as a math
/// library's `sinf`.
#[test]
fn every_kernel_builds_into_ptx() {
    let ptx = super::ptx().expect("the kernels build into PTX");
    assert!(ptx.contains(".target sm_86"), "{}", &ptx[..ptx.len().min(400)]);
    for name in KERNELS {
        assert!(ptx.contains(&format!(".entry {name}(")), "no entry point {name} in the PTX");
    }
    assert!(!ptx.contains(".extern"), "the PTX calls a function it does not define");
    assert!(!ptx.contains(".local"), "a kernel keeps values in local memory, which is slow");
}

/// Every kernel gives what plain math gives, on this machine's NVIDIA GPU.
#[test]
fn every_kernel_matches_plain_math_on_an_nvidia_gpu() {
    let mut gpu = match Cuda::open() {
        Ok(Some(gpu)) => gpu,
        Ok(None) => return,
        Err(e) => panic!("{e}"),
    };
    let failures = run_all(&mut gpu);
    assert!(
        failures.is_empty(),
        "{} cases failed on {}:\n{}",
        failures.len(),
        gpu.name,
        failures.join("\n")
    );
}

/// `CUresult`, where 0 is success.
type Status = c_int;

/// `CUdeviceptr`.
type DevicePtr = u64;

/// `CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR` and `_MINOR`.
const CAPABILITY_MAJOR: c_int = 75;
const CAPABILITY_MINOR: c_int = 76;

/// `CU_JIT_ERROR_LOG_BUFFER` and `CU_JIT_ERROR_LOG_BUFFER_SIZE_BYTES`.
const JIT_ERROR_LOG: c_int = 5;
const JIT_ERROR_LOG_BYTES: c_int = 6;

/// Purpose: This struct holds the driver functions the test calls, by the
/// names `cuda.h` maps each to.
struct Api {
    init: unsafe extern "C" fn(c_uint) -> Status,
    device_get: unsafe extern "C" fn(*mut c_int, c_int) -> Status,
    device_name: unsafe extern "C" fn(*mut c_char, c_int, c_int) -> Status,
    device_attribute: unsafe extern "C" fn(*mut c_int, c_int, c_int) -> Status,
    primary_retain: unsafe extern "C" fn(*mut *mut c_void, c_int) -> Status,
    primary_release: unsafe extern "C" fn(c_int) -> Status,
    set_current: unsafe extern "C" fn(*mut c_void) -> Status,
    module_load: unsafe extern "C" fn(
        *mut *mut c_void,
        *const c_void,
        c_uint,
        *mut c_int,
        *mut *mut c_void,
    ) -> Status,
    module_function: unsafe extern "C" fn(*mut *mut c_void, *mut c_void, *const c_char) -> Status,
    module_unload: unsafe extern "C" fn(*mut c_void) -> Status,
    alloc: unsafe extern "C" fn(*mut DevicePtr, usize) -> Status,
    free: unsafe extern "C" fn(DevicePtr) -> Status,
    copy_in: unsafe extern "C" fn(DevicePtr, *const c_void, usize) -> Status,
    copy_out: unsafe extern "C" fn(*mut c_void, DevicePtr, usize) -> Status,
    #[allow(clippy::type_complexity)]
    launch: unsafe extern "C" fn(
        *mut c_void,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        c_uint,
        *mut c_void,
        *mut *mut c_void,
        *mut *mut c_void,
    ) -> Status,
    synchronize: unsafe extern "C" fn() -> Status,
    error_name: unsafe extern "C" fn(Status, *mut *const c_char) -> Status,
}

/// Purpose: This struct holds the driver, a context on the first GPU, and
/// the kernels loaded into it.
struct Cuda {
    api: Api,
    device: c_int,
    /// Whether `open` retained the device's primary context, which `drop`
    /// then releases.
    retained: bool,
    /// The error a kernel left behind, when it was one the driver keeps,
    /// such as an illegal address: every later call in the process fails
    /// with it, so later cases are not run.
    broken: Option<String>,
    module: *mut c_void,
    kernels: HashMap<&'static str, *mut c_void>,
    name: String,
    _lib: libloading::Library,
}

impl Cuda {
    /// Purpose: This function opens the driver, the first GPU, and loads the
    /// kernels.
    ///
    /// Outputs: `Ok(None)` with the reason printed when the machine has no
    /// NVIDIA driver, no GPU, or one older than the PTX's generation;
    /// `Err` when the driver will not load the kernels.
    fn open() -> Result<Option<Cuda>, String> {
        let skip = |why: String| {
            eprintln!("skipped: {why}");
            Ok(None)
        };
        // SAFETY: the driver's library runs no code of ours when loaded.
        let lib = match unsafe { libloading::Library::new("libcuda.so.1") } {
            Ok(l) => l,
            Err(_) => return skip("this machine has no NVIDIA driver, libcuda.so.1".into()),
        };
        let api = match Api::load(&lib) {
            Ok(a) => a,
            Err(e) => return skip(e),
        };
        let mut cuda = Cuda {
            api,
            device: 0,
            retained: false,
            broken: None,
            module: std::ptr::null_mut(),
            kernels: HashMap::new(),
            name: String::new(),
            _lib: lib,
        };
        // SAFETY: each call is the driver function `cuda.h` names, with the
        // arguments it documents.
        unsafe {
            if (cuda.api.init)(0) != 0 {
                return skip("the NVIDIA driver would not start; is there a GPU?".into());
            }
            if (cuda.api.device_get)(&mut cuda.device, 0) != 0 {
                return skip("the NVIDIA driver found no GPU".into());
            }
            let mut name = [0 as c_char; 256];
            cuda.check(
                (cuda.api.device_name)(name.as_mut_ptr(), 256, cuda.device),
                "cuDeviceGetName",
            )?;
            cuda.name = CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned();
            let (mut major, mut minor) = (0, 0);
            cuda.check(
                (cuda.api.device_attribute)(&mut major, CAPABILITY_MAJOR, cuda.device),
                "cuDeviceGetAttribute",
            )?;
            cuda.check(
                (cuda.api.device_attribute)(&mut minor, CAPABILITY_MINOR, cuda.device),
                "cuDeviceGetAttribute",
            )?;
            if (major, minor) < (8, 6) {
                return skip(format!(
                    "{} is sm_{major}{minor}, older than the kernels' {}",
                    cuda.name,
                    super::ARCH
                ));
            }
            let mut ctx = std::ptr::null_mut();
            cuda.check(
                (cuda.api.primary_retain)(&mut ctx, cuda.device),
                "cuDevicePrimaryCtxRetain",
            )?;
            cuda.retained = true;
            cuda.check((cuda.api.set_current)(ctx), "cuCtxSetCurrent")?;
            eprintln!("running the kernels on {}, sm_{major}{minor}", cuda.name);

            let ptx = CString::new(super::ptx()?).map_err(|e| e.to_string())?;
            let mut log = vec![0 as c_char; 64 * 1024];
            let mut options = [JIT_ERROR_LOG, JIT_ERROR_LOG_BYTES];
            let mut values = [log.as_mut_ptr() as *mut c_void, log.len() as *mut c_void];
            let status = (cuda.api.module_load)(
                &mut cuda.module,
                ptx.as_ptr() as *const c_void,
                2,
                options.as_mut_ptr(),
                values.as_mut_ptr(),
            );
            if status != 0 {
                let text = CStr::from_ptr(log.as_ptr()).to_string_lossy().into_owned();
                return Err(format!(
                    "the driver would not load the PTX ({}):\n{text}",
                    cuda.status_name(status)
                ));
            }
            for name in KERNELS {
                let mut f = std::ptr::null_mut();
                let c = CString::new(*name).expect("a kernel name has no NUL");
                cuda.check((cuda.api.module_function)(&mut f, cuda.module, c.as_ptr()), name)?;
                cuda.kernels.insert(*name, f);
            }
        }
        Ok(Some(cuda))
    }

    /// Purpose: This function gives a driver status's name.
    fn status_name(&self, status: Status) -> String {
        let mut p: *const c_char = std::ptr::null();
        // SAFETY: `cuGetErrorName` points `p` at a static string or leaves it.
        unsafe {
            if (self.api.error_name)(status, &mut p) == 0 && !p.is_null() {
                return CStr::from_ptr(p).to_string_lossy().into_owned();
            }
        }
        format!("CUresult {status}")
    }

    /// Purpose: This function turns a driver status into a result.
    fn check(&self, status: Status, what: &str) -> Result<(), String> {
        if status == 0 { Ok(()) } else { Err(format!("{what}: {}", self.status_name(status))) }
    }
}

impl Drop for Cuda {
    fn drop(&mut self) {
        // SAFETY: the module and the primary context were made by `open`.
        unsafe {
            if !self.module.is_null() {
                (self.api.module_unload)(self.module);
            }
            if self.retained {
                (self.api.primary_release)(self.device);
            }
        }
    }
}

impl Runner for Cuda {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(
        &mut self,
        kernel: &str,
        bufs: &mut [Vec<u8>; 5],
        args: &[u8],
        blocks: u64,
    ) -> Result<(), String> {
        if let Some(e) = &self.broken {
            return Err(format!("not run, since an earlier kernel left the GPU unusable: {e}"));
        }
        let f = *self.kernels.get(kernel).ok_or_else(|| format!("no kernel {kernel}"))?;
        let mut ptrs: [DevicePtr; 5] = [0; 5];
        let result = (|| {
            // SAFETY: each call is the driver function `cuda.h` names, with
            // the arguments it documents; every buffer is live for the call.
            unsafe {
                for (p, b) in ptrs.iter_mut().zip(bufs.iter()) {
                    if b.is_empty() {
                        continue;
                    }
                    self.check((self.api.alloc)(p, b.len()), "cuMemAlloc")?;
                    self.check(
                        (self.api.copy_in)(*p, b.as_ptr() as *const c_void, b.len()),
                        "cuMemcpyHtoD",
                    )?;
                }
                let mut params: Vec<*mut c_void> =
                    ptrs.iter_mut().map(|p| p as *mut DevicePtr as *mut c_void).collect();
                params.push(args.as_ptr() as *mut c_void);
                let grid = c_uint::try_from(blocks).map_err(|_| "too many blocks".to_string())?;
                self.check(
                    (self.api.launch)(
                        f,
                        grid,
                        1,
                        1,
                        BLOCK as c_uint,
                        1,
                        1,
                        0,
                        std::ptr::null_mut(),
                        params.as_mut_ptr(),
                        std::ptr::null_mut(),
                    ),
                    "cuLaunchKernel",
                )?;
                let status = (self.api.synchronize)();
                if status != 0 {
                    let e = format!("{kernel}: {}", self.status_name(status));
                    self.broken = Some(e.clone());
                    return Err(e);
                }
                let out = &mut bufs[4];
                self.check(
                    (self.api.copy_out)(out.as_mut_ptr() as *mut c_void, ptrs[4], out.len()),
                    "cuMemcpyDtoH",
                )
            }
        })();
        for p in ptrs {
            if p != 0 {
                // SAFETY: `p` came from `cuMemAlloc` above.
                unsafe { (self.api.free)(p) };
            }
        }
        result
    }
}

impl Api {
    /// Purpose: This function looks up each driver function once.
    ///
    /// Outputs: the functions, or the name of one the driver lacks.
    fn load(lib: &libloading::Library) -> Result<Api, String> {
        /// Purpose: This function copies one function pointer out of the
        /// library.
        fn get<F: Copy>(lib: &libloading::Library, name: &str) -> Result<F, String> {
            // SAFETY: `F` is the signature `cuda.h` gives the symbol.
            unsafe { lib.get::<F>(name.as_bytes()).map(|s| *s) }
                .map_err(|_| format!("the NVIDIA driver has no {name}"))
        }
        Ok(Api {
            init: get(lib, "cuInit")?,
            device_get: get(lib, "cuDeviceGet")?,
            device_name: get(lib, "cuDeviceGetName")?,
            device_attribute: get(lib, "cuDeviceGetAttribute")?,
            primary_retain: get(lib, "cuDevicePrimaryCtxRetain")?,
            primary_release: get(lib, "cuDevicePrimaryCtxRelease_v2")?,
            set_current: get(lib, "cuCtxSetCurrent")?,
            module_load: get(lib, "cuModuleLoadDataEx")?,
            module_function: get(lib, "cuModuleGetFunction")?,
            module_unload: get(lib, "cuModuleUnload")?,
            alloc: get(lib, "cuMemAlloc_v2")?,
            free: get(lib, "cuMemFree_v2")?,
            copy_in: get(lib, "cuMemcpyHtoD_v2")?,
            copy_out: get(lib, "cuMemcpyDtoH_v2")?,
            launch: get(lib, "cuLaunchKernel")?,
            synchronize: get(lib, "cuCtxSynchronize")?,
            error_name: get(lib, "cuGetErrorName")?,
        })
    }
}
