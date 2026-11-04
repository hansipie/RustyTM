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
}

impl GpuProc {
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
}

impl GpuProcSampler {
    pub fn sample(&mut self) -> GpuProcSnap {
        let mut snap = GpuProcSnap::default();
        let mut agg: HashMap<(u32, String), GpuProc> = HashMap::new();
        let now = Instant::now();
        let mut seen: Prev = HashMap::new();
        let mut done: HashSet<(u32, u64)> = HashSet::new();
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
                let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                    snap.restricted += 1;
                    continue;
                };
                for fd in fds.flatten() {
                    let is_drm = std::fs::read_link(fd.path()).map(|p| p.starts_with("/dev/dri")).unwrap_or(false);
                    if !is_drm {
                        continue;
                    }
                    let Some(text) = read(&format!("/proc/{pid}/fdinfo/{}", fd.file_name().to_string_lossy())) else { continue };
                    let Some((id, c)) = parse_fdinfo(&text) else { continue };
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
        snap.procs = agg.into_values().collect();
        nvidia(&mut snap);
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
        for (key, label) in [("sm", "sm"), ("enc", "enc"), ("dec", "dec")] {
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
            eprintln!("{} {} {} vram={:?} shared={:?} {:?}", p.pid, p.name, p.pci, p.vram, p.shared, p.engines);
        }
        eprintln!("first={} second={} restricted={}", a.procs.len(), b.procs.len(), b.restricted);
    }
}
