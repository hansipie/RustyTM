use crate::bench::{self, Bench, BenchResult, Kind};
use crate::collect::inventory::{self as inv, App as InstalledApp, ConnResult, ModuleResult, Service, Session, Startup};
use crate::collect::proc::{self, Proc, ProcSampler};
use crate::collect::system::*;
use crate::diskscan::Scanner;
use crate::recorder::{self, Recorder, Sample, Trace};
use crate::settings::Settings;
use crate::util::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PAGES: [&str; 16] = [
    "Summary", "Processes", "Performance", "App history", "Startup apps", "Users", "Services", "System Info", "Power & Freq", "Flight Recorder", "Connections", "Installed Apps", "Drivers", "Disk Space", "Benchmarks", "Settings",
];
pub const P_SUMMARY: usize = 0;
pub const P_PROCESSES: usize = 1;
pub const P_PERF: usize = 2;
pub const P_APPHIST: usize = 3;
pub const P_STARTUP: usize = 4;
pub const P_USERS: usize = 5;
pub const P_SERVICES: usize = 6;
pub const P_SYSINFO: usize = 7;
pub const P_POWER: usize = 8;
pub const P_RECORDER: usize = 9;
pub const P_CONN: usize = 10;
pub const P_APPS: usize = 11;
pub const P_DRIVERS: usize = 12;
pub const P_DISK: usize = 13;
pub const P_BENCH: usize = 14;
pub const P_SETTINGS: usize = 15;

pub const HIST_LEN: usize = 120;
pub const PROC_COLS: [&str; 9] = ["Name", "PID", "CPU", "Memory", "Disk read", "Disk write", "User", "Status", "Threads"];

pub fn page_from_name(n: &str) -> Option<usize> {
    let n = n.to_lowercase().replace(['-', '_', ' ', '&'], "");
    let alias: &[(&str, usize)] = &[
        ("summary", 0), ("processes", 1), ("process", 1), ("performance", 2), ("apphistory", 3), ("history", 3), ("startup", 4), ("startupapps", 4), ("users", 5), ("services", 6),
        ("sysinfo", 7), ("systeminfo", 7), ("powerfreq", 8), ("powerandfreq", 8), ("power", 8), ("recorder", 9), ("flightrecorder", 9), ("flight", 9), ("connections", 10), ("networkconnections", 10),
        ("installedapps", 11), ("apps", 11), ("drivers", 12), ("kernelmodules", 12), ("diskspace", 13), ("disk", 13), ("benchmarks", 14), ("benchmark", 14), ("settings", 15),
    ];
    alias.iter().find(|(a, _)| *a == n).map(|x| x.1)
}

#[derive(Default)]
pub struct Hist {
    pub cpu: VecDeque<u64>,
    pub mem: VecDeque<u64>,
    pub disk: VecDeque<u64>,
    pub rx: VecDeque<u64>,
    pub tx: VecDeque<u64>,
    pub gpu: VecDeque<u64>,
    pub power: VecDeque<u64>,
}

pub fn push(h: &mut VecDeque<u64>, v: u64) {
    if h.len() >= HIST_LEN {
        h.pop_front();
    }
    h.push_back(v);
}

pub enum PromptKind {
    Filter,
    Affinity(i32),
    Nice(i32),
    Signal(i32),
    Run,
    ScanPath,
    BenchDir,
    TraceFile,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub buf: String,
}

pub enum Confirm {
    Signal { pids: Vec<i32>, sig: i32, text: String },
    StopScan,
    ClearHistory,
    Disconnect(String, String),
    Quit,
    DiscardTrace,
    ResetSettings,
}

pub struct Overlay {
    pub title: String,
    pub text: String,
    pub scroll: u16,
}

pub struct Replay {
    pub trace: Trace,
    pub pos: usize,
    pub playing: bool,
    pub last: Instant,
}

#[derive(Default, Clone, Copy, PartialEq)]
pub enum CpuGraph {
    #[default]
    Overall,
    Logical,
    Classes,
}

pub struct App {
    pub s: Settings,
    pub page: usize,
    pub quit: bool,
    pub started: Instant,
    pub last_tick: Instant,
    pub msg: Option<(String, Instant)>,
    pub prompt: Option<Prompt>,
    pub confirm: Option<Confirm>,
    pub overlay: Option<Overlay>,
    pub help: bool,
    pub sel: [usize; 16],
    pub filter: [String; 16],
    pub offset: [usize; 16],

    // sampled data
    pub cpu_static: CpuStatic,
    cpu_sampler: CpuSampler,
    proc_sampler: ProcSampler,
    disk_sampler: DiskSampler,
    net_sampler: NetSampler,
    rapl: Rapl,
    pub cpu: CpuUse,
    pub freqs: Vec<f64>,
    pub mem: Mem,
    pub disks: Vec<DiskStat>,
    pub nets: Vec<NetStat>,
    pub gpus: Arc<Mutex<Vec<GpuStat>>>,
    pub gpu_now: Vec<GpuStat>,
    pub sensors: Vec<Sensor>,
    pub restricted: Vec<String>,
    pub procs: Vec<Proc>,
    pub plist: Vec<Proc>,
    exited: HashMap<i32, (Proc, Instant)>,
    smooth: HashMap<i32, f32>,
    pub hist: Hist,
    pub dh: HashMap<String, VecDeque<u64>>,
    pub perf_dev: usize,
    pub cpu_graph: CpuGraph,
    pub show_kernel: bool,

    // processes
    pub proc_sel: i32,
    pub proc_values: bool,
    pub collapsed: HashSet<i32>,

    // app history
    pub app_hist: HashMap<String, (f64, u64, u64)>,
    ah_prev: HashMap<i32, (u64, u64, u64)>,

    // inventories
    pub startup: Vec<Startup>,
    startup_bg: Bg<Vec<Startup>>,
    pub sessions: Vec<Session>,
    sess_bg: Bg<Vec<Session>>,
    pub services: Vec<Service>,
    svc_bg: Bg<Vec<Service>>,
    pub conns: Option<ConnResult>,
    conn_bg: Bg<ConnResult>,
    pub conn_scope: usize,
    pub show_ipc: bool,
    pub apps: Vec<InstalledApp>,
    apps_bg: Bg<Vec<InstalledApp>>,
    pub apps_loaded: bool,
    pub show_pkgs: bool,
    pub mods: Option<ModuleResult>,
    mods_bg: Bg<ModuleResult>,
    pub mods_loaded_only: bool,
    pub sysinfo: Vec<InfoSection>,
    sysinfo_bg: Bg<Vec<InfoSection>>,
    pub loaded_at: HashMap<usize, Instant>,
    pub sysinfo_scroll: u16,

    // disk space
    pub scanner: Scanner,
    pub mounts: Vec<MountUse>,
    pub disk_view: usize,

    // benchmarks
    pub bench: Bench,
    pub bench_hist: Vec<BenchResult>,
    pub bench_show_hist: bool,
    pub bench_dir: PathBuf,

