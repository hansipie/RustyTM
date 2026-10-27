//! GPU benchmark over OpenCL. The ICD loader (libOpenCL.so.1) is opened at run time with
//! dlopen, so the program has no build or link dependency on any GPU stack: without an
//! OpenCL driver the test simply reports that none was found. OpenCL takes kernels as
//! source text and needs no display server, which suits a terminal program.

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

type Id = *mut c_void;
type ClInt = i32;

const CL_SUCCESS: ClInt = 0;
const CL_DEVICE_TYPE_GPU: u64 = 1 << 2;
const CL_DEVICE_MAX_COMPUTE_UNITS: u32 = 0x1002;
const CL_DEVICE_MAX_CLOCK_FREQUENCY: u32 = 0x100C;
const CL_DEVICE_MAX_MEM_ALLOC_SIZE: u32 = 0x1010;
const CL_DEVICE_GLOBAL_MEM_SIZE: u32 = 0x101F;
const CL_DEVICE_NAME: u32 = 0x102B;
const CL_DEVICE_HOST_UNIFIED_MEMORY: u32 = 0x1035;
const CL_PROGRAM_BUILD_LOG: u32 = 0x1183;
const CL_MEM_READ_WRITE: u64 = 1;

// Each work item runs 8 independent float4 FMA chains (enough to hide ALU latency) for
// ITERS rounds; `a` and `b` are kernel arguments so the compiler cannot fold the loop.
const ITERS: u64 = 256;
const FLOPS_PER_ITEM: f64 = (8 * 4 * 2 * ITERS) as f64;
const FLOPS_ITEMS: usize = 1 << 20;
const SRC: &str = "
__kernel void flops(__global float *out, float a, float b) {
    float x = (float)get_global_id(0) * 1e-9f;
    float4 v0 = (float4)(x, x + 0.1f, x + 0.2f, x + 0.3f);
    float4 v1 = v0 + 1.0f, v2 = v0 + 2.0f, v3 = v0 + 3.0f, v4 = v0 + 4.0f, v5 = v0 + 5.0f, v6 = v0 + 6.0f, v7 = v0 + 7.0f;
    for (int i = 0; i < ITERS; i++) {
        v0 = mad(v0, a, b); v1 = mad(v1, a, b); v2 = mad(v2, a, b); v3 = mad(v3, a, b);
        v4 = mad(v4, a, b); v5 = mad(v5, a, b); v6 = mad(v6, a, b); v7 = mad(v7, a, b);
    }
    float4 s = v0 + v1 + v2 + v3 + v4 + v5 + v6 + v7;
    out[get_global_id(0)] = s.x + s.y + s.z + s.w;
}
__kernel void copy(__global const float4 *src, __global float4 *dst) {
    size_t i = get_global_id(0);
    dst[i] = src[i];
}
";

#[derive(Default, Debug)]
pub struct GpuRun {
    pub device: String,
    pub gflops: f64,
    pub mem_gbs: f64,
}

/// The OpenCL entry points used here, resolved from the loader.
struct Cl {
    get_platform_ids: unsafe extern "C" fn(u32, *mut Id, *mut u32) -> ClInt,
    get_device_ids: unsafe extern "C" fn(Id, u64, u32, *mut Id, *mut u32) -> ClInt,
    get_device_info: unsafe extern "C" fn(Id, u32, usize, *mut c_void, *mut usize) -> ClInt,
    create_context: unsafe extern "C" fn(*const isize, u32, *const Id, *const c_void, *mut c_void, *mut ClInt) -> Id,
    create_command_queue: unsafe extern "C" fn(Id, Id, u64, *mut ClInt) -> Id,
    create_program_with_source: unsafe extern "C" fn(Id, u32, *const *const c_char, *const usize, *mut ClInt) -> Id,
    build_program: unsafe extern "C" fn(Id, u32, *const Id, *const c_char, *const c_void, *mut c_void) -> ClInt,
    get_program_build_info: unsafe extern "C" fn(Id, Id, u32, usize, *mut c_void, *mut usize) -> ClInt,
    create_kernel: unsafe extern "C" fn(Id, *const c_char, *mut ClInt) -> Id,
    create_buffer: unsafe extern "C" fn(Id, u64, usize, *mut c_void, *mut ClInt) -> Id,
    set_kernel_arg: unsafe extern "C" fn(Id, u32, usize, *const c_void) -> ClInt,
    enqueue_nd_range_kernel: unsafe extern "C" fn(Id, Id, u32, *const usize, *const usize, *const usize, u32, *const Id, *mut Id) -> ClInt,
    finish: unsafe extern "C" fn(Id) -> ClInt,
    release_mem_object: unsafe extern "C" fn(Id) -> ClInt,
    release_kernel: unsafe extern "C" fn(Id) -> ClInt,
    release_program: unsafe extern "C" fn(Id) -> ClInt,
    release_command_queue: unsafe extern "C" fn(Id) -> ClInt,
    release_context: unsafe extern "C" fn(Id) -> ClInt,
}

