use crate::util::read;
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

#[derive(Clone, Default, Debug)]
pub struct Proc {
    pub pid: i32,
    pub ppid: i32,
    pub name: String,
    pub cmdline: String,
    pub state: char,
    pub uid: u32,
    pub user: String,
    pub cpu: f32,
    pub rss: u64,
    pub virt: u64,
    pub threads: u32,
    pub nice: i32,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub read_rate: f64,
    pub write_rate: f64,
    pub cpu_ticks: u64,
    pub start_ticks: u64,
}

impl Proc {
    pub fn state_label(&self) -> &'static str {
        match self.state {
            'R' => "Running",
            'S' | 'D' | 'I' => "Sleeping",
            'T' | 't' => "Stopped",
            'Z' => "Zombie",
            'X' => "Exited",
            _ => "Unknown",
        }
    }
}

pub fn clk_tck() -> f64 {
    let v = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if v > 0 {
        v as f64
    } else {
        100.0
    }
}

fn page_size() -> u64 {
    let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if v > 0 {
        v as u64
    } else {
        4096
    }
}

pub fn users() -> HashMap<u32, String> {
    let mut m = HashMap::new();
    if let Ok(t) = std::fs::read_to_string("/etc/passwd") {
        for l in t.lines() {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() > 2 {
                if let Ok(uid) = f[2].parse() {
                    m.entry(uid).or_insert_with(|| f[0].to_string());
                }
            }
        }
    }
    m
}

fn parse_stat(txt: &str) -> Option<(String, Vec<&str>)> {
    let open = txt.find('(')?;
    let close = txt.rfind(')')?;
    let name = txt[open + 1..close].to_string();
    let rest: Vec<&str> = txt[close + 1..].split_whitespace().collect();
    if rest.len() < 22 {
        return None;
    }
    Some((name, rest))
}

#[derive(Default)]
pub struct ProcSampler {
    prev: HashMap<i32, (u64, u64, u64)>, // ticks, read, write
    at: Option<Instant>,
    users: HashMap<u32, String>,
    ncpu: f64,
    ticks: f64,
    page: u64,
}

impl ProcSampler {
    pub fn new() -> Self {
        let ncpu = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
        ProcSampler { users: users(), ncpu, ticks: clk_tck(), page: page_size(), ..Default::default() }
    }

