//! Per-process GPU usage: dedicated VRAM, shared (GTT/system) memory and engine load.
//!
//! * AMD / Intel / nouveau: DRM client statistics in `/proc/<pid>/fdinfo/<fd>`
//!   (<https://docs.kernel.org/gpu/drm-usage-stats.html>).
//! * NVIDIA: `nvidia-smi -q -x` (memory) and `nvidia-smi pmon` (sm / enc / dec load).

use crate::util::{read, run, which};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Default, Clone, Debug)]
pub struct GpuProc {
    pub pid: u32,
    pub name: String,
    /// PCI address, normalised to `dddd:bb:dd.f` lower case (matches `GpuStat::pci`).
    pub pci: String,
    pub vram: Option<u64>,
    pub shared: Option<u64>,
    /// Engine name -> load in percent (already divided by the engine capacity).
    pub engines: Vec<(String, f32)>,
    /// Main GPU runtime detected from the loaded libraries (`CUDA`, `HIP`, `Vulkan`...), `unknown` if not readable.
    pub runtime: &'static str,
}

/// Runtimes in display order: compute stacks first, then graphics, then video.
pub const RUNTIMES: [&str; 10] = ["CUDA", "HIP", "ggml", "ONNX", "OpenCL", "Level Zero", "Vulkan", "OpenGL", "Video", "unknown"];

/// Library name fragments identifying a runtime in `/proc/<pid>/maps`.
const RUNTIME_LIBS: [(&str, &[&str]); 9] = [
    ("CUDA", &["/libcuda.so", "/libcudart", "/libnvidia-ptxjitcompiler"]),
    ("HIP", &["/libamdhip64", "/libhsa-runtime64", "/librocblas"]),
    ("ggml", &["/libggml"]),
    ("ONNX", &["/libonnxruntime"]),
    ("OpenCL", &["/libOpenCL"]),
    ("Level Zero", &["/libze_loader", "/libze_intel_gpu"]),
    ("Vulkan", &["/libvulkan"]),
    ("OpenGL", &["/libGL.so", "/libGLX", "/libEGL", "/libgbm", "/libGLdispatch"]),
    ("Video", &["/libva.so", "/libvdpau", "/libnvcuvid", "/libnvidia-encode"]),
];

/// Main runtime of a process from the text of its `/proc/<pid>/maps`.
pub fn detect_runtime(maps: &str) -> &'static str {
    RUNTIME_LIBS.iter().find(|(_, libs)| libs.iter().any(|l| maps.contains(l))).map(|r| r.0).unwrap_or("unknown")
}

/// Engine classes shown in the UI.
pub const ENGINE_CLASSES: [&str; 4] = ["3D", "Compute", "Copy", "Video"];

/// Class of a driver engine name: amdgpu (`gfx`, `compute`, `dma`, `dec`, `enc`...),
/// i915 (`render`, `copy`, `video`, `video-enhance`), xe (`rcs`, `ccs`, `bcs`, `vcs`, `vecs`), nvidia-smi (`gfx`, `compute`, `enc`, `dec`).
pub fn engine_class(name: &str) -> Option<usize> {
    let n = name.to_lowercase();
    let any = |l: &[&str]| l.iter().any(|x| n.starts_with(x));
    if any(&["gfx", "render", "rcs", "3d", "graphics"]) {
        Some(0)
    } else if any(&["compute", "ccs", "comp"]) {
        Some(1)
    } else if any(&["dma", "copy", "bcs", "sdma", "blit"]) {
        Some(2)
    } else if any(&["dec", "enc", "vcs", "vecs", "video", "jpeg", "vcn", "uvd", "vce", "ofa"]) {
        Some(3)
    } else {
        None
    }
}

pub fn runtime_rank(r: &str) -> usize {
    RUNTIMES.iter().position(|x| *x == r).unwrap_or(RUNTIMES.len())
}

impl GpuProc {
    /// Load per engine class (3D, Compute, Copy, Video), in percent.
    pub fn class_loads(&self) -> [f32; 4] {
        let mut l = [0.0f32; 4];
        for (n, v) in &self.engines {
            if let Some(i) = engine_class(n) {
                l[i] = (l[i] + v).min(100.0);
            }
        }
        l
    }