impl Cl {
    fn load() -> Result<Cl, String> {
        // The handle is never closed: some vendor drivers leave threads behind that crash on unload.
        let lib = [c"libOpenCL.so.1", c"libOpenCL.so"].iter().map(|n| unsafe { libc::dlopen(n.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) }).find(|h| !h.is_null());
        let lib = lib.ok_or("no OpenCL runtime found (install your GPU vendor's OpenCL driver or Mesa rusticl)")?;
        macro_rules! sym {
            ($n:literal) => {{
                let p = unsafe { libc::dlsym(lib, concat!($n, "\0").as_ptr() as *const c_char) };
                if p.is_null() {
                    return Err(format!("the OpenCL runtime lacks {}", $n));
                }
                // the target type is the struct field's fn pointer type
                #[allow(clippy::missing_transmute_annotations)]
                unsafe { std::mem::transmute::<*mut c_void, _>(p) }
            }};
        }
        Ok(Cl {
            get_platform_ids: sym!("clGetPlatformIDs"),
            get_device_ids: sym!("clGetDeviceIDs"),
            get_device_info: sym!("clGetDeviceInfo"),
            create_context: sym!("clCreateContext"),
            create_command_queue: sym!("clCreateCommandQueue"),
            create_program_with_source: sym!("clCreateProgramWithSource"),
            build_program: sym!("clBuildProgram"),
            get_program_build_info: sym!("clGetProgramBuildInfo"),
            create_kernel: sym!("clCreateKernel"),
            create_buffer: sym!("clCreateBuffer"),
            set_kernel_arg: sym!("clSetKernelArg"),
            enqueue_nd_range_kernel: sym!("clEnqueueNDRangeKernel"),
            finish: sym!("clFinish"),
            release_mem_object: sym!("clReleaseMemObject"),
            release_kernel: sym!("clReleaseKernel"),
            release_program: sym!("clReleaseProgram"),
            release_command_queue: sym!("clReleaseCommandQueue"),
            release_context: sym!("clReleaseContext"),
        })
    }

    fn info<T: Default>(&self, dev: Id, param: u32) -> T {
        let mut v = T::default();
        unsafe { (self.get_device_info)(dev, param, std::mem::size_of::<T>(), &mut v as *mut T as *mut c_void, std::ptr::null_mut()) };
        v
    }

    fn info_str(&self, dev: Id, param: u32) -> String {
        let mut buf = [0u8; 256];
        unsafe { (self.get_device_info)(dev, param, buf.len(), buf.as_mut_ptr() as *mut c_void, std::ptr::null_mut()) };
        CStr::from_bytes_until_nul(&buf).map(|s| s.to_string_lossy().trim().to_string()).unwrap_or_default()
    }