    /// `per_core`: report CPU as percent of one core (can exceed 100) instead of whole machine.
    pub fn sample(&mut self, per_core: bool) -> Vec<Proc> {
        let now = Instant::now();
        let dt = self.at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        let mut out = Vec::new();
        let mut cur = HashMap::new();
        if let Ok(rd) = std::fs::read_dir("/proc") {
            for e in rd.flatten() {
                let fname = e.file_name();
                let Some(pid) = fname.to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
                let base = format!("/proc/{pid}");
                let Ok(stat) = std::fs::read_to_string(format!("{base}/stat")) else { continue };
                let Some((name, f)) = parse_stat(&stat) else { continue };
                let utime: u64 = f[11].parse().unwrap_or(0);
                let stime: u64 = f[12].parse().unwrap_or(0);
                let ticks = utime + stime;
                let uid = std::fs::metadata(&base).map(|m| m.uid()).unwrap_or(0);
                let cmdline = std::fs::read(format!("{base}/cmdline"))
                    .map(|b| {
                        b.split(|c| *c == 0)
                            .filter(|s| !s.is_empty())
                            .map(|s| String::from_utf8_lossy(s).into_owned())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let (mut rb, mut wb) = (0u64, 0u64);
                if let Ok(io) = std::fs::read_to_string(format!("{base}/io")) {
                    for l in io.lines() {
                        if let Some(v) = l.strip_prefix("read_bytes: ") {
                            rb = v.trim().parse().unwrap_or(0);
                        } else if let Some(v) = l.strip_prefix("write_bytes: ") {
                            wb = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let (mut cpu, mut rr, mut wr) = (0.0f32, 0.0, 0.0);
                if let Some(&(pt, pr, pw)) = self.prev.get(&pid) {
                    if dt > 0.0 {
                        let d = ticks.saturating_sub(pt) as f64 / self.ticks / dt * 100.0;
                        cpu = (if per_core { d } else { d / self.ncpu }) as f32;
                        rr = rb.saturating_sub(pr) as f64 / dt;
                        wr = wb.saturating_sub(pw) as f64 / dt;
                    }
                }
                cur.insert(pid, (ticks, rb, wb));
                out.push(Proc {
                    pid,
                    ppid: f[1].parse().unwrap_or(0),
                    name,
                    cmdline,
                    state: f[0].chars().next().unwrap_or('?'),
                    uid,
                    user: self.users.get(&uid).cloned().unwrap_or_else(|| uid.to_string()),
                    cpu,
                    rss: f[21].parse::<u64>().unwrap_or(0) * self.page,
                    virt: f[20].parse().unwrap_or(0),
                    threads: f[17].parse().unwrap_or(1),
                    nice: f[16].parse().unwrap_or(0),
                    read_bytes: rb,
                    write_bytes: wb,
                    read_rate: rr,
                    write_rate: wr,
                    cpu_ticks: ticks,
                    start_ticks: f[19].parse().unwrap_or(0),
                });
            }
        }
        self.prev = cur;
        self.at = Some(now);
        out
    }
}

/// Depth-first ordering of the process tree: (depth, index into `procs`).
pub fn tree_order(procs: &[Proc], collapsed: &std::collections::HashSet<i32>) -> Vec<(usize, usize)> {
    let mut children: HashMap<i32, Vec<usize>> = HashMap::new();
    let pids: std::collections::HashSet<i32> = procs.iter().map(|p| p.pid).collect();
    let mut roots = Vec::new();
    for (i, p) in procs.iter().enumerate() {
        if p.ppid != p.pid && pids.contains(&p.ppid) {
            children.entry(p.ppid).or_default().push(i);
        } else {
            roots.push(i);
        }
    }
    let mut out = Vec::with_capacity(procs.len());
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|i| (0, i)).collect();
    while let Some((d, i)) = stack.pop() {
        out.push((d, i));
        if collapsed.contains(&procs[i].pid) {
            continue;
        }
        if let Some(ch) = children.get(&procs[i].pid) {
            for &c in ch.iter().rev() {
                stack.push((d + 1, c));
            }
        }
    }
    out
}

pub fn descendants(procs: &[Proc], pid: i32) -> Vec<i32> {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut out = Vec::new();
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        if let Some(ch) = children.get(&p) {
            for &c in ch {
                if c != pid && !out.contains(&c) {
                    out.push(c);
                    stack.push(c);
                }
            }
        }
    }
    out
}

pub fn signal(pid: i32, sig: i32) -> Result<(), String> {
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

pub fn set_nice(pid: i32, nice: i32) -> Result<(), String> {
    let r = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, nice) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

pub fn get_affinity(pid: i32) -> Option<Vec<usize>> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(pid, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return None;
        }
        Some((0..1024).filter(|&c| libc::CPU_ISSET(c, &set)).collect())
    }
}

/// Applies the mask to every thread of the process; returns (applied, total).
pub fn set_affinity(pid: i32, cpus: &[usize]) -> Result<(usize, usize), String> {
    if cpus.is_empty() {
        return Err("At least one processor must remain selected".into());
    }
    let mut tids: Vec<i32> = std::fs::read_dir(format!("/proc/{pid}/task"))
        .map_err(|_| "Set affinity failed: the process has exited".to_string())?
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse().ok()))
        .collect();
    if tids.is_empty() {
        tids.push(pid);
    }
    let total = tids.len();
    let mut ok = 0;
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cpus {
            libc::CPU_SET(c, &mut set);
        }
        for t in tids {
            if libc::sched_setaffinity(t, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0 {
                ok += 1;
            }
        }
    }
    if ok == 0 {
        Err("Set affinity failed: access denied".into())
    } else {
        Ok((ok, total))
    }
}

pub fn parse_cpu_list(s: &str, max: usize) -> Result<Vec<usize>, String> {
    let mut v = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (usize, usize) = (
                a.trim().parse().map_err(|_| format!("bad range '{part}'"))?,
                b.trim().parse().map_err(|_| format!("bad range '{part}'"))?,
            );
            v.extend(a..=b);
        } else {
            v.push(part.parse().map_err(|_| format!("bad cpu '{part}'"))?);
        }
    }
    if v.iter().any(|&c| c >= max) {
        return Err(format!("processor index out of range (0-{})", max.saturating_sub(1)));
    }
    v.sort_unstable();
    v.dedup();
    Ok(v)
}

