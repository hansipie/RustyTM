use crate::util::*;
use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

// ───────────────────────────── CPU ─────────────────────────────

#[derive(Default, Clone)]
pub struct CpuStatic {
    pub model: String,
    pub logical: usize,
    pub physical: usize,
    pub sockets: usize,
    pub caches: Vec<(String, String)>,
    pub base_mhz: Option<f64>,
    pub governor: String,
    pub driver: String,
    pub epp: String,
    pub virt: String,
    pub numa: usize,
    /// per logical CPU: true when it is an efficiency core (lower capacity than the maximum)
    pub efficiency: Vec<bool>,
}

pub fn cpu_static() -> CpuStatic {
    let info = read("/proc/cpuinfo").unwrap_or_default();
    let model = info
        .lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|x| x.1.trim().to_string())
        .unwrap_or_else(|| "Unknown processor".into());
    let flags = info.lines().find(|l| l.starts_with("flags")).unwrap_or("");
    let virt = if flags.split_whitespace().any(|f| f == "hypervisor") {
        "Virtual machine guest".to_string()
    } else if flags.split_whitespace().any(|f| f == "vmx" || f == "svm") {
        "Supported (VT-x / AMD-V)".to_string()
    } else {
        "Not available".to_string()
    };
    let logical = read("/proc/stat")
        .map(|s| s.lines().filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).map_or(false, |c| c.is_ascii_digit())).count())
        .unwrap_or(1)
        .max(1);
    let mut cores = BTreeSet::new();
    let mut pkgs = BTreeSet::new();
    let mut caps = Vec::new();
    for i in 0..logical {
        let t = format!("/sys/devices/system/cpu/cpu{i}/topology");
        let pkg = read_u64(&format!("{t}/physical_package_id")).unwrap_or(0);
        let core = read_u64(&format!("{t}/core_id")).unwrap_or(i as u64);
        cores.insert((pkg, core));
        pkgs.insert(pkg);
        caps.push(read_u64(&format!("/sys/devices/system/cpu/cpu{i}/cpu_capacity")));
    }
    let maxcap = caps.iter().flatten().max().copied();
    let efficiency = caps.iter().map(|c| matches!((c, maxcap), (Some(c), Some(m)) if *c < m)).collect();
    let mut caches: Vec<(String, String)> = Vec::new();
    for idx in 0..8 {
        let d = format!("/sys/devices/system/cpu/cpu0/cache/index{idx}");
        let (Some(level), Some(ty), Some(size)) = (read(&format!("{d}/level")), read(&format!("{d}/type")), read(&format!("{d}/size"))) else { continue };
        let tag = match ty.as_str() {
            "Data" => format!("L{level}d"),
            "Instruction" => format!("L{level}i"),
            _ => format!("L{level}"),
        };
        caches.push((tag, size));
    }
    let cf = "/sys/devices/system/cpu/cpu0/cpufreq";
    let base_mhz = read_f64(&format!("{cf}/base_frequency"))
        .or_else(|| read_f64(&format!("{cf}/cpuinfo_min_freq")).filter(|_| false))
        .map(|k| k / 1000.0)
        .or_else(|| info.lines().find(|l| l.starts_with("cpu MHz")).and_then(|l| l.split_once(':')).and_then(|x| x.1.trim().parse().ok()));
    let numa = std::fs::read_dir("/sys/devices/system/node")
        .map(|rd| rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("node")).count())
        .unwrap_or(1)
        .max(1);
    CpuStatic {
        model,
        logical,
        physical: cores.len().max(1),
        sockets: pkgs.len().max(1),
        caches,
        base_mhz,
        governor: read(&format!("{cf}/scaling_governor")).unwrap_or_else(|| "n/a".into()),
        driver: read(&format!("{cf}/scaling_driver")).unwrap_or_else(|| "n/a".into()),
        epp: read(&format!("{cf}/energy_performance_preference")).unwrap_or_else(|| "n/a".into()),
        virt,
        numa,
        efficiency,
    }
}

#[derive(Default, Clone)]
pub struct CpuUse {
    pub overall: f32,
    pub kernel: f32,
    pub per: Vec<f32>,
    pub per_kernel: Vec<f32>,
}

#[derive(Default)]
pub struct CpuSampler {
    prev: Vec<Vec<u64>>,
}

fn busy_kernel(prev: &[u64], cur: &[u64]) -> (f32, f32) {
    let d = |i: usize| cur.get(i).copied().unwrap_or(0).saturating_sub(prev.get(i).copied().unwrap_or(0)) as f64;
    // user nice system idle iowait irq softirq steal
    let total: f64 = (0..8).map(d).sum();
    if total <= 0.0 {
        return (0.0, 0.0);
    }
    let idle = d(3) + d(4);
    let kernel = d(2) + d(5) + d(6);
    ((((total - idle) / total) * 100.0) as f32, ((kernel / total) * 100.0) as f32)
}

