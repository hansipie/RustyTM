use crate::collect::system::free_space;
use crate::util::data_dir;
use serde::{Deserialize, Serialize};
use std::hint::black_box;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod gpu;

// Reference figures the score is baselined to (score 100). They are this tool's own
// yardstick: scores are comparable between runs of this program only.
const REF_SINGLE_MOPS: f64 = 900.0;
const REF_MULTI_GOPS: f64 = 6.0;
const REF_DISK_READ_MBS: f64 = 1500.0;
const REF_DISK_WRITE_MBS: f64 = 1200.0;
const REF_GPU_GFLOPS: f64 = 5000.0;
const REF_GPU_MEM_GBS: f64 = 200.0;

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct BenchResult {
    pub cpu_single_mops: Option<f64>,
    pub cpu_multi_gops: Option<f64>,
    pub disk_read_mbs: Option<f64>,
    pub disk_write_mbs: Option<f64>,
    pub net_down_mbps: Option<f64>,
    pub net_up_mbps: Option<f64>,
    pub gpu_gflops: Option<f64>,
    pub gpu_mem_gbs: Option<f64>,
    #[serde(default)]
    pub gpu_device: String,
    pub score: Option<f64>,
    pub at: u64,
    pub note: String,
}

impl BenchResult {
    pub fn compute_score(&mut self) {
        let ratios: Vec<f64> = [
            self.cpu_single_mops.map(|v| v / REF_SINGLE_MOPS),
            self.cpu_multi_gops.map(|v| v / REF_MULTI_GOPS),
            self.disk_read_mbs.map(|v| v / REF_DISK_READ_MBS),
            self.disk_write_mbs.map(|v| v / REF_DISK_WRITE_MBS),
            self.gpu_gflops.map(|v| v / REF_GPU_GFLOPS),
            self.gpu_mem_gbs.map(|v| v / REF_GPU_MEM_GBS),
        ]
        .into_iter()
        .flatten()
        .filter(|r| *r > 0.0)
        .collect();
        self.score = if ratios.is_empty() { None } else { Some(100.0 * (ratios.iter().map(|r| r.ln()).sum::<f64>() / ratios.len() as f64).exp()) };
    }
}

#[derive(Default, Clone)]
pub struct BenchState {
    pub running: bool,
    pub label: String,
    pub progress: f32,
    pub errors: Vec<String>,
    pub result: BenchResult,
    pub finished: bool,
    pub cancelled: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Cpu,
    Disk,
    Gpu,
    Internet,
    All,
}

pub struct Bench {
    pub state: Arc<Mutex<BenchState>>,
    cancel: Arc<AtomicBool>,
}

fn kernel(iters: u64) -> u64 {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut f = 1.0f64;
    for i in 0..iters {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x = x.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(i);
        f = f * 1.000_000_1 + (x & 0xff) as f64 * 1e-9;
    }
    black_box(x ^ f.to_bits())
}

const OPS_PER_ITER: f64 = 9.0;
const CHUNK: u64 = 500_000;

/// Runs the kernel on `threads` threads for ~`secs`; returns Mops/sec in total.
fn cpu_run(threads: usize, secs: f64, cancel: &AtomicBool) -> f64 {
    let start = Instant::now();
    let total: u64 = std::thread::scope(|s| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut n = 0u64;
                    while start.elapsed().as_secs_f64() < secs && !cancel.load(Ordering::Relaxed) {
                        black_box(kernel(CHUNK));
                        n += CHUNK;
                    }
                    n
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or(0)).sum()
    });
    total as f64 * OPS_PER_ITER / start.elapsed().as_secs_f64() / 1e6
}

fn disk_run(dir: &Path, st: &Mutex<BenchState>, cancel: &AtomicBool) -> Result<(f64, f64), String> {
    let free = free_space(dir).ok_or("could not determine free space")?;
    let size: u64 = (256u64 << 20).min(free / 4);
    if size < (16 << 20) {
        return Err("not enough free space in the selected folder".into());
    }
    let path = dir.join(format!(".rustytm-bench-{}.tmp", std::process::id()));
    let mut buf = vec![0u8; 1 << 20];
    let mut seed = 0x1234_5678_9abc_def0u64;
    for c in buf.chunks_mut(8) {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        c.copy_from_slice(&seed.to_le_bytes()[..c.len()]);
    }
    let setp = |label: &str, p: f32| {
        if let Ok(mut s) = st.lock() {
            s.label = label.into();
            s.progress = p;
        }
    };
    let res = (|| -> Result<(f64, f64), String> {
        setp("Writing test file", 0.0);
        let mut f = std::fs::File::create(&path).map_err(|e| format!("the benchmark file could not be created ({e})"))?;
        let t = Instant::now();
        let mut written = 0u64;
        while written < size {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            f.write_all(&buf).map_err(|_| "writing the benchmark file failed".to_string())?;
            written += buf.len() as u64;
            setp("Writing test file", written as f32 / size as f32 * 0.5);
        }
        f.sync_all().map_err(|_| "flushing the benchmark file failed".to_string())?;
        let wr = written as f64 / 1e6 / t.elapsed().as_secs_f64();
        unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        drop(f);
        let mut f = std::fs::File::open(&path).map_err(|_| "the benchmark file could not be reopened for reading".to_string())?;
        unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        setp("Reading test file", 0.5);
        let t = Instant::now();
        let mut read = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let n = f.read(&mut buf).map_err(|_| "reading the benchmark file failed".to_string())?;
            if n == 0 {
                break;
            }
            read += n as u64;
            setp("Reading test file", 0.5 + read as f32 / size as f32 * 0.5);
        }
        let rd = read as f64 / 1e6 / t.elapsed().as_secs_f64();
        Ok((rd, wr))
    })();
    let _ = std::fs::remove_file(&path);
    res
}