/// Multi-section text description of one process (details, environment, threads, files).
pub fn detail(pid: i32) -> String {
    let base = format!("/proc/{pid}");
    let mut s = String::new();
    if !std::path::Path::new(&base).exists() {
        return "The process exited before its details could be read.".into();
    }
    if let Some(c) = std::fs::read(format!("{base}/cmdline")).ok().map(|b| {
        b.split(|c| *c == 0).map(|x| String::from_utf8_lossy(x).into_owned()).collect::<Vec<_>>().join(" ")
    }) {
        s += &format!("Command line: {}\n", c.trim());
    }
    if let Ok(exe) = std::fs::read_link(format!("{base}/exe")) {
        s += &format!("Executable:   {}\n", exe.display());
    }
    if let Ok(cwd) = std::fs::read_link(format!("{base}/cwd")) {
        s += &format!("Working dir:  {}\n", cwd.display());
    }
    if let Some(st) = read(&format!("{base}/status")) {
        for l in st.lines() {
            for k in ["Umask", "VmPeak", "VmSize", "VmRSS", "VmSwap", "Threads", "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches", "Cpus_allowed_list"] {
                if l.starts_with(k) {
                    s += &format!("{}\n", l.replace('\t', " "));
                }
            }
        }
    }
    if let Some(sm) = read(&format!("{base}/smaps_rollup")) {
        s += "\nMemory (smaps_rollup)\n";
        for l in sm.lines().skip(1) {
            s += &format!("  {}\n", l.replace('\t', " "));
        }
    } else {
        s += "\nMemory mappings: not readable (summarizing another user's memory needs ptrace access; run as that user or root).\n";
    }
    match std::fs::read(format!("{base}/environ")) {
        Ok(b) => {
            s += "\nEnvironment\n";
            for v in b.split(|c| *c == 0).filter(|x| !x.is_empty()) {
                s += &format!("  {}\n", String::from_utf8_lossy(v));
            }
        }
        Err(_) => s += "\nEnvironment: not readable.\n",
    }
    if let Ok(rd) = std::fs::read_dir(format!("{base}/task")) {
        let mut n = 0;
        s += "\nThreads\n";
        for e in rd.flatten() {
            let tid = e.file_name().to_string_lossy().into_owned();
            let nm = read(&format!("{base}/task/{tid}/comm")).unwrap_or_default();
            if n < 64 {
                s += &format!("  Thread {tid}  {nm}\n");
            }
            n += 1;
        }
        if n > 64 {
            s += &format!("  (Threads listed: 64 of {n})\n");
        }
    }
    if let Ok(rd) = std::fs::read_dir(format!("{base}/fd")) {
        s += "\nOpen files, sockets and pipes\n";
        let mut n = 0;
        for e in rd.flatten() {
            if let Ok(t) = std::fs::read_link(e.path()) {
                if n < 128 {
                    s += &format!("  {:>4} -> {}\n", e.file_name().to_string_lossy(), t.display());
                }
                n += 1;
            }
        }
        if n > 128 {
            s += &format!("  ... {n} descriptors in total\n");
        }
    } else {
        s += "\nOpen files: not readable (needs the same user or root).\n";
    }
    if let Some(stack) = read(&format!("{base}/stack")) {
        s += &format!("\nKernel stack\n{stack}\n");
    }
    s
}

pub fn sample_report(procs: &[Proc], dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let mut s = String::from("PID\tPPID\tUSER\tSTATE\tCPU%\tRSS\tTHREADS\tNAME\tCOMMAND\n");
    for p in procs {
        s += &format!("{}\t{}\t{}\t{}\t{:.1}\t{}\t{}\t{}\t{}\n", p.pid, p.ppid, p.user, p.state_label(), p.cpu, p.rss, p.threads, p.name, p.cmdline);
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("process-sample-{ts}.tsv"));
    std::fs::write(&path, s).map_err(|e| e.to_string())?;
    Ok(path)
}