impl CpuSampler {
    pub fn sample(&mut self) -> CpuUse {
        let txt = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        let cur: Vec<Vec<u64>> = txt
            .lines()
            .filter(|l| l.starts_with("cpu"))
            .map(|l| l.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect())
            .collect();
        let mut out = CpuUse::default();
        if self.prev.len() == cur.len() && !cur.is_empty() {
            let (b, k) = busy_kernel(&self.prev[0], &cur[0]);
            out.overall = b;
            out.kernel = k;
            for i in 1..cur.len() {
                let (b, k) = busy_kernel(&self.prev[i], &cur[i]);
                out.per.push(b);
                out.per_kernel.push(k);
            }
        } else {
            out.per = vec![0.0; cur.len().saturating_sub(1)];
            out.per_kernel = out.per.clone();
        }
        self.prev = cur;
        out
    }
}

/// Current frequency per logical CPU in MHz.
pub fn cpu_freqs(n: usize) -> Vec<f64> {
    let mut v = Vec::with_capacity(n);
    let info = read("/proc/cpuinfo").unwrap_or_default();
    let fallback: Vec<f64> = info.lines().filter(|l| l.starts_with("cpu MHz")).filter_map(|l| l.split_once(':')?.1.trim().parse().ok()).collect();
    for i in 0..n {
        let f = read_f64(&format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq")).map(|k| k / 1000.0).or_else(|| fallback.get(i).copied()).unwrap_or(0.0);
        v.push(f);
    }
    v
}

pub fn cpu_max_mhz() -> Option<f64> {
    read_f64("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq").map(|k| k / 1000.0)
}

// ───────────────────────────── Memory ─────────────────────────────

#[derive(Default, Clone)]
pub struct Mem {
    pub total: u64,
    pub available: u64,
    pub used: u64,
    pub cached: u64,
    pub buffers: u64,
    pub shared: u64,
    pub slab: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub committed: u64,
    pub commit_limit: u64,
    pub psi_some10: f32,
    pub page_in_kb: u64,
    pub page_out_kb: u64,
}

pub fn mem() -> Mem {
    let mut m: HashMap<String, u64> = HashMap::new();
    for l in read("/proc/meminfo").unwrap_or_default().lines() {
        if let Some((k, v)) = l.split_once(':') {
            let n = v.trim().trim_end_matches("kB").trim().parse::<u64>().unwrap_or(0) * 1024;
            m.insert(k.to_string(), n);
        }
    }
    let g = |k: &str| m.get(k).copied().unwrap_or(0);
    let psi = read("/proc/pressure/memory")
        .and_then(|s| s.lines().find(|l| l.starts_with("some")).map(str::to_string))
        .and_then(|l| l.split_whitespace().find(|t| t.starts_with("avg10=")).and_then(|t| t[6..].parse().ok()))
        .unwrap_or(0.0);
    let vm = read("/proc/vmstat").unwrap_or_default();
    let vmv = |k: &str| vm.lines().find_map(|l| l.strip_prefix(k).and_then(|r| r.trim().parse::<u64>().ok())).unwrap_or(0);
    Mem {
        total: g("MemTotal"),
        available: g("MemAvailable"),
        used: g("MemTotal").saturating_sub(g("MemAvailable")),
        cached: g("Cached"),
        buffers: g("Buffers"),
        shared: g("Shmem"),
        slab: g("Slab"),
        swap_total: g("SwapTotal"),
        swap_used: g("SwapTotal").saturating_sub(g("SwapFree")),
        committed: g("Committed_AS"),
        commit_limit: g("CommitLimit"),
        psi_some10: psi,
        page_in_kb: vmv("pgpgin "),
        page_out_kb: vmv("pgpgout "),
    }
}

// ───────────────────────────── Disks ─────────────────────────────

#[derive(Default, Clone)]
pub struct DiskStat {
    pub name: String,
    pub model: String,
    pub size: u64,
    pub rotational: bool,
    pub scheduler: String,
    pub read_rate: f64,
    pub write_rate: f64,
    pub active_pct: f32,
    pub resp_ms: f64,
    pub total_read: u64,
    pub total_written: u64,
    pub system: bool,
}

#[derive(Default)]
pub struct DiskSampler {
    prev: HashMap<String, (Vec<u64>, Instant)>,
}

fn root_disk() -> Option<String> {
    let mi = read("/proc/self/mountinfo")?;
    for l in mi.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.get(4) == Some(&"/") {
            let dash = f.iter().position(|x| *x == "-")?;
            let src = f.get(dash + 2)?;
            let dev = src.strip_prefix("/dev/")?;
            // resolve /dev/mapper/x and dm-N to physical parents via slaves
            let mut name = std::fs::canonicalize(format!("/dev/{dev}")).ok()?.file_name()?.to_string_lossy().into_owned();
            for _ in 0..4 {
                let slaves = format!("/sys/class/block/{name}/slaves");
                if let Some(s) = std::fs::read_dir(&slaves).ok().and_then(|mut r| r.next()).and_then(|e| e.ok()) {
                    name = s.file_name().to_string_lossy().into_owned();
                } else {
                    break;
                }
            }
            let sys = std::fs::canonicalize(format!("/sys/class/block/{name}")).ok()?;
            if sys.join("partition").exists() {
                return sys.parent()?.file_name().map(|s| s.to_string_lossy().into_owned());
            }
            return Some(name);
        }
    }
    None
}