fn net_run(st: &Mutex<BenchState>, cancel: &AtomicBool) -> Result<(f64, f64), String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(40)).user_agent("rustytm-internet-benchmark").build();
    let setp = |label: &str, p: f32| {
        if let Ok(mut s) = st.lock() {
            s.label = label.into();
            s.progress = p;
        }
    };
    setp("Testing download speed", 0.0);
    let t = Instant::now();
    let resp = agent.get("https://speed.cloudflare.com/__down?bytes=25000000").call().map_err(|e| e.to_string())?;
    let mut r = resp.into_reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        got += n as u64;
        setp("Testing download speed", got as f32 / 25_000_000.0 * 0.5);
    }
    if got == 0 {
        return Err("The download test received no usable data.".into());
    }
    let down = got as f64 * 8.0 / t.elapsed().as_secs_f64() / 1e6;
    setp("Testing upload speed", 0.5);
    let body = vec![0u8; 10_000_000];
    let t = Instant::now();
    agent.post("https://speed.cloudflare.com/__up").send_bytes(&body).map_err(|e| e.to_string())?;
    let up = body.len() as f64 * 8.0 / t.elapsed().as_secs_f64() / 1e6;
    Ok((down, up))
}

impl Bench {
    pub fn new() -> Self {
        Bench { state: Arc::new(Mutex::new(BenchState::default())), cancel: Arc::new(AtomicBool::new(false)) }
    }
    pub fn snapshot(&self) -> BenchState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }
    pub fn running(&self) -> bool {
        self.state.lock().map(|s| s.running).unwrap_or(false)
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn start(&self, kind: Kind, dir: PathBuf) {
        if self.running() {
            return;
        }
        self.cancel.store(false, Ordering::SeqCst);
        *self.state.lock().unwrap() = BenchState { running: true, ..Default::default() };
        let (st, cancel) = (self.state.clone(), self.cancel.clone());
        std::thread::spawn(move || {
            let set = |l: &str, p: f32| {
                if let Ok(mut s) = st.lock() {
                    s.label = l.into();
                    s.progress = p;
                }
            };
            let err = |e: String| {
                if let Ok(mut s) = st.lock() {
                    if e != "cancelled" {
                        s.errors.push(e);
                    }
                }
            };
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
            let mut r = BenchResult { at: now(), ..Default::default() };
            if matches!(kind, Kind::Cpu | Kind::All) && !cancel.load(Ordering::SeqCst) {
                set("Testing one CPU core", 0.0);
                r.cpu_single_mops = Some(cpu_run(1, 3.0, &cancel));
                set("Testing all CPU cores", 0.5);
                r.cpu_multi_gops = Some(cpu_run(threads, 3.0, &cancel) / 1000.0);
            }
            if matches!(kind, Kind::Disk | Kind::All) && !cancel.load(Ordering::SeqCst) {
                set("Checking free space", 0.0);
                match disk_run(&dir, &st, &cancel) {
                    Ok((rd, wr)) => {
                        r.disk_read_mbs = Some(rd);
                        r.disk_write_mbs = Some(wr);
                    }
                    Err(e) => err(format!("Disk test failed: {e}")),
                }
            }
            if matches!(kind, Kind::Gpu | Kind::All) && !cancel.load(Ordering::SeqCst) {
                match gpu::run(3.0, &cancel, &set) {
                    Ok(g) => {
                        r.gpu_gflops = Some(g.gflops);
                        r.gpu_mem_gbs = Some(g.mem_gbs);
                        r.gpu_device = g.device;
                    }
                    Err(e) => err(format!("GPU test failed: {e}")),
                }
            }
            if matches!(kind, Kind::Internet | Kind::All) && !cancel.load(Ordering::SeqCst) {
                match net_run(&st, &cancel) {
                    Ok((d, u)) => {
                        r.net_down_mbps = Some(d);
                        r.net_up_mbps = Some(u);
                    }
                    Err(e) => err(format!("Internet test failed: {e}")),
                }
            }
            r.compute_score();
            let cancelled = cancel.load(Ordering::SeqCst);
            if !cancelled {
                let _ = append_history(&r);
            }
            if let Ok(mut s) = st.lock() {
                s.result = r;
                s.running = false;
                s.finished = !cancelled;
                s.cancelled = cancelled;
                s.progress = 1.0;
                s.label = if cancelled { "Cancelled".into() } else { "Complete".into() };
            }
        });
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn history_path() -> PathBuf {
    data_dir().join("benchmark-history.json")
}

pub fn load_history() -> Vec<BenchResult> {
    std::fs::read_to_string(history_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn append_history(r: &BenchResult) -> Result<(), String> {
    let mut h = load_history();
    h.push(r.clone());
    save_history(&h)
}

pub fn save_history(h: &[BenchResult]) -> Result<(), String> {
    let p = history_path();
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(h).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(tmp, p).map_err(|e| e.to_string())
}