    /// Every GPU device of every platform; the first one is the one benchmarked: discrete
    /// before integrated, then the largest compute units × clock.
    fn gpus(&self) -> Vec<Id> {
        let mut np = 0u32;
        let mut plats = [std::ptr::null_mut(); 16];
        unsafe { (self.get_platform_ids)(plats.len() as u32, plats.as_mut_ptr(), &mut np) };
        let mut devs = Vec::new();
        for &p in &plats[..(np as usize).min(plats.len())] {
            let mut nd = 0u32;
            let mut ds = [std::ptr::null_mut(); 16];
            if unsafe { (self.get_device_ids)(p, CL_DEVICE_TYPE_GPU, ds.len() as u32, ds.as_mut_ptr(), &mut nd) } == CL_SUCCESS {
                devs.extend_from_slice(&ds[..(nd as usize).min(ds.len())]);
            }
        }
        devs.sort_by_key(|&d| {
            let unified = self.info::<u32>(d, CL_DEVICE_HOST_UNIFIED_MEMORY) != 0;
            (unified, std::cmp::Reverse(self.info::<u32>(d, CL_DEVICE_MAX_COMPUTE_UNITS) as u64 * self.info::<u32>(d, CL_DEVICE_MAX_CLOCK_FREQUENCY) as u64))
        });
        devs
    }
}

/// OpenCL objects created during a run, released in reverse order whatever the outcome.
struct Objs<'a> {
    cl: &'a Cl,
    ctx: Id,
    queue: Id,
    prog: Id,
    kernels: Vec<Id>,
    mems: Vec<Id>,
}

impl Drop for Objs<'_> {
    fn drop(&mut self) {
        unsafe {
            if !self.queue.is_null() {
                (self.cl.finish)(self.queue);
            }
            for &m in &self.mems {
                (self.cl.release_mem_object)(m);
            }
            for &k in &self.kernels {
                (self.cl.release_kernel)(k);
            }
            if !self.prog.is_null() {
                (self.cl.release_program)(self.prog);
            }
            if !self.queue.is_null() {
                (self.cl.release_command_queue)(self.queue);
            }
            if !self.ctx.is_null() {
                (self.cl.release_context)(self.ctx);
            }
        }
    }
}

fn check(what: &str, code: ClInt) -> Result<(), String> {
    if code == CL_SUCCESS { Ok(()) } else { Err(format!("{what} failed (OpenCL error {code})")) }
}

fn made(what: &str, id: Id, code: ClInt) -> Result<Id, String> {
    check(what, code)?;
    if id.is_null() { Err(format!("{what} failed")) } else { Ok(id) }
}

fn buffer(o: &mut Objs, size: usize) -> Result<Id, String> {
    let mut e = 0;
    let m = made("allocating GPU memory", unsafe { (o.cl.create_buffer)(o.ctx, CL_MEM_READ_WRITE, size, std::ptr::null_mut(), &mut e) }, e)?;
    o.mems.push(m);
    Ok(m)
}

fn arg<T>(cl: &Cl, k: Id, i: u32, v: &T) -> Result<(), String> {
    check("setting a kernel argument", unsafe { (cl.set_kernel_arg)(k, i, std::mem::size_of::<T>(), v as *const T as *const c_void) })
}

/// Runs `kernel` over `items` work items for ~`secs` after one warm-up dispatch; dispatches
/// are queued in batches grown until a batch takes ≥ 50 ms, so the per-sync cost stays
/// negligible. Returns dispatches per second.
fn timed(cl: &Cl, q: Id, kernel: Id, items: usize, secs: f64, cancel: &AtomicBool, prog: &dyn Fn(f32)) -> Result<f64, String> {
    let launch = |n: u32| -> Result<(), String> {
        for _ in 0..n {
            check("launching a kernel", unsafe { (cl.enqueue_nd_range_kernel)(q, kernel, 1, std::ptr::null(), &items, std::ptr::null(), 0, std::ptr::null(), std::ptr::null_mut()) })?;
        }
        check("waiting for the GPU", unsafe { (cl.finish)(q) })
    };
    launch(1)?;
    let (mut batch, mut done, mut spent) = (1u32, 0u64, 0f64);
    while spent < secs {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let t = Instant::now();
        launch(batch)?;
        let dt = t.elapsed().as_secs_f64();
        done += batch as u64;
        spent += dt;
        if dt < 0.05 && batch < 1 << 16 {
            batch *= 2;
        }
        prog((spent / secs).min(1.0) as f32);
    }
    Ok(done as f64 / spent)
}