impl DiskSampler {
    pub fn sample(&mut self) -> Vec<DiskStat> {
        let root = root_disk();
        let ds = read("/proc/diskstats").unwrap_or_default();
        let mut out = Vec::new();
        let now = Instant::now();
        for l in ds.lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 14 {
                continue;
            }
            let name = f[2];
            if ["loop", "ram", "zram", "sr", "fd", "dm-", "md"].iter().any(|p| name.starts_with(p)) {
                continue;
            }
            if !std::path::Path::new(&format!("/sys/block/{name}")).exists() {
                continue; // partition
            }
            let v: Vec<u64> = f[3..].iter().filter_map(|x| x.parse().ok()).collect();
            if v.len() < 11 {
                continue;
            }
            let mut d = DiskStat {
                name: name.to_string(),
                model: read(&format!("/sys/block/{name}/device/model")).unwrap_or_default(),
                size: read_u64(&format!("/sys/block/{name}/size")).unwrap_or(0) * 512,
                rotational: read(&format!("/sys/block/{name}/queue/rotational")).as_deref() == Some("1"),
                scheduler: read(&format!("/sys/block/{name}/queue/scheduler"))
                    .map(|s| s.split_whitespace().find(|t| t.starts_with('[')).map(|t| t.trim_matches(|c| c == '[' || c == ']').to_string()).unwrap_or(s))
                    .unwrap_or_default(),
                total_read: v[2] * 512,
                total_written: v[6] * 512,
                system: root.as_deref() == Some(name),
                ..Default::default()
            };
            if let Some((p, t)) = self.prev.get(name) {
                let dt = now.duration_since(*t).as_secs_f64();
                if dt > 0.0 {
                    let dl = |i: usize| v[i].saturating_sub(p[i]) as f64;
                    d.read_rate = dl(2) * 512.0 / dt;
                    d.write_rate = dl(6) * 512.0 / dt;
                    d.active_pct = ((dl(9) / (dt * 1000.0)) * 100.0).clamp(0.0, 100.0) as f32;
                    let ios = dl(0) + dl(4);
                    if ios > 0.0 {
                        d.resp_ms = (dl(3) + dl(7)) / ios;
                    }
                }
            }
            self.prev.insert(name.to_string(), (v, now));
            out.push(d);
        }
        out
    }
}

pub struct MountUse {
    pub mount: String,
    pub fstype: String,
    pub source: String,
    pub total: u64,
    pub free: u64,
}

const PSEUDO_FS: &[&str] = &[
    "devtmpfs", "squashfs", "overlay", "cgroup2", "cgroup", "devpts", "ramfs", "autofs", "fuse.portal", "fuse.gvfsd-fuse", "efivarfs", "tracefs", "proc", "sysfs", "securityfs", "debugfs", "configfs", "fusectl", "pstore", "bpf", "mqueue", "hugetlbfs", "binfmt_misc", "nsfs", "rpc_pipefs", "tmpfs",
];

pub fn mounts() -> Vec<MountUse> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for l in read("/proc/self/mounts").unwrap_or_default().lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 3 || PSEUDO_FS.contains(&f[2]) || !f[0].starts_with('/') {
            continue;
        }
        if !seen.insert(f[0].to_string()) {
            continue;
        }
        let mount = f[1].replace("\\040", " ");
        let Ok(c) = std::ffi::CString::new(mount.clone()) else { continue };
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
            continue;
        }
        let bs = st.f_frsize as u64;
        out.push(MountUse { mount, fstype: f[2].into(), source: f[0].into(), total: st.f_blocks as u64 * bs, free: st.f_bavail as u64 * bs });
    }
    out
}