    pub fn runtime_label(&self) -> String {
        if self.runtime.is_empty() { "unknown".into() } else { self.runtime.into() }
    }

    /// Highest engine load, if any engine reports one.
    pub fn busy(&self) -> Option<f32> {
        self.engines.iter().map(|e| e.1).fold(None, |m, v| Some(m.map_or(v, |x: f32| x.max(v))))
    }
}

#[derive(Default, Clone, Debug)]
pub struct GpuProcSnap {
    pub procs: Vec<GpuProc>,
    /// Processes whose open files could not be inspected (other users, no ptrace access).
    pub restricted: usize,
}

/// `00000000:01:00.0` (nvidia-smi) and `0000:01:00.0` (sysfs) -> `0000:01:00.0`.
pub fn norm_pci(s: &str) -> String {
    let s = s.trim().to_lowercase();
    match s.split_once(':') {
        Some((dom, rest)) if dom.len() > 4 => format!("{}:{rest}", &dom[dom.len() - 4..]),
        _ => s,
    }
}

fn parse_size(v: &str) -> Option<u64> {
    let mut it = v.split_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    Some(match it.next() {
        Some("KiB") => n << 10,
        Some("MiB") => n << 20,
        Some("GiB") => n << 30,
        _ => n,
    })
}

#[derive(Default)]
struct Client {
    pci: String,
    vram: Option<u64>,
    shared: Option<u64>,
    /// engine -> busy time in ns, or cycles
    engines: HashMap<String, u64>,
    capacity: HashMap<String, f32>,
    /// xe: engine -> total cycles of the engine class
    total_cycles: HashMap<String, u64>,
}

fn parse_fdinfo(text: &str) -> Option<(u64, Client)> {
    let mut c = Client::default();
    let mut id = None;
    let mut is_drm = false;
    // Prefer the generic `drm-resident-*` keys over the old amdgpu `drm-memory-*` alias.
    let mut vram_res = None;
    let mut shared_res = None;
    let (mut vram_old, mut shared_old) = (None, None);
    for l in text.lines() {
        let Some((k, v)) = l.split_once(':') else { continue };
        let v = v.trim();
        match k {
            "drm-driver" => is_drm = true,
            "drm-client-id" => id = v.parse().ok(),
            "drm-pdev" => c.pci = norm_pci(v),
            _ => {
                if let Some(r) = k.strip_prefix("drm-resident-") {
                    let n = parse_size(v).unwrap_or(0);
                    if r.starts_with("vram") {
                        *vram_res.get_or_insert(0) += n;
                    } else if r == "gtt" || r == "system" {
                        *shared_res.get_or_insert(0) += n;
                    }
                } else if let Some(r) = k.strip_prefix("drm-memory-") {
                    let n = parse_size(v).unwrap_or(0);
                    if r.starts_with("vram") {
                        *vram_old.get_or_insert(0) += n;
                    } else if r == "gtt" || r == "system" {
                        *shared_old.get_or_insert(0) += n;
                    }
                } else if let Some(e) = k.strip_prefix("drm-engine-capacity-") {
                    if let Ok(x) = v.parse::<f32>() {
                        c.capacity.insert(e.into(), x.max(1.0));
                    }
                } else if let Some(e) = k.strip_prefix("drm-engine-") {
                    if let Some(n) = v.split_whitespace().next().and_then(|x| x.parse().ok()) {
                        c.engines.insert(e.into(), n);
                    }
                } else if let Some(e) = k.strip_prefix("drm-total-cycles-") {
                    if let Some(n) = v.split_whitespace().next().and_then(|x| x.parse().ok()) {
                        c.total_cycles.insert(e.into(), n);
                    }
                } else if let Some(e) = k.strip_prefix("drm-cycles-") {
                    if let Some(n) = v.split_whitespace().next().and_then(|x| x.parse().ok()) {
                        c.engines.insert(format!("cycles:{e}"), n);
                    }
                }
            }
        }
    }
    if !is_drm {
        return None;
    }
    c.vram = vram_res.or(vram_old);
    c.shared = shared_res.or(shared_old);
    Some((id?, c))
}