    // recorder
    pub rec: Option<Recorder>,
    pub replay: Option<Replay>,
    pub traces: Vec<PathBuf>,
    pub trace_pick: bool,
}

impl App {
    pub fn new(s: Settings) -> App {
        let cpu_static = cpu_static();
        let gpus = Arc::new(Mutex::new(crate::collect::system::gpus()));
        {
            let g = gpus.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(1500));
                let v = crate::collect::system::gpus();
                if let Ok(mut x) = g.lock() {
                    *x = v;
                }
            });
        }
        let bench_dir = if s.bench_dir.is_empty() { std::env::temp_dir() } else { PathBuf::from(&s.bench_dir) };
        let collapsed = s.collapsed.iter().copied().collect();
        let page = s.last_page.min(PAGES.len() - 1);
        let app_hist = std::fs::read_to_string(data_dir().join("app-history.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let mut a = App {
            s,
            page,
            quit: false,
            started: Instant::now(),
            last_tick: Instant::now() - Duration::from_secs(10),
            msg: None,
            prompt: None,
            confirm: None,
            overlay: None,
            help: false,
            sel: [0; 16],
            filter: Default::default(),
            offset: [0; 16],
            cpu_static,
            cpu_sampler: CpuSampler::default(),
            proc_sampler: ProcSampler::new(),
            disk_sampler: DiskSampler::default(),
            net_sampler: NetSampler::default(),
            rapl: Rapl::default(),
            cpu: CpuUse::default(),
            freqs: vec![],
            mem: Mem::default(),
            disks: vec![],
            nets: vec![],
            gpus,
            gpu_now: vec![],
            sensors: vec![],
            restricted: vec![],
            procs: vec![],
            plist: vec![],
            exited: HashMap::new(),
            smooth: HashMap::new(),
            hist: Hist::default(),
            dh: HashMap::new(),
            perf_dev: 0,
            cpu_graph: CpuGraph::Overall,
            show_kernel: false,
            proc_sel: 0,
            proc_values: true,
            collapsed,
            app_hist,
            ah_prev: HashMap::new(),
            startup: vec![],
            startup_bg: Bg::new(),
            sessions: vec![],
            sess_bg: Bg::new(),
            services: vec![],
            svc_bg: Bg::new(),
            conns: None,
            conn_bg: Bg::new(),
            conn_scope: 0,
            show_ipc: false,
            apps: vec![],
            apps_bg: Bg::new(),
            apps_loaded: false,
            show_pkgs: false,
            mods: None,
            mods_bg: Bg::new(),
            mods_loaded_only: false,
            sysinfo: vec![],
            sysinfo_bg: Bg::new(),
            loaded_at: HashMap::new(),
            sysinfo_scroll: 0,
            scanner: Scanner::new(),
            mounts: vec![],
            disk_view: 0,
            bench: Bench::new(),
            bench_hist: bench::load_history(),
            bench_show_hist: false,
            bench_dir,
            rec: None,
            replay: None,
            traces: recorder::list_traces(),
            trace_pick: false,
        };
        a.tick();
        a.enter_page();
        a
    }

    pub fn say(&mut self, m: impl Into<String>) {
        self.msg = Some((m.into(), Instant::now()));
    }

    pub fn save_settings(&mut self) {
        self.s.last_page = self.page;
        self.s.collapsed = self.collapsed.iter().copied().collect();
        if let Err(e) = self.s.save() {
            self.say(format!("Settings could not be saved: {e}"));
        }
    }

    pub fn persist_on_exit(&mut self) {
        self.save_settings();
        let _ = std::fs::create_dir_all(data_dir());
        if let Ok(t) = serde_json::to_string(&self.app_hist) {
            let _ = std::fs::write(data_dir().join("app-history.json"), t);
        }
    }

    // ───────────── sampling ─────────────

    pub fn tick(&mut self) {
        self.last_tick = Instant::now();
        self.cpu = self.cpu_sampler.sample();
        self.freqs = cpu_freqs(self.cpu_static.logical);
        self.mem = mem();
        self.disks = self.disk_sampler.sample();
        self.nets = self.net_sampler.sample();
        if let Ok(g) = self.gpus.lock() {
            self.gpu_now = g.clone();
        }
        let set = sensors(&mut self.rapl, self.cpu_static.logical, &self.gpu_now);
        self.sensors = set.sensors;
        self.restricted = set.restricted;
        self.sample_procs();

        push(&mut self.hist.cpu, self.cpu.overall.round() as u64);
        push(&mut self.hist.mem, if self.mem.total > 0 { (self.mem.used as f64 / self.mem.total as f64 * 100.0) as u64 } else { 0 });
        push(&mut self.hist.disk, self.disks.iter().map(|d| d.active_pct).fold(0.0, f32::max).round() as u64);
        let (rx, tx) = self.net_totals();
        push(&mut self.hist.rx, rx as u64);
        push(&mut self.hist.tx, tx as u64);
        push(&mut self.hist.gpu, self.gpu_now.first().and_then(|g| g.util).unwrap_or(0.0).round() as u64);
        let pw = self.sensors.iter().filter(|s| s.label.contains("package") || s.label.contains("Battery power")).map(|s| s.value).next().unwrap_or(0.0);
        push(&mut self.hist.power, pw.round() as u64);

        let mut dh: Vec<(String, u64)> = Vec::new();
        for d in &self.disks {
            dh.push((format!("disk:{}", d.name), d.active_pct.round() as u64));
            dh.push((format!("dr:{}", d.name), d.read_rate as u64));
            dh.push((format!("dw:{}", d.name), d.write_rate as u64));
        }
        for n in &self.nets {
            dh.push((format!("rx:{}", n.name), n.rx_rate as u64));
            dh.push((format!("tx:{}", n.name), n.tx_rate as u64));
        }
        for (i, g) in self.gpu_now.iter().enumerate() {
            dh.push((format!("gpu:{i}"), g.util.unwrap_or(0.0).round() as u64));
            dh.push((format!("gmem:{i}"), match (g.mem_used, g.mem_total) { (Some(u), Some(t)) if t > 0 => u * 100 / t, _ => 0 }));
        }
        for (k, v) in dh {
            push(self.dh.entry(k).or_default(), v);
        }
        self.record_sample();
        self.step_replay_clock();
        if self.msg.as_ref().map_or(false, |(_, t)| t.elapsed() > Duration::from_secs(6)) {
            self.msg = None;
        }
        self.poll_bg();
        self.periodic_refresh();
    }

    fn net_totals(&self) -> (f64, f64) {
        self.nets.iter().filter(|n| n.name != "lo").fold((0.0, 0.0), |a, n| (a.0 + n.rx_rate, a.1 + n.tx_rate))
    }

    fn sample_procs(&mut self) {
        let mut new = self.proc_sampler.sample(self.s.per_core_cpu);
        if self.s.smooth_cpu {
            for p in new.iter_mut() {
                let prev = self.smooth.get(&p.pid).copied().unwrap_or(p.cpu);
                p.cpu = prev * 0.5 + p.cpu * 0.5;
                self.smooth.insert(p.pid, p.cpu);
            }
        }
        let alive: HashSet<i32> = new.iter().map(|p| p.pid).collect();
        self.smooth.retain(|k, _| alive.contains(k));
        for old in &self.procs {
            if !alive.contains(&old.pid) {
                let mut e = old.clone();
                e.state = 'X';
                e.cpu = 0.0;
                self.exited.insert(old.pid, (e, Instant::now()));
            }
        }
        self.exited.retain(|pid, (_, t)| t.elapsed() < Duration::from_secs(30) && !alive.contains(pid));
        // app history (cumulative per executable name)
        let tck = proc::clk_tck();
        let mut cur = HashMap::new();
        for p in &new {
            if let Some(&(pt, pr, pw)) = self.ah_prev.get(&p.pid) {
                let e = self.app_hist.entry(p.name.clone()).or_default();
                e.0 += p.cpu_ticks.saturating_sub(pt) as f64 / tck;
                e.1 += p.read_bytes.saturating_sub(pr);
                e.2 += p.write_bytes.saturating_sub(pw);
            }
            cur.insert(p.pid, (p.cpu_ticks, p.read_bytes, p.write_bytes));
        }
        self.ah_prev = cur;
        self.procs = new;
        self.plist = self.procs.clone();
        if self.s.show_exited {
            self.plist.extend(self.exited.values().map(|(p, _)| p.clone()));
        }
    }

    fn record_sample(&mut self) {
        let temps = self.rec_temps();
        let gpu: Vec<f32> = self.gpu_now.iter().map(|g| g.util.unwrap_or(0.0)).collect();
        let power = self.sensors.iter().find(|s| s.label.contains("package")).map(|s| s.value as f32);
        let (rx, tx) = self.net_totals();
        let s = Sample {
            t_ms: 0,
            cpu: self.cpu.per.clone(),
            cpu_overall: self.cpu.overall,
            mem_used: self.mem.used,
            temps: temps.1,
            gpu,
            power_w: power,
            net_rx: rx,
            net_tx: tx,
            disk_read: self.disks.iter().map(|d| d.read_rate).sum(),
            disk_write: self.disks.iter().map(|d| d.write_rate).sum(),
        };
        if let Some(r) = &mut self.rec {
            if r.active {
                let mut s = s;
                s.cpu.resize(r.trace.logical, 0.0);
                s.temps.resize(r.trace.temp_names.len(), 0.0);
                s.gpu.resize(r.trace.gpu_names.len(), 0.0);
                r.push(s);
            }
        }
    }

    pub fn rec_temps(&self) -> (Vec<String>, Vec<f32>) {
        let mut names = Vec::new();
        let mut vals = Vec::new();
        for s in self.sensors.iter().filter(|s| s.group == "Temperatures").take(32) {
            names.push(format!("{} / {}", s.device, s.label));
            vals.push(s.value as f32);
        }
        (names, vals)
    }

    fn step_replay_clock(&mut self) {
        if let Some(r) = &mut self.replay {
            if r.playing && r.last.elapsed() >= Duration::from_millis(1000) {
                r.last = Instant::now();
                if r.pos + 1 < r.trace.samples.len() {
                    r.pos += 1;
                } else {
                    r.playing = false;
                }
            }
        }
    }

    // ───────────── background data ─────────────

    fn poll_bg(&mut self) {
        if let Some(v) = self.startup_bg.poll() {
            self.startup = v;
        }
        if let Some(v) = self.sess_bg.poll() {
            self.sessions = v;
        }
        if let Some(v) = self.svc_bg.poll() {
            self.services = v;
        }
        if let Some(v) = self.conn_bg.poll() {
            self.conns = Some(v);
        }
        if let Some(v) = self.apps_bg.poll() {
            self.apps = v;
            self.apps_loaded = true;
        }
        if let Some(v) = self.mods_bg.poll() {
            self.mods = Some(v);
        }
        if let Some(v) = self.sysinfo_bg.poll() {
            self.sysinfo = v;
        }
    }

    pub fn loading(&self, page: usize) -> bool {
        match page {
            P_STARTUP => self.startup_bg.busy(),
            P_USERS => self.sess_bg.busy(),
            P_SERVICES => self.svc_bg.busy(),
            P_CONN => self.conn_bg.busy(),
            P_APPS => self.apps_bg.busy(),
            P_DRIVERS => self.mods_bg.busy(),
            P_SYSINFO => self.sysinfo_bg.busy(),
            _ => false,
        }
    }

    pub fn refresh_page(&mut self, page: usize) {
        self.loaded_at.insert(page, Instant::now());
        match page {
            P_STARTUP => self.startup_bg.spawn(inv::startup_apps),
            P_USERS => self.sess_bg.spawn(inv::sessions),
            P_SERVICES => self.svc_bg.spawn(inv::services),
            P_CONN => self.conn_bg.spawn(inv::connections),
            P_APPS => self.apps_bg.spawn(inv::installed_apps),
            P_DRIVERS => self.mods_bg.spawn(inv::modules),
            P_SYSINFO => {
                let (c, d, n, g) = (self.cpu_static.clone(), self.disks.clone(), self.nets.clone(), self.gpu_now.clone());
                self.sysinfo_bg.spawn(move || system_info(&c, &d, &n, &g));
            }
            P_DISK | P_BENCH => {
                self.mounts = mounts();
                self.bench_hist = bench::load_history();
            }
            P_RECORDER => self.traces = recorder::list_traces(),
            _ => {}
        }
    }

    fn periodic_refresh(&mut self) {
        let every = match self.page {
            P_CONN => 3,
            P_SERVICES | P_USERS => 5,
            P_DRIVERS => 10,
            P_DISK => 5,
            _ => return,
        };
        if self.loaded_at.get(&self.page).map_or(true, |t| t.elapsed().as_secs() >= every) {
            self.refresh_page(self.page);
        }
    }

    pub fn enter_page(&mut self) {
        if !self.loaded_at.contains_key(&self.page) {
            self.refresh_page(self.page);
        }
    }

    pub fn goto(&mut self, p: usize) {
        self.page = p % PAGES.len();
        self.enter_page();
    }

    // ───────────── derived views ─────────────

    pub fn matches(&self, page: usize, cells: &[&str]) -> bool {
        let f = &self.filter[page];
        if f.is_empty() {
            return true;
        }
        let f = f.to_lowercase();
        cells.iter().any(|c| c.to_lowercase().contains(&f))
    }

    /// Processes page ordering: (tree depth, index into `plist`).
    pub fn proc_view(&self) -> Vec<(usize, usize)> {
        let f = &self.filter[P_PROCESSES];
        let idx_ok = |p: &Proc| f.is_empty() || { let q = f.to_lowercase(); p.name.to_lowercase().contains(&q) || p.user.to_lowercase().contains(&q) || p.pid.to_string() == *f || p.cmdline.to_lowercase().contains(&q) };
        if self.s.process_tree && f.is_empty() {
            return proc::tree_order(&self.plist, &self.collapsed);
        }
        let mut v: Vec<usize> = (0..self.plist.len()).filter(|&i| idx_ok(&self.plist[i])).collect();
        let col = self.s.sort_col.min(PROC_COLS.len() - 1);
        v.sort_by(|&a, &b| {
            let (x, y) = (&self.plist[a], &self.plist[b]);
            let o = match col {
                0 => x.name.to_lowercase().cmp(&y.name.to_lowercase()),
                1 => x.pid.cmp(&y.pid),
                2 => x.cpu.partial_cmp(&y.cpu).unwrap_or(std::cmp::Ordering::Equal),
                3 => x.rss.cmp(&y.rss),
                4 => x.read_rate.partial_cmp(&y.read_rate).unwrap_or(std::cmp::Ordering::Equal),
                5 => x.write_rate.partial_cmp(&y.write_rate).unwrap_or(std::cmp::Ordering::Equal),
                6 => x.user.cmp(&y.user),
                7 => x.state_label().cmp(y.state_label()),
                _ => x.threads.cmp(&y.threads),
            };
            if self.s.sort_desc { o.reverse() } else { o }
        });
        v.into_iter().map(|i| (0, i)).collect()
    }

    /// Keeps the selection on a visible row (first row when nothing valid is selected).
    pub fn fix_selection(&mut self) {
        let view = self.proc_view();
        if !view.iter().any(|(_, i)| self.plist[*i].pid == self.proc_sel) {
            if let Some((_, i)) = view.first() {
                self.proc_sel = self.plist[*i].pid;
            }
        }
    }

    pub fn selected_proc(&self) -> Option<&Proc> {
        self.plist.iter().find(|p| p.pid == self.proc_sel)
    }

    pub fn scope_conns(&self) -> Vec<&inv::Conn> {
        let Some(c) = &self.conns else { return vec![] };
        c.conns
            .iter()
            .filter(|x| self.show_ipc || x.exposure != "Local IPC")
            .filter(|x| match self.conn_scope {
                1 => x.listening,
                2 => x.listening && (x.exposure == "All interfaces" || x.exposure == "Public"),
                _ => true,
            })
            .filter(|x| {
                let owner = x.owners.iter().map(|o| format!("{} (PID {})", o.1, o.0)).collect::<Vec<_>>().join(", ");
                self.matches(P_CONN, &[&x.local, &x.remote, &x.service, &x.state, &owner])
            })
            .collect()
    }

    pub fn module_view(&self) -> Vec<&inv::Module> {
        let Some(m) = &self.mods else { return vec![] };
        m.modules.iter().filter(|x| (!self.mods_loaded_only || x.loaded) && self.matches(P_DRIVERS, &[&x.name, &x.source, &x.state])).collect()
    }

    pub fn app_view(&self) -> Vec<&InstalledApp> {
        self.apps.iter().filter(|a| (self.show_pkgs || !a.packages_only) && self.matches(P_APPS, &[&a.name, &a.publisher, &a.source])).collect()
    }

    pub fn service_view(&self) -> Vec<&Service> {
        self.services.iter().filter(|s| self.matches(P_SERVICES, &[&s.unit, &s.description, &s.active, &s.sub])).collect()
    }

    pub fn startup_view(&self) -> Vec<&Startup> {
        self.startup.iter().filter(|s| self.matches(P_STARTUP, &[&s.name, &s.command])).collect()
    }

    pub fn users_view(&self) -> Vec<(u32, String, Vec<&Session>, usize, f32, u64)> {
        let mut m: HashMap<u32, (String, usize, f32, u64)> = HashMap::new();
        for p in &self.procs {
            let e = m.entry(p.uid).or_insert_with(|| (p.user.clone(), 0, 0.0, 0));
            e.1 += 1;
            e.2 += p.cpu;
            e.3 += p.rss;
        }
        let mut v: Vec<_> = m
            .into_iter()
            .map(|(uid, (name, n, cpu, rss))| (uid, name.clone(), self.sessions.iter().filter(|s| s.uid == uid).collect::<Vec<_>>(), n, cpu, rss))
            .filter(|u| u.3 > 0 && (!u.2.is_empty() || u.0 >= 1000 || u.0 == 0))
            .filter(|u| self.matches(P_USERS, &[&u.1]))
            .collect();
        v.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    pub fn power_view(&self) -> Vec<&Sensor> {
        self.sensors.iter().filter(|s| self.matches(P_POWER, &[&s.label, &s.device, s.group])).collect()
    }

    pub fn app_hist_view(&self) -> Vec<(&String, &(f64, u64, u64))> {
        let mut v: Vec<_> = self.app_hist.iter().filter(|(k, _)| self.matches(P_APPHIST, &[k])).collect();
        v.sort_by(|a, b| b.1 .0.partial_cmp(&a.1 .0).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    pub fn row_count(&self, page: usize) -> usize {
        match page {
            P_PROCESSES => self.proc_view().len(),
            P_APPHIST => self.app_hist_view().len(),
            P_STARTUP => self.startup_view().len(),
            P_USERS => self.users_view().len(),
            P_SERVICES => self.service_view().len(),
            P_POWER => self.power_view().len(),
            P_CONN => self.scope_conns().len(),
            P_APPS => self.app_view().len(),
            P_DRIVERS => self.module_view().len(),
            P_RECORDER => self.traces.len(),
            P_SETTINGS => SETTINGS_ITEMS.len(),
            P_DISK => self.mounts.len(),
            _ => 0,
        }
    }

    // ───────────── keys ─────────────

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.help {
            self.help = false;
            return;
        }
        if self.prompt.is_some() {
            return self.prompt_key(k);
        }
        if let Some(c) = self.confirm.take() {
            return self.confirm_key(c, k);
        }
        if let Some(o) = &mut self.overlay {
            match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.overlay = None,
                KeyCode::Down | KeyCode::Char('j') => o.scroll = o.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => o.scroll = o.scroll.saturating_sub(1),
                KeyCode::PageDown => o.scroll = o.scroll.saturating_add(15),
                KeyCode::PageUp => o.scroll = o.scroll.saturating_sub(15),
                KeyCode::Home => o.scroll = 0,
                _ => {}
            }
            return;
        }
        let replaying = self.replay.is_some() && self.page == P_RECORDER;
        match k.code {
            KeyCode::Char('q') => {
                if self.rec.as_ref().map_or(false, |r| r.saved.is_none() && !r.trace.samples.is_empty()) {
                    self.confirm = Some(Confirm::Quit);
                } else {
                    self.quit = true;
                }
                return;
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.help = true;
                return;
            }
            KeyCode::Tab | KeyCode::Char(']') => return self.goto(self.page + 1),
            KeyCode::BackTab | KeyCode::Char('[') => return self.goto(self.page + PAGES.len() - 1),
            KeyCode::Right if !replaying => return self.goto(self.page + 1),
            KeyCode::Left if !replaying => return self.goto(self.page + PAGES.len() - 1),
            KeyCode::Char('b') => {
                self.s.sidebar = !self.s.sidebar;
                return self.save_settings();
            }
            KeyCode::Char('B') => {
                self.s.status_bar = !self.s.status_bar;
                return self.save_settings();
            }
            KeyCode::Char('N') => {
                self.prompt = Some(Prompt { kind: PromptKind::Run, label: "Type the name of a program, and it will be run".into(), buf: String::new() });
                return;
            }
            KeyCode::Char('/') if !matches!(self.page, P_SUMMARY | P_PERF | P_SYSINFO | P_BENCH | P_SETTINGS | P_DISK | P_RECORDER) => {
                self.prompt = Some(Prompt { kind: PromptKind::Filter, label: format!("Filter {}", PAGES[self.page]), buf: self.filter[self.page].clone() });
                return;
            }
            KeyCode::Esc if !self.filter[self.page].is_empty() => {
                self.filter[self.page].clear();
                return;
            }
            _ => {}
        }
        // list navigation
        let n = self.row_count(self.page);
        if self.page != P_PROCESSES && !(self.page == P_SYSINFO) {
            let sel = &mut self.sel[self.page];
            let sel_before = *sel;
            match k.code {
                KeyCode::Down | KeyCode::Char('j') => *sel = (*sel + 1).min(n.saturating_sub(1)),
                KeyCode::Up | KeyCode::Char('k') if self.page != P_SERVICES => *sel = sel.saturating_sub(1),
                KeyCode::PageDown => *sel = (*sel + 15).min(n.saturating_sub(1)),
                KeyCode::PageUp => *sel = sel.saturating_sub(15),
                KeyCode::Home => *sel = 0,
                KeyCode::End => *sel = n.saturating_sub(1),
                _ => {}
            }
            if *sel != sel_before {
                return;
            }
            *sel = (*sel).min(n.saturating_sub(1));
        }
        match self.page {
            P_PROCESSES => self.key_processes(k),
            P_PERF => self.key_perf(k),
            P_STARTUP => self.key_startup(k),
            P_USERS => self.key_users(k),
            P_SERVICES => self.key_services(k),
            P_SYSINFO => self.key_sysinfo(k),
            P_RECORDER => self.key_recorder(k),
            P_CONN => self.key_conn(k),
            P_APPS => {
                match k.code {
                    KeyCode::Char('p') => self.show_pkgs = !self.show_pkgs,
                    KeyCode::Char('r') => self.refresh_page(P_APPS),
                    _ => {}
                }
            }
            P_DRIVERS => match k.code {
                KeyCode::Char('l') => self.mods_loaded_only = !self.mods_loaded_only,
                KeyCode::Char('r') => self.refresh_page(P_DRIVERS),
                KeyCode::Enter => {
                    let v = self.module_view();
                    if let Some(m) = v.get(self.sel[P_DRIVERS]) {
                        let t = format!(
                            "{}\n\nState:        {}\nSource:       {}\nSize:         {}\nReferences:   {}\nUsed by:      {}\nDepends on:   {}\nVersion:      {}\nSource ver.:  {}\nTaint flags:  {}\nFile path:    {}\n\n{}\n",
                            m.name, m.state, m.source, bytes(m.size as f64), m.refs, m.used_by.join(", "), m.depends.join(", "), m.version, m.srcversion, m.taint, m.path, inv::unload_hint(m)
                        );
                        self.overlay = Some(Overlay { title: format!("Module details: {}", m.name), text: t, scroll: 0 });
                    }
                }
                _ => {}
            },
            P_APPHIST => {
                if k.code == KeyCode::Char('C') {
                    self.app_hist.clear();
                    self.say("App history cleared.");
                }
            }
            P_DISK => self.key_disk(k),
            P_BENCH => self.key_bench(k),
            P_SETTINGS => self.key_settings(k),
            _ => {}
        }
    }

    fn prompt_key(&mut self, k: KeyEvent) {
        let Some(p) = &mut self.prompt else { return };
        match k.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Backspace => {
                p.buf.pop();
                if matches!(p.kind, PromptKind::Filter) {
                    self.filter[self.page] = p.buf.clone();
                }
            }
            KeyCode::Char(c) => {
                p.buf.push(c);
                if matches!(p.kind, PromptKind::Filter) {
                    self.filter[self.page] = p.buf.clone();
                    self.sel[self.page] = 0;
                }
            }
            KeyCode::Enter => {
                let p = self.prompt.take().unwrap();
                self.prompt_done(p);
            }
            _ => {}
        }
    }

    fn prompt_done(&mut self, p: Prompt) {
        let v = p.buf.trim().to_string();
        match p.kind {
            PromptKind::Filter => self.filter[self.page] = v,
            PromptKind::Run => {
                if v.is_empty() {
                    return;
                }
                match std::process::Command::new("sh").arg("-c").arg(&v).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
                    Ok(_) => self.say(format!("Started: {v}")),
                    Err(_) => self.say(format!("Could not run \"{v}\".")),
                }
            }
            PromptKind::Nice(pid) => match v.parse::<i32>() {
                Ok(n) if (-20..=19).contains(&n) => match proc::set_nice(pid, n) {
                    Ok(_) => self.say(format!("Priority of {pid} set to {n}")),
                    Err(e) => self.say(format!("Set priority failed: {e}")),
                },
                _ => self.say("Nice value must be between -20 and 19"),
            },
            PromptKind::Affinity(pid) => match proc::parse_cpu_list(&v, self.cpu_static.logical).and_then(|l| proc::set_affinity(pid, &l)) {
                Ok((a, t)) => self.say(format!("Set affinity applied to {a} of {t} threads")),
                Err(e) => self.say(e),
            },
            PromptKind::Signal(pid) => {
                let sig = match v.to_uppercase().trim_start_matches("SIG") {
                    "HUP" => Some(1),
                    "INT" => Some(2),
                    "QUIT" => Some(3),
                    "KILL" => Some(9),
                    "USR1" => Some(10),
                    "USR2" => Some(12),
                    "TERM" => Some(15),
                    "CONT" => Some(18),
                    "STOP" => Some(19),
                    o => o.parse::<i32>().ok().filter(|n| (1..=64).contains(n)),
                };
                match sig {
                    Some(s) => self.do_signal(&[pid], s),
                    None => self.say("Unknown signal"),
                }
            }
            PromptKind::ScanPath => {
                let pb = PathBuf::from(if v.is_empty() { "/".into() } else { v });
                if pb.is_dir() {
                    self.scanner.start(pb);
                } else {
                    self.say("Choose a folder to measure");
                }
            }
            PromptKind::BenchDir => {
                let pb = PathBuf::from(&v);
                if pb.is_dir() {
                    self.s.bench_dir = v;
                    self.bench_dir = pb;
                    self.save_settings();
                } else {
                    self.say("That is not a folder.");
                }
            }
            PromptKind::TraceFile => {
                let pb = PathBuf::from(&v);
                self.open_replay(&pb);
            }
        }
    }

    fn confirm_key(&mut self, c: Confirm, k: KeyEvent) {
        if !matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter) {
            return;
        }
        match c {
            Confirm::Signal { pids, sig, .. } => self.do_signal(&pids, sig),
            Confirm::StopScan => self.scanner.stop(),
            Confirm::ClearHistory => {
                self.bench_hist.clear();
                if let Err(e) = bench::save_history(&[]) {
                    self.say(format!("Benchmark history was not saved: {e}"));
                }
            }
            Confirm::Disconnect(id, user) => match inv::terminate_session(&id) {
                Ok(_) => self.say(format!("Signed off session {id} of {user}")),
                Err(e) => self.say(e),
            },
            Confirm::Quit => self.quit = true,
            Confirm::DiscardTrace => self.rec = None,
            Confirm::ResetSettings => {
                self.s = Settings::default();
                self.save_settings();
                self.say("Settings reset.");
            }
        }
    }

    pub fn do_signal(&mut self, pids: &[i32], sig: i32) {
        let (mut ok, mut last) = (0, String::new());
        for &p in pids {
            match proc::signal(p, sig) {
                Ok(_) => ok += 1,
                Err(e) => last = e,
            }
        }
        if ok == pids.len() {
            self.say(format!("Signal {sig} sent to {ok} process(es)"));
        } else {
            self.say(format!("Process action failed: {last}"));
        }
    }

    fn key_processes(&mut self, k: KeyEvent) {
        self.fix_selection();
        let view = self.proc_view();
        let cur = view.iter().position(|(_, i)| self.plist[*i].pid == self.proc_sel).unwrap_or(0);
        let mv = |me: &mut App, idx: usize| {
            if let Some((_, i)) = view.get(idx) {
                me.proc_sel = me.plist[*i].pid;
            }
        };
        let last = view.len().saturating_sub(1);
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => mv(self, (cur + 1).min(last)),
            KeyCode::Up | KeyCode::Char('k') => mv(self, cur.saturating_sub(1)),
            KeyCode::PageDown => mv(self, (cur + 15).min(last)),
            KeyCode::PageUp => mv(self, cur.saturating_sub(15)),
            KeyCode::Home => mv(self, 0),
            KeyCode::End => mv(self, last),
            KeyCode::Char('>') | KeyCode::Char('.') => self.s.sort_col = (self.s.sort_col + 1) % PROC_COLS.len(),
            KeyCode::Char('<') | KeyCode::Char(',') => self.s.sort_col = (self.s.sort_col + PROC_COLS.len() - 1) % PROC_COLS.len(),
            KeyCode::Char('o') => self.s.sort_desc = !self.s.sort_desc,
            KeyCode::Char('t') => {
                self.s.process_tree = !self.s.process_tree;
                self.save_settings();
            }
            KeyCode::Char('e') => {
                self.s.show_exited = !self.s.show_exited;
                self.save_settings();
            }
            KeyCode::Char('v') => self.proc_values = !self.proc_values,
            KeyCode::Char(' ') => {
                if self.collapsed.contains(&self.proc_sel) {
                    self.collapsed.remove(&self.proc_sel);
                } else {
                    self.collapsed.insert(self.proc_sel);
                }
            }
            KeyCode::Char('E') => {
                let parents: HashSet<i32> = self.plist.iter().map(|p| p.ppid).collect();
                if self.collapsed.is_empty() {
                    self.collapsed = self.plist.iter().filter(|p| parents.contains(&p.pid)).map(|p| p.pid).collect();
                } else {
                    self.collapsed.clear();
                }
            }
            KeyCode::Enter | KeyCode::Char('i') => {
                let pid = self.proc_sel;
                self.overlay = Some(Overlay { title: format!("Linux process details: {} (PID {pid})", self.selected_proc().map(|p| p.name.clone()).unwrap_or_default()), text: proc::detail(pid), scroll: 0 });
            }
            KeyCode::Char('x') | KeyCode::Char('X') | KeyCode::Char('K') => {
                let Some(p) = self.selected_proc().cloned() else { return };
                let tree = k.code == KeyCode::Char('X');
                let sig = if k.code == KeyCode::Char('K') { 9 } else { 15 };
                let mut pids = vec![p.pid];
                if tree {
                    pids.extend(proc::descendants(&self.procs, p.pid));
                }
                let how = if sig == 9 { "Force terminate" } else { "Terminate" };
                let text = if tree && pids.len() > 1 {
                    format!("{how} {} and its {} descendants? Unsaved data may be lost.", p.name, pids.len() - 1)
                } else if sig == 9 {
                    format!("Force terminate {}? It is not given the chance to clean up, so unsaved data will be lost.", p.name)
                } else {
                    format!("Terminate {}? Unsaved data may be lost.", p.name)
                };
                self.confirm = Some(Confirm::Signal { pids, sig, text });
            }
            KeyCode::Char('p') => {
                let pid = self.proc_sel;
                self.do_signal(&[pid], 19);
            }
            KeyCode::Char('c') => {
                let pid = self.proc_sel;
                self.do_signal(&[pid], 18);
            }
            KeyCode::Char('s') => self.prompt = Some(Prompt { kind: PromptKind::Signal(self.proc_sel), label: "Send signal (name or number: HUP INT TERM KILL STOP CONT USR1 USR2)".into(), buf: String::new() }),
            KeyCode::Char('n') => {
                let cur = self.selected_proc().map(|p| p.nice).unwrap_or(0);
                self.prompt = Some(Prompt { kind: PromptKind::Nice(self.proc_sel), label: "Nice priority (-20 highest .. 19 lowest; 0 normal, 10 low, 19 lowest)".into(), buf: cur.to_string() });
            }
            KeyCode::Char('a') => {
                let cur = proc::get_affinity(self.proc_sel).map(|v| v.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",")).unwrap_or_default();
                let max = self.cpu_static.logical;
                if max > 1024 {
                    return self.say("Unavailable on systems with more than 1024 processors");
                }
                self.prompt = Some(Prompt { kind: PromptKind::Affinity(self.proc_sel), label: format!("Set affinity: CPU list, e.g. 0-3,6 (0..{})", max - 1), buf: cur });
            }
            KeyCode::Char('C') => {
                if let Some(p) = self.selected_proc() {
                    let t = if p.cmdline.is_empty() { p.name.clone() } else { p.cmdline.clone() };
                    crate::draw::copy_osc52(&t);
                    self.say("Command line copied");
                }
            }
            KeyCode::Char('Y') => {
                if let Some(p) = self.selected_proc() {
                    crate::draw::copy_osc52(&p.name);
                    self.say("Process name copied");
                }
            }
            KeyCode::Char('W') => {
                if let Some(p) = self.selected_proc() {
                    let q = p.name.replace(' ', "+");
                    let _ = std::process::Command::new("xdg-open").arg(format!("https://duckduckgo.com/?q={q}")).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
                    self.say("Searching online…");
                }
            }
            KeyCode::Char('m') => match proc::sample_report(&self.procs, &data_dir().join("reports")) {
                Ok(p) => self.say(format!("The file was written to: {}", p.display())),
                Err(e) => self.say(format!("The sample report could not be written: {e}")),
            },
            _ => {}
        }
    }

    fn key_perf(&mut self, k: KeyEvent) {
        let n = self.perf_devices().len();
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.perf_dev = (self.perf_dev + 1).min(n.saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') => self.perf_dev = self.perf_dev.saturating_sub(1),
            KeyCode::Char('g') => {
                self.cpu_graph = match self.cpu_graph {
                    CpuGraph::Overall => CpuGraph::Logical,
                    CpuGraph::Logical => CpuGraph::Classes,
                    CpuGraph::Classes => CpuGraph::Overall,
                }
            }
            KeyCode::Char('K') => self.show_kernel = !self.show_kernel,
            KeyCode::Char('c') => {
                self.s.combined_disks = !self.s.combined_disks;
                self.s.combined_network = self.s.combined_disks;
                self.s.combined_gpus = self.s.combined_disks;
                self.save_settings();
            }
            _ => {}
        }
    }

    pub fn perf_devices(&self) -> Vec<String> {
        let mut v = vec!["CPU".to_string(), "Memory".to_string()];
        if self.s.combined_disks {
            v.push("All disks".into());
        } else {
            v.extend(self.disks.iter().map(|d| format!("Disk {}", d.name)));
        }
        if self.s.combined_network {
            v.push("All networks".into());
        } else {
            v.extend(self.nets.iter().filter(|n| n.name != "lo").map(|n| format!("{} {}", if n.wireless { "Wi-Fi" } else { "Network" }, n.name)));
        }
        if self.s.combined_gpus && self.gpu_now.len() > 1 {
            v.push("All GPUs".into());
        } else {
            v.extend(self.gpu_now.iter().enumerate().map(|(i, g)| format!("GPU {i} {}", g.vendor)));
        }
        v
    }

    fn key_startup(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char(' ') | KeyCode::Enter | KeyCode::Char('e') => {
                let item = self.startup_view().get(self.sel[P_STARTUP]).map(|s| (*s).clone());
                if let Some(s) = item {
                    match inv::set_startup_enabled(&s, !s.enabled) {
                        Ok(_) => {
                            self.say(format!("{} {}", if s.enabled { "Disabled" } else { "Enabled" }, s.name));
                            self.refresh_page(P_STARTUP);
                        }
                        Err(e) => self.say(format!("Could not change the enabled state for \"{}\": {e}", s.name)),
                    }
                }
            }
            KeyCode::Char('r') => self.refresh_page(P_STARTUP),
            KeyCode::Char('o') => {
                if let Some(s) = self.startup_view().get(self.sel[P_STARTUP]) {
                    let m = format!("File: {}", s.file);
                    self.say(m);
                }
            }
            _ => {}
        }
    }

    fn key_users(&mut self, k: KeyEvent) {
        let sel = self.users_view().get(self.sel[P_USERS]).map(|u| (u.0, u.1.clone(), u.2.iter().map(|s| s.id.clone()).collect::<Vec<_>>()));
        match k.code {
            KeyCode::Char('l') => {
                if let Some((_, _, ids)) = sel {
                    for id in ids {
                        match inv::lock_session(&id) {
                            Ok(_) => self.say(format!("Locked session {id}")),
                            Err(e) => self.say(e),
                        }
                    }
                }
            }
            KeyCode::Char('d') => {
                if let Some((_, user, ids)) = sel {
                    if let Some(id) = ids.first() {
                        let own = ids.len() == 1 && user == std::env::var("USER").unwrap_or_default();
                        let txt = if own { "This ends your own session and closes everything running in it. Continue?" } else { "This ends the session and closes everything running in it. Continue?" };
                        let _ = txt;
                        self.confirm = Some(Confirm::Disconnect(id.clone(), user));
                    } else {
                        self.say("That user has no login session to disconnect.");
                    }
                }
            }
            KeyCode::Char('M') => {
                if let Err(e) = inv::manage_accounts() {
                    self.say(e);
                }
            }
            KeyCode::Char('r') => self.refresh_page(P_USERS),
            _ => {}
        }
    }

    fn key_services(&mut self, k: KeyEvent) {
        let sel = self.service_view().get(self.sel[P_SERVICES]).map(|s| (*s).clone());
        let act = |me: &mut App, s: &Service, a: &str| match inv::service_action(&s.unit, a) {
            Ok(_) => {
                me.say(format!("{a} {} succeeded", s.unit));
                me.refresh_page(P_SERVICES);
            }
            Err(e) => me.say(e),
        };
        match (k.code, sel) {
            (KeyCode::Char('s'), Some(s)) => act(self, &s, "start"),
            (KeyCode::Char('x'), Some(s)) => act(self, &s, "stop"),
            (KeyCode::Char('R'), Some(s)) => act(self, &s, "restart"),
            (KeyCode::Char('e'), Some(s)) => {
                let a = if s.enabled == "enabled" { "disable" } else { "enable" };
                act(self, &s, a)
            }
            (KeyCode::Char('g'), Some(s)) => {
                if let Some(pid) = s.pid {
                    self.proc_sel = pid;
                    self.goto(P_PROCESSES);
                } else {
                    self.say("This service has no running main process.");
                }
            }
            (KeyCode::Char('r'), _) => self.refresh_page(P_SERVICES),
            (KeyCode::Up | KeyCode::Char('k'), _) => self.sel[P_SERVICES] = self.sel[P_SERVICES].saturating_sub(1),
            _ => {}
        }
    }

    fn key_sysinfo(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.sysinfo_scroll = self.sysinfo_scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.sysinfo_scroll = self.sysinfo_scroll.saturating_sub(1),
            KeyCode::PageDown => self.sysinfo_scroll = self.sysinfo_scroll.saturating_add(15),
            KeyCode::PageUp => self.sysinfo_scroll = self.sysinfo_scroll.saturating_sub(15),
            KeyCode::Home => self.sysinfo_scroll = 0,
            KeyCode::Char('r') => self.refresh_page(P_SYSINFO),
            KeyCode::Char('E') => {
                let dir = data_dir().join("exports");
                let _ = std::fs::create_dir_all(&dir);
                let p = dir.join("system-information.txt");
                match std::fs::write(&p, export_info(&self.sysinfo)) {
                    Ok(_) => self.say(format!("Exported to {}", p.display())),
                    Err(e) => self.say(format!("Export failed: {e}")),
                }
            }
            _ => {}
        }
    }

    fn key_conn(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('a') => self.conn_scope = 0,
            KeyCode::Char('l') => self.conn_scope = 1,
            KeyCode::Char('p') => self.conn_scope = 2,
            KeyCode::Char('t') => self.show_ipc = !self.show_ipc,
            KeyCode::Char('r') => self.refresh_page(P_CONN),
            KeyCode::Char('g') => {
                let pid = self.scope_conns().get(self.sel[P_CONN]).and_then(|c| c.owners.first().map(|o| o.0));
                if let Some(pid) = pid {
                    self.proc_sel = pid;
                    self.goto(P_PROCESSES);
                }
            }
            _ => {}
        }
    }

    fn key_disk(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Enter | KeyCode::Char('s') => {
                let root = self.mounts.get(self.sel[P_DISK]).map(|m| PathBuf::from(&m.mount)).unwrap_or_else(|| PathBuf::from("/"));
                self.scanner.start(root);
            }
            KeyCode::Char('c') => self.prompt = Some(Prompt { kind: PromptKind::ScanPath, label: "Choose a folder to measure".into(), buf: home().to_string_lossy().into_owned() }),
            KeyCode::Char(' ') => self.scanner.toggle_pause(),
            KeyCode::Char('x') => {
                if self.scanner.running() {
                    if self.s.confirm_stop_scan {
                        self.confirm = Some(Confirm::StopScan);
                    } else {
                        self.scanner.stop();
                    }
                }
            }
            KeyCode::Char('v') => self.disk_view = (self.disk_view + 1) % 3,
            KeyCode::Char('r') => self.refresh_page(P_DISK),
            _ => {}
        }
    }

    fn key_bench(&mut self, k: KeyEvent) {
        let dir = self.bench_dir.clone();
        match k.code {
            KeyCode::Char('a') => self.bench.start(Kind::All, dir),
            KeyCode::Char('c') => self.bench.start(Kind::Cpu, dir),
            KeyCode::Char('d') => self.bench.start(Kind::Disk, dir),
            KeyCode::Char('i') => self.bench.start(Kind::Internet, dir),
            KeyCode::Char('f') => self.prompt = Some(Prompt { kind: PromptKind::BenchDir, label: "Choose benchmark folder".into(), buf: self.bench_dir.to_string_lossy().into_owned() }),
            KeyCode::Char('h') => self.bench_show_hist = !self.bench_show_hist,
            KeyCode::Char('C') => self.confirm = Some(Confirm::ClearHistory),
            KeyCode::Esc => self.bench.cancel(),
            _ => {}
        }
    }

    fn key_settings(&mut self, k: KeyEvent) {
        if !matches!(k.code, KeyCode::Enter | KeyCode::Char(' ')) {
            return;
        }
        let i = self.sel[P_SETTINGS];
        let s = &mut self.s;
        match i {
            0 => s.fahrenheit = !s.fahrenheit,
            1 => s.show_exited = !s.show_exited,
            2 => s.smooth_cpu = !s.smooth_cpu,
            3 => s.process_tree = !s.process_tree,
            4 => s.per_core_cpu = !s.per_core_cpu,
            5 => s.combined_gpus = !s.combined_gpus,
            6 => s.combined_disks = !s.combined_disks,
            7 => s.combined_network = !s.combined_network,
            8 => s.status_bar = !s.status_bar,
            9 => s.sidebar = !s.sidebar,
            10 => s.refresh_ms = match s.refresh_ms { 500 => 1000, 1000 => 2000, 2000 => 5000, _ => 500 },
            11 => s.light_theme = !s.light_theme,
            12 => s.confirm_stop_scan = !s.confirm_stop_scan,
            13 => {
                self.confirm = Some(Confirm::ResetSettings);
                return;
            }
            _ => {}
        }
        self.save_settings();
    }

    fn key_recorder(&mut self, k: KeyEvent) {
        if self.replay.is_some() {
            let r = self.replay.as_mut().unwrap();
            let last = r.trace.samples.len() - 1;
            match k.code {
                KeyCode::Left | KeyCode::Char(',') => r.pos = r.pos.saturating_sub(1),
                KeyCode::Right | KeyCode::Char('.') => r.pos = (r.pos + 1).min(last),
                KeyCode::PageUp => r.pos = r.pos.saturating_sub(30),
                KeyCode::PageDown => r.pos = (r.pos + 30).min(last),
                KeyCode::Home => r.pos = 0,
                KeyCode::End => r.pos = last,
                KeyCode::Char(' ') => {
                    r.playing = !r.playing;
                    r.last = Instant::now();
                }
                KeyCode::Char('R') => {
                    r.pos = 0;
                    r.playing = true;
                }
                KeyCode::Char('x') | KeyCode::Esc => self.replay = None,
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('r') | KeyCode::Char(' ') if !self.trace_pick => {
                if self.rec.as_ref().map_or(false, |r| r.active) {
                    if let Some(r) = &mut self.rec {
                        r.active = false;
                    }
                    self.say("Recording stopped. Press s to save or d to discard.");
                } else if self.rec.is_some() {
                    self.say("Save or discard the captured trace first.");
                } else {
                    let (names, _) = self.rec_temps();
                    let gnames = self.gpu_now.iter().map(|g| g.name.clone()).collect();
                    self.rec = Some(Recorder::start(uname().0, self.cpu_static.logical, self.mem.total, names, gnames));
                }
            }
            KeyCode::Char('s') => {
                if let Some(r) = &mut self.rec {
                    match r.save() {
                        Ok(p) => {
                            self.say(format!("Saved {}", p.display()));
                            self.rec = None;
                            self.traces = recorder::list_traces();
                        }
                        Err(e) => self.say(format!("The Flight Recorder trace could not be saved: {e}")),
                    }
                }
            }
            KeyCode::Char('d') => {
                if self.rec.is_some() {
                    self.confirm = Some(Confirm::DiscardTrace);
                }
            }
            KeyCode::Char('l') | KeyCode::Enter => {
                if self.rec.is_some() {
                    return self.say("Close the recording before opening a replay.");
                }
                if let Some(p) = self.traces.get(self.sel[P_RECORDER]).cloned() {
                    self.open_replay(&p);
                } else {
                    self.say("No saved traces. Press o to open a trace file by path.");
                }
            }
            KeyCode::Char('o') => self.prompt = Some(Prompt { kind: PromptKind::TraceFile, label: "Choose a Flight Recorder trace to replay".into(), buf: String::new() }),
            KeyCode::Char('R') => self.traces = recorder::list_traces(),
            _ => {}
        }
    }

    pub fn open_replay(&mut self, p: &std::path::Path) {
        match recorder::load(p) {
            Ok(t) => self.replay = Some(Replay { trace: t, pos: 0, playing: true, last: Instant::now() }),
            Err(e) => self.say(e),
        }
    }
}

pub const SETTINGS_ITEMS: [&str; 14] = [
    "Temperature unit (Celsius / Fahrenheit)",
    "Show exited processes",
    "Smooth CPU readings",
    "Process tree",
    "CPU per core (100% = one core)",
    "Show combined GPUs",
    "Show combined disks",
    "Show combined network",
    "Status bar",
    "Sidebar",
    "Refresh interval",
    "Light theme accents",
    "Confirm before stopping a disk scan",
    "Reset all settings…",
];