pub fn free_space(path: &std::path::Path) -> Option<u64> {
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

// ───────────────────────────── Network ─────────────────────────────

#[derive(Default, Clone)]
pub struct NetStat {
    pub name: String,
    pub rx_rate: f64,
    pub tx_rate: f64,
    pub rx_total: u64,
    pub tx_total: u64,
    pub link_mbps: Option<u64>,
    pub mac: String,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub up: bool,
    pub wireless: bool,
}

#[derive(Default)]
pub struct NetSampler {
    prev: HashMap<String, (u64, u64, Instant)>,
}

fn addresses() -> HashMap<String, (Vec<String>, Vec<String>)> {
    let mut m: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return m;
        }
        let mut p = ifap;
        while !p.is_null() {
            let a = &*p;
            if !a.ifa_addr.is_null() {
                let name = std::ffi::CStr::from_ptr(a.ifa_name).to_string_lossy().into_owned();
                match (*a.ifa_addr).sa_family as i32 {
                    libc::AF_INET => {
                        let sin = &*(a.ifa_addr as *const libc::sockaddr_in);
                        let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                        m.entry(name).or_default().0.push(ip.to_string());
                    }
                    libc::AF_INET6 => {
                        let sin = &*(a.ifa_addr as *const libc::sockaddr_in6);
                        m.entry(name).or_default().1.push(std::net::Ipv6Addr::from(sin.sin6_addr.s6_addr).to_string());
                    }
                    _ => {}
                }
            }
            p = a.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    m
}

impl NetSampler {
    pub fn sample(&mut self) -> Vec<NetStat> {
        let txt = read("/proc/net/dev").unwrap_or_default();
        let addrs = addresses();
        let now = Instant::now();
        let mut out = Vec::new();
        for l in txt.lines().skip(2) {
            let Some((name, rest)) = l.split_once(':') else { continue };
            let name = name.trim().to_string();
            let v: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if v.len() < 9 {
                continue;
            }
            let (rx, tx) = (v[0], v[8]);
            let base = format!("/sys/class/net/{name}");
            let mut s = NetStat {
                name: name.clone(),
                rx_total: rx,
                tx_total: tx,
                mac: read(&format!("{base}/address")).unwrap_or_default(),
                link_mbps: read(&format!("{base}/speed")).and_then(|x| x.parse::<i64>().ok()).filter(|x| *x > 0).map(|x| x as u64),
                up: read(&format!("{base}/operstate")).map_or(false, |s| s == "up" || s == "unknown"),
                wireless: std::path::Path::new(&format!("{base}/wireless")).exists(),
                ..Default::default()
            };
            if let Some((v4, v6)) = addrs.get(&name) {
                s.ipv4 = v4.clone();
                s.ipv6 = v6.clone();
            }
            if let Some((prx, ptx, t)) = self.prev.get(&name) {
                let dt = now.duration_since(*t).as_secs_f64();
                if dt > 0.0 {
                    s.rx_rate = rx.saturating_sub(*prx) as f64 / dt;
                    s.tx_rate = tx.saturating_sub(*ptx) as f64 / dt;
                }
            }
            self.prev.insert(name, (rx, tx, now));
            out.push(s);
        }
        out
    }
}

// ───────────────────────────── GPU ─────────────────────────────

#[derive(Default, Clone, Debug)]
pub struct GpuStat {
    pub name: String,
    pub vendor: String,
    pub driver: String,
    pub util: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
    pub temp: Option<f32>,
    pub power: Option<f32>,
    pub power_limit: Option<f32>,
    pub clock_mhz: Option<f32>,
    pub clock_max_mhz: Option<f32>,
    pub mem_clock_mhz: Option<f32>,
    pub mem_clock_max_mhz: Option<f32>,
    pub enc: Option<f32>,
    pub dec: Option<f32>,
    pub pci: String,
}

pub fn gpus() -> Vec<GpuStat> {
    let mut out = Vec::new();
    if which("nvidia-smi") {
        let q = "name,driver_version,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw,power.limit,clocks.gr,clocks.max.gr,clocks.mem,clocks.max.mem,utilization.encoder,utilization.decoder,pci.bus_id";
        if let Some(t) = run("nvidia-smi", &[&format!("--query-gpu={q}"), "--format=csv,noheader,nounits"]) {
            for l in t.lines() {
                let f: Vec<&str> = l.split(',').map(str::trim).collect();
                if f.len() < 15 {
                    continue;
                }
                let n = |i: usize| f[i].parse::<f32>().ok();
                out.push(GpuStat {
                    name: f[0].into(),
                    vendor: "NVIDIA".into(),
                    driver: f[1].into(),
                    util: n(2),
                    mem_used: n(3).map(|m| (m * 1048576.0) as u64),
                    mem_total: n(4).map(|m| (m * 1048576.0) as u64),
                    temp: n(5),
                    power: n(6),
                    power_limit: n(7),
                    clock_mhz: n(8),
                    clock_max_mhz: n(9),
                    mem_clock_mhz: n(10),
                    mem_clock_max_mhz: n(11),
                    enc: n(12),
                    dec: n(13),
                    pci: f[14].into(),
                });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir("/sys/class/drm") {
        let mut cards: Vec<_> = rd.flatten().filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("card") && n[4..].chars().all(|c| c.is_ascii_digit())
        }).collect();
        cards.sort_by_key(|e| e.file_name());
        for c in cards {
            let d = c.path().join("device");
            let ds = d.to_string_lossy().into_owned();
            let vendor = read(&format!("{ds}/vendor")).unwrap_or_default();
            if vendor == "0x10de" && !out.is_empty() {
                continue; // nvidia-smi already covered it
            }
            let driver = std::fs::read_link(d.join("driver")).ok().and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned())).unwrap_or_default();
            let vname = match vendor.as_str() {
                "0x1002" => "AMD",
                "0x8086" => "Intel",
                "0x10de" => "NVIDIA",
                _ => "GPU",
            };
            let pci = d.canonicalize().ok().and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned())).unwrap_or_default();
            let mut g = GpuStat { name: format!("{vname} graphics ({driver})"), vendor: vname.into(), driver, pci, ..Default::default() };
            g.util = read_f64(&format!("{ds}/gpu_busy_percent")).map(|v| v as f32);
            g.mem_used = read_u64(&format!("{ds}/mem_info_vram_used"));
            g.mem_total = read_u64(&format!("{ds}/mem_info_vram_total"));
            if let Ok(hw) = std::fs::read_dir(d.join("hwmon")) {
                for h in hw.flatten() {
                    let hp = h.path().to_string_lossy().into_owned();
                    g.temp = g.temp.or(read_f64(&format!("{hp}/temp1_input")).map(|v| (v / 1000.0) as f32));
                    g.power = g.power.or(read_f64(&format!("{hp}/power1_average")).or(read_f64(&format!("{hp}/power1_input"))).map(|v| (v / 1e6) as f32));
                    g.power_limit = g.power_limit.or(read_f64(&format!("{hp}/power1_cap")).map(|v| (v / 1e6) as f32));
                    g.clock_mhz = g.clock_mhz.or(read_f64(&format!("{hp}/freq1_input")).map(|v| (v / 1e6) as f32));
                }
            }
            if g.clock_mhz.is_none() {
                g.clock_mhz = read_f64(&format!("/sys/class/drm/{}/gt_cur_freq_mhz", c.file_name().to_string_lossy())).map(|v| v as f32);
                g.clock_max_mhz = read_f64(&format!("/sys/class/drm/{}/gt_max_freq_mhz", c.file_name().to_string_lossy())).map(|v| v as f32);
            }
            out.push(g);
        }
    }
    out
}