/// Measures FP32 throughput (GFLOPS, FMA counted as two operations) and on-device memory
/// copy bandwidth (GB/sec, bytes read + bytes written) of the strongest OpenCL GPU.
pub fn run(secs: f64, cancel: &AtomicBool, set: &dyn Fn(&str, f32)) -> Result<GpuRun, String> {
    set("Looking for an OpenCL GPU", 0.0);
    let cl = Cl::load()?;
    let dev = *cl.gpus().first().ok_or("no OpenCL GPU device found (is the GPU vendor's OpenCL driver installed?)")?;
    let device = cl.info_str(dev, CL_DEVICE_NAME);
    let mut e = 0;
    let mut o = Objs { cl: &cl, ctx: std::ptr::null_mut(), queue: std::ptr::null_mut(), prog: std::ptr::null_mut(), kernels: vec![], mems: vec![] };
    o.ctx = made("creating the OpenCL context", unsafe { (cl.create_context)(std::ptr::null(), 1, &dev, std::ptr::null(), std::ptr::null_mut(), &mut e) }, e)?;
    o.queue = made("creating the command queue", unsafe { (cl.create_command_queue)(o.ctx, dev, 0, &mut e) }, e)?;
    set("Compiling GPU kernels", 0.0);
    let (src, len) = (SRC.as_ptr() as *const c_char, SRC.len());
    o.prog = made("loading the kernels", unsafe { (cl.create_program_with_source)(o.ctx, 1, &src, &len, &mut e) }, e)?;
    let opts = std::ffi::CString::new(format!("-D ITERS={ITERS}")).unwrap_or_default();
    if unsafe { (cl.build_program)(o.prog, 1, &dev, opts.as_ptr(), std::ptr::null(), std::ptr::null_mut()) } != CL_SUCCESS {
        let mut log = vec![0u8; 4096];
        unsafe { (cl.get_program_build_info)(o.prog, dev, CL_PROGRAM_BUILD_LOG, log.len(), log.as_mut_ptr() as *mut c_void, std::ptr::null_mut()) };
        let log = CStr::from_bytes_until_nul(&log).map(|s| s.to_string_lossy().trim().to_string()).unwrap_or_default();
        return Err(format!("the GPU driver could not compile the test kernels: {}", log.lines().next().unwrap_or("no build log")));
    }
    let kflops = made("creating the compute kernel", unsafe { (cl.create_kernel)(o.prog, c"flops".as_ptr(), &mut e) }, e)?;
    o.kernels.push(kflops);
    let kcopy = made("creating the copy kernel", unsafe { (cl.create_kernel)(o.prog, c"copy".as_ptr(), &mut e) }, e)?;
    o.kernels.push(kcopy);

    // compute: 1 Mi work items × 16 Ki flops = 17 GFLOP per dispatch
    let out = buffer(&mut o, FLOPS_ITEMS * 4)?;
    arg(&cl, kflops, 0, &out)?;
    arg(&cl, kflops, 1, &0.999_9f32)?;
    arg(&cl, kflops, 2, &0.000_1f32)?;
    set("Testing GPU compute", 0.0);
    let rate = timed(&cl, o.queue, kflops, FLOPS_ITEMS, secs, cancel, &|p| set("Testing GPU compute", p * 0.5))?;
    let gflops = rate * FLOPS_ITEMS as f64 * FLOPS_PER_ITEM / 1e9;

    // memory: two buffers of up to 256 MiB, larger than any GPU cache so it measures VRAM
    let max_alloc = cl.info::<u64>(dev, CL_DEVICE_MAX_MEM_ALLOC_SIZE);
    let global = cl.info::<u64>(dev, CL_DEVICE_GLOBAL_MEM_SIZE);
    let size = (256u64 << 20).min(max_alloc).min(global / 4) & !0xffff;
    if size < 16 << 20 {
        return Err("the GPU has too little memory for the bandwidth test".into());
    }
    let (src_m, dst_m) = (buffer(&mut o, size as usize)?, buffer(&mut o, size as usize)?);
    arg(&cl, kcopy, 0, &src_m)?;
    arg(&cl, kcopy, 1, &dst_m)?;
    set("Testing GPU memory", 0.5);
    let rate = timed(&cl, o.queue, kcopy, size as usize / 16, secs, cancel, &|p| set("Testing GPU memory", 0.5 + p * 0.5))?;
    let mem_gbs = rate * 2.0 * size as f64 / 1e9;
    Ok(GpuRun { device, gflops, mem_gbs })
}