/// Previous counters of one DRM client: engine -> (value, total cycles).
type Prev = HashMap<(u32, u64), (Instant, HashMap<String, (u64, u64)>)>;

#[derive(Default)]
pub struct GpuProcSampler {
    prev: Prev,
    runtimes: HashMap<u32, &'static str>,
    /// PIDs that held a DRM descriptor at the last full scan; rescanned every cycle.
    drm_pids: HashSet<u32>,
    /// Every PID listed in /proc at the last full scan: a PID missing from it is new and is scanned every pass.
    known_pids: HashSet<u32>,
    cycle: u32,
    restricted: usize,
}

/// A full scan of every process descriptor happens every this many cycles; in between only known DRM clients are read.
const FULL_SCAN_EVERY: u32 = 5;

impl GpuProcSampler {
    /// Call when sampling restarts after a pause: forget the rate baselines (they would be averaged over
    /// the whole pause) and make the next pass a full discovery scan.
    pub fn resume(&mut self) {
        self.prev.clear();
        self.cycle = 0;
    }

    pub fn sample(&mut self) -> GpuProcSnap {
        let mut snap = GpuProcSnap::default();
        let mut agg: HashMap<(u32, String), GpuProc> = HashMap::new();
        let now = Instant::now();
        let mut seen: Prev = HashMap::new();
        let mut done: HashSet<(u32, u64)> = HashSet::new();
        let full = self.cycle % FULL_SCAN_EVERY == 0;
        self.cycle = self.cycle.wrapping_add(1);
        let mut restricted = 0;
        let mut drm_pids: HashSet<u32> = HashSet::new();
        let mut alive: HashSet<u32> = HashSet::with_capacity(self.known_pids.len() + 16);
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
                alive.insert(pid);
                // between full scans: known DRM clients, plus processes never seen before (catches short-lived clients)
                if !full && !self.drm_pids.contains(&pid) && self.known_pids.contains(&pid) {
                    continue;
                }
                let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                    restricted += 1;
                    continue;
                };
                for fd in fds.flatten() {
                    let is_drm = std::fs::read_link(fd.path()).map(|p| p.starts_with("/dev/dri")).unwrap_or(false);
                    if !is_drm {
                        continue;
                    }
                    let Some(text) = read(&format!("/proc/{pid}/fdinfo/{}", fd.file_name().to_string_lossy())) else { continue };
                    let Some((id, c)) = parse_fdinfo(&text) else { continue };
                    drm_pids.insert(pid);
                    if !done.insert((pid, id)) {
                        continue; // same DRM client reached through several descriptors
                    }
                    let p = agg.entry((pid, c.pci.clone())).or_insert_with(|| GpuProc {
                        pid,
                        name: read(&format!("/proc/{pid}/comm")).unwrap_or_default(),
                        pci: c.pci.clone(),
                        ..Default::default()
                    });
                    if let Some(v) = c.vram {
                        *p.vram.get_or_insert(0) += v;
                    }
                    if let Some(v) = c.shared {
                        *p.shared.get_or_insert(0) += v;
                    }
                    let cur: HashMap<String, (u64, u64)> = c
                        .engines
                        .iter()
                        .map(|(k, &v)| {
                            let tot = k.strip_prefix("cycles:").and_then(|n| c.total_cycles.get(n)).copied().unwrap_or(0);
                            (k.clone(), (v, tot))
                        })
                        .collect();
                    if let Some((t0, old)) = self.prev.get(&(pid, id)) {
                        let dt_ns = now.duration_since(*t0).as_nanos() as f64;
                        for (k, &(v, tot)) in &cur {
                            let Some(&(v0, tot0)) = old.get(k) else { continue };
                            let (name, pct) = if let Some(n) = k.strip_prefix("cycles:") {
                                let dtot = tot.saturating_sub(tot0) as f64;
                                let cap = c.capacity.get(n).copied().unwrap_or(1.0) as f64;
                                (n.to_string(), if dtot > 0.0 { v.saturating_sub(v0) as f64 / dtot * 100.0 / cap } else { 0.0 })
                            } else {
                                let cap = c.capacity.get(k).copied().unwrap_or(1.0) as f64;
                                (k.clone(), if dt_ns > 0.0 { v.saturating_sub(v0) as f64 / dt_ns * 100.0 / cap } else { 0.0 })
                            };
                            let pct = pct.clamp(0.0, 100.0) as f32;
                            match p.engines.iter_mut().find(|e| e.0 == name) {
                                Some(e) => e.1 = (e.1 + pct).min(100.0),
                                None => p.engines.push((name, pct)),
                            }
                        }
                    }
                    seen.insert((pid, id), (now, cur));
                }
            }
        }
        self.prev = seen;
        if full {
            self.restricted = restricted;
        }
        snap.restricted = self.restricted;
        // only full scans mark PIDs as known: a process started since then is re-read every pass
        // until the next full scan, so one that opens the GPU a moment after starting is still caught
        if full {
            self.known_pids = alive;
        }
        self.drm_pids = if full { drm_pids } else { self.drm_pids.union(&drm_pids).copied().collect() };
        snap.procs = agg.into_values().collect();
        nvidia(&mut snap);
        // runtime from the loaded libraries; /proc/<pid>/maps is read once per process
        let mut rt = HashMap::new();
        for p in &mut snap.procs {
            let r = *rt.entry(p.pid).or_insert_with(|| {
                self.runtimes.get(&p.pid).copied().unwrap_or_else(|| read(&format!("/proc/{}/maps", p.pid)).map(|m| detect_runtime(&m)).unwrap_or("unknown"))
            });
            p.runtime = r;
        }
        self.runtimes = rt;
        snap.procs.sort_by(|a, b| b.vram.unwrap_or(0).cmp(&a.vram.unwrap_or(0)).then(b.busy().unwrap_or(0.0).total_cmp(&a.busy().unwrap_or(0.0))));
        snap
    }
}

fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let a = s.find(&open)? + open.len();
    let b = s[a..].find(&format!("</{name}>"))? + a;
    Some(s[a..b].trim())
}

/// NVIDIA processes: memory from the XML report, engine load from `pmon`.
fn nvidia(snap: &mut GpuProcSnap) {
    if !which("nvidia-smi") {
        return;
    }
    let Some(xml) = run("nvidia-smi", &["-q", "-x"]) else { return };
    let mut pcis: Vec<String> = Vec::new();
    let mut idx: HashMap<(u32, String), usize> = HashMap::new();
    for g in xml.split("<gpu id=\"").skip(1) {
        let Some(id) = g.split('"').next() else { continue };
        let pci = norm_pci(id);
        pcis.push(pci.clone());
        for pi in g.split("<process_info>").skip(1) {
            let pi = pi.split("</process_info>").next().unwrap_or("");
            let Some(pid) = tag(pi, "pid").and_then(|p| p.parse::<u32>().ok()) else { continue };
            let mem = tag(pi, "used_memory").and_then(|m| m.strip_suffix("MiB")).and_then(|m| m.trim().parse::<u64>().ok()).map(|m| m << 20);
            let name = read(&format!("/proc/{pid}/comm")).or_else(|| tag(pi, "process_name").map(|n| n.rsplit('/').next().unwrap_or(n).to_string())).unwrap_or_default();
            // a PID listed as both compute and graphics: keep the larger value, never the sum
            match idx.get(&(pid, pci.clone())) {
                Some(&i) => {
                    let p: &mut GpuProc = &mut snap.procs[i];
                    p.vram = p.vram.max(mem);
                }
                None => {
                    idx.insert((pid, pci.clone()), snap.procs.len());
                    snap.procs.push(GpuProc { pid, name, pci: pci.clone(), vram: mem, ..Default::default() });
                }
            }
        }
    }
    let Some(pm) = run("nvidia-smi", &["pmon", "-c", "1", "-s", "u"]) else { return };
    let mut cols: Vec<String> = Vec::new();
    for l in pm.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if let Some(h) = l.strip_prefix('#') {
            if h.contains("pid") {
                cols = h.split_whitespace().map(String::from).collect();
            }
            continue;
        }
        if cols.is_empty() || f.len() < cols.len() {
            continue;
        }
        let col = |n: &str| cols.iter().position(|c| c == n).and_then(|i| f.get(i).copied());
        let (Some(g), Some(pid)) = (col("gpu").and_then(|x| x.parse::<usize>().ok()), col("pid").and_then(|x| x.parse::<u32>().ok())) else { continue };
        let Some(pci) = pcis.get(g) else { continue };
        let Some(&i) = idx.get(&(pid, pci.clone())) else { continue };
        let ty = col("type").unwrap_or("G");
        let sm = if ty.contains('C') && !ty.contains('G') { "compute" } else { "gfx" };
        for (key, label) in [("sm", sm), ("enc", "enc"), ("dec", "dec")] {
            if let Some(v) = col(key).and_then(|x| x.parse::<f32>().ok()) {
                snap.procs[i].engines.push((label.into(), v));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pci_is_normalised() {
        assert_eq!(norm_pci("00000000:01:00.0"), "0000:01:00.0");
        assert_eq!(norm_pci("0000:06:00.0"), "0000:06:00.0");
    }

    #[test]
    fn amdgpu_fdinfo() {
        let t = "drm-driver:\tamdgpu\ndrm-client-id:\t53\ndrm-pdev:\t0000:06:00.0\ndrm-resident-vram:\t12 KiB\ndrm-resident-gtt:\t2 MiB\ndrm-memory-vram:\t12 KiB\ndrm-memory-gtt: \t2048 KiB\ndrm-engine-gfx:\t1000 ns\ndrm-engine-capacity-gfx:\t2\n";
        let (id, c) = parse_fdinfo(t).unwrap();
        assert_eq!(id, 53);
        assert_eq!(c.pci, "0000:06:00.0");
        assert_eq!(c.vram, Some(12 << 10)); // alias not counted twice
        assert_eq!(c.shared, Some(2 << 20));
        assert_eq!(c.engines["gfx"], 1000);
        assert_eq!(c.capacity["gfx"], 2.0);
    }

    #[test]
    fn runtime_detection() {
        assert_eq!(detect_runtime("7f00 r-xp /usr/lib64/libcuda.so.1\n7f01 r-xp /usr/lib64/libvulkan.so.1"), "CUDA");
        assert_eq!(detect_runtime("7f01 r-xp /usr/lib64/libvulkan.so.1"), "Vulkan");
        assert_eq!(detect_runtime("7f01 r-xp /usr/lib64/libamdhip64.so.6"), "HIP");
        assert_eq!(detect_runtime("7f01 r-xp /usr/lib64/libc.so.6"), "unknown");
        assert!(runtime_rank("CUDA") < runtime_rank("Vulkan"));
    }

    #[test]
    fn engine_classes() {
        for (n, c) in [("gfx", 0), ("render", 0), ("rcs", 0), ("compute", 1), ("ccs", 1), ("dma", 2), ("copy", 2), ("bcs", 2), ("dec", 3), ("enc", 3), ("vcs", 3), ("video-enhance", 3), ("vecs", 3)] {
            assert_eq!(engine_class(n), Some(c), "{n}");
        }
        assert_eq!(engine_class("weird"), None);
        let p = GpuProc { engines: vec![("gfx".into(), 60.0), ("render".into(), 70.0), ("dec".into(), 5.0)], ..Default::default() };
        assert_eq!(p.class_loads(), [100.0, 0.0, 0.0, 5.0]);
    }

    #[test]
    fn non_drm_fdinfo_is_ignored() {
        assert!(parse_fdinfo("pos:\t0\nflags:\t02\n").is_none());
    }

    #[test]
    fn live_sample() {
        let mut s = GpuProcSampler::default();
        let a = s.sample();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let b = s.sample();
        for p in &b.procs {
            eprintln!("{} {} {} {} vram={:?} shared={:?} {:?}", p.runtime, p.pid, p.name, p.pci, p.vram, p.shared, p.engines);
        }
        eprintln!("first={} second={} restricted={}", a.procs.len(), b.procs.len(), b.restricted);
    }
}