// ───────────────────────────── Sensors ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Sensor {
    pub group: &'static str,
    pub device: String,
    pub label: String,
    pub value: f64,
    pub unit: &'static str,
}

#[derive(Default)]
pub struct Rapl {
    prev: HashMap<String, (u64, Instant)>,
}

pub struct SensorSet {
    pub sensors: Vec<Sensor>,
    /// Sources that exist but could not be read (permission)
    pub restricted: Vec<String>,
}

impl Rapl {
    pub fn read(&mut self) -> (Vec<Sensor>, Vec<String>) {
        let mut out = Vec::new();
        let mut restricted = Vec::new();
        let now = Instant::now();
        if let Ok(rd) = std::fs::read_dir("/sys/class/powercap") {
            for e in rd.flatten() {
                let p = e.path();
                let n = e.file_name().to_string_lossy().into_owned();
                if !n.starts_with("intel-rapl") && !n.starts_with("amd-rapl") {
                    continue;
                }
                let ef = p.join("energy_uj");
                let name = read(&p.join("name").to_string_lossy()).unwrap_or(n.clone());
                match std::fs::read_to_string(&ef).ok().and_then(|s| s.trim().parse::<u64>().ok()) {
                    Some(uj) => {
                        if let Some((pu, t)) = self.prev.get(&n) {
                            let dt = now.duration_since(*t).as_secs_f64();
                            if dt > 0.0 && uj >= *pu {
                                out.push(Sensor { group: "Powers", device: "RAPL".into(), label: format!("{name} power"), value: (uj - pu) as f64 / 1e6 / dt, unit: "W" });
                            }
                        }
                        self.prev.insert(n, (uj, now));
                    }
                    None => restricted.push(format!("{name} energy counter (read permission, which by default only root has)")),
                }
            }
        }
        (out, restricted)
    }
}

pub fn sensors(rapl: &mut Rapl, logical: usize, gpu_list: &[GpuStat]) -> SensorSet {
    let mut s = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/hwmon") {
        let mut dirs: Vec<_> = rd.flatten().collect();
        dirs.sort_by_key(|e| e.file_name());
        for h in dirs {
            let p = h.path();
            let dev = read(&p.join("name").to_string_lossy()).unwrap_or_else(|| h.file_name().to_string_lossy().into_owned());
            let Ok(files) = std::fs::read_dir(&p) else { continue };
            let mut names: Vec<String> = files.flatten().map(|f| f.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            for f in names {
                let Some(stem) = f.strip_suffix("_input") else { continue };
                let Some(v) = read_f64(&p.join(&f).to_string_lossy()) else { continue };
                let label = read(&p.join(format!("{stem}_label")).to_string_lossy()).unwrap_or_else(|| stem.to_string());
                let (group, value, unit) = if stem.starts_with("temp") {
                    ("Temperatures", v / 1000.0, "°C")
                } else if stem.starts_with("in") {
                    ("Voltages", v / 1000.0, "V")
                } else if stem.starts_with("power") {
                    ("Powers", v / 1e6, "W")
                } else if stem.starts_with("fan") {
                    ("Fans", v, "RPM")
                } else if stem.starts_with("freq") {
                    ("Clocks", v / 1e6, "MHz")
                } else if stem.starts_with("curr") {
                    ("Counters", v / 1000.0, "A")
                } else {
                    continue;
                };
                s.push(Sensor { group, device: dev.clone(), label, value, unit });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir("/sys/class/thermal") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("thermal_zone") {
                let p = e.path().to_string_lossy().into_owned();
                if let (Some(t), Some(ty)) = (read_f64(&format!("{p}/temp")), read(&format!("{p}/type"))) {
                    s.push(Sensor { group: "Temperatures", device: "acpi-thermal-zones".into(), label: ty, value: t / 1000.0, unit: "°C" });
                }
            }
        }
    }
    for (i, f) in cpu_freqs(logical).into_iter().enumerate() {
        if f > 0.0 {
            s.push(Sensor { group: "Clocks", device: "cpufreq".into(), label: format!("Core {i} clock"), value: f, unit: "MHz" });
        }
    }
    if let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") {
        for e in rd.flatten() {
            let p = e.path().to_string_lossy().into_owned();
            if read(&format!("{p}/type")).as_deref() == Some("Battery") {
                if let Some(c) = read_f64(&format!("{p}/capacity")) {
                    s.push(Sensor { group: "Levels (Energy Perf)", device: "battery".into(), label: "Battery charge".into(), value: c, unit: "%" });
                }
                if let Some(w) = read_f64(&format!("{p}/power_now")) {
                    s.push(Sensor { group: "Powers", device: "battery".into(), label: "Battery power".into(), value: w / 1e6, unit: "W" });
                }
            }
        }
    }
    let (r, restricted) = rapl.read();
    s.extend(r);
    for g in gpu_list {
        let d = g.name.clone();
        if let Some(t) = g.temp {
            s.push(Sensor { group: "Temperatures", device: d.clone(), label: "GPU core".into(), value: t as f64, unit: "°C" });
        }
        if let Some(p) = g.power {
            s.push(Sensor { group: "Powers", device: d.clone(), label: "GPU board power".into(), value: p as f64, unit: "W" });
        }
        if let Some(c) = g.clock_mhz {
            s.push(Sensor { group: "Clocks", device: d.clone(), label: "GPU graphics clock".into(), value: c as f64, unit: "MHz" });
        }
        if let Some(u) = g.util {
            s.push(Sensor { group: "Levels (Energy Perf)", device: d, label: "GPU utilization".into(), value: u as f64, unit: "%" });
        }
    }
    SensorSet { sensors: s, restricted }
}

// ───────────────────────────── System information ─────────────────────────────

pub type InfoSection = (String, Vec<(String, String)>);

fn os_release() -> HashMap<String, String> {
    read("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.trim_matches('"').to_string()))
        .collect()
}

pub fn uname() -> (String, String, String) {
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        libc::uname(&mut u);
        let f = |a: &[libc::c_char]| std::ffi::CStr::from_ptr(a.as_ptr()).to_string_lossy().into_owned();
        (f(&u.nodename), f(&u.release), f(&u.machine))
    }
}

pub fn uptime_secs() -> u64 {
    read("/proc/uptime").and_then(|s| s.split_whitespace().next().and_then(|x| x.parse::<f64>().ok())).unwrap_or(0.0) as u64
}

pub fn load_avg() -> (f64, f64, f64, String) {
    let s = read("/proc/loadavg").unwrap_or_default();
    let f: Vec<&str> = s.split_whitespace().collect();
    let p = |i: usize| f.get(i).and_then(|x| x.parse().ok()).unwrap_or(0.0);
    (p(0), p(1), p(2), f.get(3).unwrap_or(&"0/0").to_string())
}

pub fn system_info(cpu: &CpuStatic, disks: &[DiskStat], nets: &[NetStat], gpus: &[GpuStat]) -> Vec<InfoSection> {
    let m = mem();
    let (host, kernel, arch) = uname();
    let osr = os_release();
    let (l1, l5, l15, tasks) = load_avg();
    let dmi = |k: &str| read(&format!("/sys/class/dmi/id/{k}")).unwrap_or_else(|| "n/a".into());
    let mut out: Vec<InfoSection> = Vec::new();
    let kv = |a: &str, b: String| (a.to_string(), b);
    out.push((
        "Hardware Overview".into(),
        vec![
            kv("Machine", format!("{} {}", dmi("sys_vendor"), dmi("product_name"))),
            kv("Form factor", read("/sys/class/dmi/id/chassis_type").map(chassis).unwrap_or_else(|| "n/a".into())),
            kv("Firmware", format!("{} {}", dmi("bios_vendor"), dmi("bios_version"))),
            kv("Processor", cpu.model.clone()),
            kv("Logical processors", cpu.logical.to_string()),
            kv("Physical cores", cpu.physical.to_string()),
            kv("Sockets", cpu.sockets.to_string()),
            kv("NUMA nodes", cpu.numa.to_string()),
            kv("Base frequency", cpu.base_mhz.map(|m| format!("{m:.0} MHz")).unwrap_or_else(|| "n/a".into())),
            kv("Current frequency", { let f = cpu_freqs(cpu.logical); if f.is_empty() { "n/a".into() } else { format!("{:.0} MHz (avg)", f.iter().sum::<f64>() / f.len() as f64) } }),
            kv("Frequency driver", cpu.driver.clone()),
            kv("Frequency governor", cpu.governor.clone()),
            kv("Power preference", cpu.epp.clone()),
            kv("Virtualization", cpu.virt.clone()),
        ]
        .into_iter()
        .chain(cpu.caches.iter().map(|(a, b)| (format!("{a} cache"), b.clone())))
        .collect(),
    ));
    out.push((
        "Memory".into(),
        vec![
            kv("Physical memory", bytes(m.total as f64)),
            kv("In use", bytes(m.used as f64)),
            kv("Available", bytes(m.available as f64)),
            kv("Cached", bytes(m.cached as f64)),
            kv("Buffers", bytes(m.buffers as f64)),
            kv("Shared", bytes(m.shared as f64)),
            kv("Kernel slab", bytes(m.slab as f64)),
            kv("Swap in use", bytes(m.swap_used as f64)),
            kv("Swap total", bytes(m.swap_total as f64)),
            kv("Committed", bytes(m.committed as f64)),
            kv("Commit limit", bytes(m.commit_limit as f64)),
            kv("Stall (PSI some, 10s)", format!("{:.2}%", m.psi_some10)),
            kv("Page ins", bytes(m.page_in_kb as f64 * 1024.0)),
            kv("Page outs", bytes(m.page_out_kb as f64 * 1024.0)),
        ],
    ));
    out.push((
        "Storage".into(),
        disks
            .iter()
            .flat_map(|d| {
                vec![
                    (format!("{} ", d.name), format!("{} {}{}", d.model.trim(), bytes(d.size as f64), if d.system { "  [system disk]" } else { "" })),
                    (format!("{} type", d.name), if d.rotational { "Rotational".into() } else { "Solid state".into() }),
                    (format!("{} I/O scheduler", d.name), d.scheduler.clone()),
                    (format!("{} read since boot", d.name), bytes(d.total_read as f64)),
                    (format!("{} written since boot", d.name), bytes(d.total_written as f64)),
                ]
            })
            .collect(),
    ));
    out.push((
        "Graphics".into(),
        gpus.iter()
            .flat_map(|g| {
                let mut v = vec![kv("Adapter", g.name.clone()), kv("Driver version", g.driver.clone()), kv("PCI Express", g.pci.clone())];
                if let Some(t) = g.mem_total {
                    v.push(kv("Dedicated memory", bytes(t as f64)));
                }
                if let Some(c) = g.clock_max_mhz {
                    v.push(kv("Maximum clock", format!("{c:.0} MHz")));
                }
                if let Some(p) = g.power_limit {
                    v.push(kv("Power limit", format!("{p:.0} W")));
                }
                v
            })
            .chain([
                kv("OpenGL", run("glxinfo", &["-B"]).and_then(|t| t.lines().find(|l| l.starts_with("OpenGL version")).map(|l| l.split_once(':').map(|x| x.1.trim().to_string()).unwrap_or_default())).unwrap_or_else(|| "n/a (glxinfo not installed)".into())),
                kv("Vulkan", run("vulkaninfo", &["--summary"]).and_then(|t| t.lines().find(|l| l.contains("apiVersion")).map(|l| l.split_once('=').map(|x| x.1.trim().to_string()).unwrap_or_default())).unwrap_or_else(|| "n/a (vulkaninfo not installed)".into())),
            ])
            .collect(),
    ));
    out.push((
        "Network".into(),
        nets.iter()
            .filter(|n| n.name != "lo")
            .flat_map(|n| {
                vec![
                    (format!("{} state", n.name), if n.up { "Up".to_string() } else { "Down".to_string() }),
                    (format!("{} hardware address", n.name), n.mac.clone()),
                    (format!("{} IPv4 address", n.name), n.ipv4.join(", ")),
                    (format!("{} IPv6 address", n.name), n.ipv6.join(", ")),
                    (format!("{} link speed", n.name), n.link_mbps.map(|m| format!("{m} Mb/s")).unwrap_or_else(|| "n/a".into())),
                    (format!("{} total received", n.name), bytes(n.rx_total as f64)),
                    (format!("{} total sent", n.name), bytes(n.tx_total as f64)),
                ]
            })
            .collect(),
    ));
    let mut power = vec![];
    if let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") {
        for e in rd.flatten() {
            let p = e.path().to_string_lossy().into_owned();
            let ty = read(&format!("{p}/type")).unwrap_or_default();
            if ty == "Battery" {
                power.push(kv("Power source", "Battery".into()));
                power.push(kv("Battery charge", format!("{}%", read(&format!("{p}/capacity")).unwrap_or_default())));
                power.push(kv("Charging", read(&format!("{p}/status")).unwrap_or_default()));
                power.push(kv("Cycle count", read(&format!("{p}/cycle_count")).unwrap_or_else(|| "n/a".into())));
                power.push(kv("Condition", read(&format!("{p}/health")).unwrap_or_else(|| "n/a".into())));
            } else if ty == "Mains" {
                power.push(kv("Mains online", read(&format!("{p}/online")).map(|v| if v == "1" { "Yes".into() } else { "No".into() }).unwrap_or_default()));
            }
        }
    }
    if power.is_empty() {
        power.push(kv("Power source", "AC (no battery found)".into()));
    }
    out.push(("Power".into(), power));
    out.push((
        "Operating System".into(),
        vec![
            kv("Distribution", osr.get("PRETTY_NAME").cloned().unwrap_or_else(|| "Linux".into())),
            kv("Kernel release", kernel),
            kv("Architecture", arch),
            kv("Host name", host),
            kv("Time since boot", uptime_str(uptime_secs())),
            kv("Load average (1 min)", format!("{l1:.2}")),
            kv("Load average (5 min)", format!("{l5:.2}")),
            kv("Load average (15 min)", format!("{l15:.2}")),
            kv("Runnable / total tasks", tasks),
            kv("Virtual machine", cpu.virt.clone()),
        ],
    ));
    out
}

fn chassis(code: String) -> String {
    match code.trim() {
        "3" => "Desktop",
        "8" | "9" | "10" | "14" => "Laptop",
        "30" | "31" | "32" => "Tablet / convertible",
        "17" | "23" => "Server",
        "13" => "All-in-one",
        _ => "Other",
    }
    .into()
}

pub fn export_info(sections: &[InfoSection]) -> String {
    let mut s = String::new();
    for (t, rows) in sections {
        s += &format!("{t}\n{}\n", "=".repeat(t.len()));
        for (k, v) in rows {
            s += &format!("{k:<32} {v}\n");
        }
        s.push('\n');
    }
    s
}
