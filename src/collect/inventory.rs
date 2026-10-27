use crate::util::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

// ───────────────────────────── Connections ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Conn {
    pub proto: &'static str,
    pub state: String,
    pub local: String,
    pub remote: String,
    pub service: String,
    pub exposure: &'static str,
    pub listening: bool,
    pub uid: u32,
    pub inode: u64,
    pub owners: Vec<(i32, String)>,
}

fn parse_v4(h: &str) -> Option<Ipv4Addr> {
    let v = u32::from_str_radix(h, 16).ok()?;
    let b = v.to_le_bytes();
    Some(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
}

fn parse_v6(h: &str) -> Option<Ipv6Addr> {
    if h.len() != 32 {
        return None;
    }
    let mut b = [0u8; 16];
    for w in 0..4 {
        let v = u32::from_str_radix(&h[w * 8..w * 8 + 8], 16).ok()?;
        b[w * 4..w * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    Some(Ipv6Addr::from(b))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Scope {
    Unspecified,
    Loopback,
    Local,
    Public,
}

fn scope_v4(a: Ipv4Addr) -> Scope {
    if a.is_unspecified() {
        Scope::Unspecified
    } else if a.is_loopback() {
        Scope::Loopback
    } else if a.is_private() || a.is_link_local() || (a.octets()[0] == 100 && (a.octets()[1] & 0xc0) == 0x40) {
        Scope::Local
    } else {
        Scope::Public
    }
}

fn scope_v6(a: Ipv6Addr) -> Scope {
    if a.is_unspecified() {
        Scope::Unspecified
    } else if a.is_loopback() {
        Scope::Loopback
    } else if let Some(v4) = a.to_ipv4_mapped() {
        scope_v4(v4)
    } else if (a.segments()[0] & 0xfe00) == 0xfc00 || (a.segments()[0] & 0xffc0) == 0xfe80 {
        Scope::Local
    } else {
        Scope::Public
    }
}

pub fn service_name(port: u16) -> &'static str {
    match port {
        20 | 21 => "FTP",
        22 => "SSH",
        25 => "SMTP",
        53 => "DNS",
        67 | 68 => "DHCP",
        80 => "HTTP",
        110 => "POP3",
        123 => "NTP",
        143 => "IMAP",
        161 => "SNMP",
        389 => "LDAP",
        443 => "HTTPS",
        445 => "SMB",
        465 => "SMTPS",
        548 => "AFP",
        587 => "SMTP submission",
        631 => "IPP printing",
        993 => "IMAPS",
        995 => "POP3S",
        1714..=1764 => "KDE Connect",
        3306 => "MySQL",
        3389 => "RDP",
        5353 => "mDNS",
        5432 => "PostgreSQL",
        5900 => "Screen Sharing",
        6379 => "Redis",
        8080 | 8000 | 8888 => "HTTP alt",
        8443 => "HTTPS alt",
        _ => "",
    }
}

fn socket_owners() -> HashMap<u64, Vec<(i32, String)>> {
    let mut m: HashMap<u64, Vec<(i32, String)>> = HashMap::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return m };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        let mut comm = None;
        for fd in fds.flatten() {
            let Ok(t) = std::fs::read_link(fd.path()) else { continue };
            let t = t.to_string_lossy();
            if let Some(i) = t.strip_prefix("socket:[").and_then(|x| x.strip_suffix(']')).and_then(|x| x.parse::<u64>().ok()) {
                let c = comm.get_or_insert_with(|| read(&format!("/proc/{pid}/comm")).unwrap_or_default()).clone();
                let v = m.entry(i).or_default();
                if !v.iter().any(|(p, _)| *p == pid) {
                    v.push((pid, c));
                }
            }
        }
    }
    m
}

pub struct ConnResult {
    pub conns: Vec<Conn>,
    pub warnings: Vec<String>,
}

pub fn connections() -> ConnResult {
    let owners = socket_owners();
    let mut conns = Vec::new();
    let mut warnings = Vec::new();
    let tcp_states = ["", "Established", "SYN sent", "SYN received", "FIN wait 1", "FIN wait 2", "Time wait", "Closed", "Close wait", "Last ACK", "Listening", "Closing"];
    let mut any = false;
    for (file, proto, v6) in [("/proc/net/tcp", "TCP", false), ("/proc/net/tcp6", "TCP6", true), ("/proc/net/udp", "UDP", false), ("/proc/net/udp6", "UDP6", true)] {
        let Some(txt) = std::fs::read_to_string(file).ok() else {
            warnings.push(format!("{file} could not be read; results are incomplete."));
            continue;
        };
        any = true;
        for l in txt.lines().skip(1) {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 {
                continue;
            }
            let (lh, lp) = f[1].split_once(':').unwrap_or(("", "0"));
            let (rh, rp) = f[2].split_once(':').unwrap_or(("", "0"));
            let lport = u16::from_str_radix(lp, 16).unwrap_or(0);
            let rport = u16::from_str_radix(rp, 16).unwrap_or(0);
            let (la, ra, ls, rs) = if v6 {
                let (a, b) = (parse_v6(lh), parse_v6(rh));
                (a.map(|x| format!("[{x}]")), b.map(|x| format!("[{x}]")), a.map(scope_v6), b.map(scope_v6))
            } else {
                let (a, b) = (parse_v4(lh), parse_v4(rh));
                (a.map(|x| x.to_string()), b.map(|x| x.to_string()), a.map(scope_v4), b.map(scope_v4))
            };
            let (Some(la), Some(ra), Some(ls), Some(rs)) = (la, ra, ls, rs) else { continue };
            let st = u8::from_str_radix(f[3], 16).unwrap_or(0) as usize;
            let tcp = proto.starts_with("TCP");
            let listening = if tcp { st == 10 } else { rs == Scope::Unspecified && rport == 0 };
            let state = if tcp {
                tcp_states.get(st).copied().unwrap_or("Unknown").to_string()
            } else if listening {
                "Datagram".into()
            } else {
                "Connected".into()
            };
            let exposure = if listening {
                match ls {
                    Scope::Unspecified => "All interfaces",
                    Scope::Loopback => "Loopback",
                    Scope::Local => "Local network",
                    Scope::Public => "Public",
                }
            } else {
                match rs {
                    Scope::Loopback => "Loopback",
                    Scope::Local => "Local network",
                    _ => "Public",
                }
            };
            let inode: u64 = f[9].parse().unwrap_or(0);
            let svc_port = if listening { lport } else { rport };
            conns.push(Conn {
                proto,
                state,
                local: format!("{la}:{lport}"),
                remote: if listening { "*".into() } else { format!("{ra}:{rport}") },
                service: service_name(svc_port).into(),
                exposure,
                listening,
                uid: f[7].parse().unwrap_or(0),
                inode,
                owners: owners.get(&inode).cloned().unwrap_or_default(),
            });
        }
    }
    if let Some(txt) = std::fs::read_to_string("/proc/net/unix").ok() {
        any = true;
        for l in txt.lines().skip(1) {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 7 {
                continue;
            }
            let ty = match f[4] {
                "0001" => "UNIX stream",
                "0002" => "UNIX datagram",
                _ => "UNIX",
            };
            let inode: u64 = f[6].parse().unwrap_or(0);
            let path = f.get(7).copied().unwrap_or("");
            let listening = u32::from_str_radix(f[3], 16).unwrap_or(0) & 0x10000 != 0;
            conns.push(Conn {
                proto: ty,
                state: if listening { "Listening".into() } else if f[5] == "03" { "Connected".into() } else { "Unconnected".into() },
                local: if path.is_empty() { "Unnamed socket".into() } else { path.to_string() },
                remote: "-".into(),
                service: String::new(),
                exposure: "Local IPC",
                listening,
                uid: 0,
                inode,
                owners: owners.get(&inode).cloned().unwrap_or_default(),
            });
        }
    }
    if !any {
        warnings.push("Socket tables are unavailable in this network namespace.".into());
    }
    if conns.iter().any(|c| c.owners.is_empty() && c.inode != 0) {
        warnings.push("Socket owners are unavailable for sockets of other users; run as root to see them all.".into());
    }
    ConnResult { conns, warnings }
}

// ───────────────────────────── Kernel modules ─────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub name: String,
    pub size: u64,
    pub refs: i64,
    pub used_by: Vec<String>,
    pub state: String,
    pub source: String,
    pub version: String,
    pub srcversion: String,
    pub depends: Vec<String>,
    pub path: String,
    pub taint: String,
    pub builtin: bool,
    pub loaded: bool,
}

pub struct ModuleResult {
    pub modules: Vec<Module>,
    pub kernel_release: String,
    pub notes: Vec<String>,
    pub slab: u64,
}

pub fn modules() -> ModuleResult {
    let (_, rel, _) = super::system::uname();
    let mut notes = Vec::new();
    let mut map: BTreeMap<String, Module> = BTreeMap::new();
    match std::fs::read_to_string("/proc/modules") {
        Ok(t) => {
            for l in t.lines() {
                let f: Vec<&str> = l.split_whitespace().collect();
                if f.len() < 5 {
                    continue;
                }
                let name = f[0].to_string();
                let used: Vec<String> = f[3].trim_end_matches(',').split(',').filter(|s| !s.is_empty() && *s != "-").map(String::from).collect();
                let state = match f[4] {
                    "Live" => "Loaded",
                    "Loading" => "Loading",
                    "Unloading" => "Unloading",
                    o => o,
                };
                map.insert(
                    name.clone(),
                    Module { name, size: f[1].parse().unwrap_or(0), refs: f[2].parse().unwrap_or(0), used_by: used, state: state.into(), loaded: true, ..Default::default() },
                );
            }
        }
        Err(_) => notes.push("Loaded-module states are unavailable; showing the on-disk inventory only.".into()),
    }
    let dep = format!("/lib/modules/{rel}/modules.dep");
    match std::fs::read_to_string(&dep) {
        Ok(t) => {
            let mut count = 0;
            for l in t.lines() {
                let Some((path, deps)) = l.split_once(':') else { continue };
                let name = Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let name = name.split('.').next().unwrap_or("").replace('-', "_");
                if name.is_empty() {
                    continue;
                }
                count += 1;
                let e = map.entry(name.clone()).or_insert_with(|| Module { name: name.clone(), state: "Not loaded".into(), ..Default::default() });
                e.path = format!("/lib/modules/{rel}/{path}");
                e.depends = deps.split_whitespace().filter_map(|d| Path::new(d).file_name().map(|s| s.to_string_lossy().split('.').next().unwrap_or("").replace('-', "_"))).collect();
                if count >= 20000 {
                    notes.push("The installed-module list stopped at 20000 entries; modules that are not loaded may be missing from it.".into());
                    break;
                }
            }
        }
        Err(_) => notes.push("The installed-module inventory is unavailable; showing loaded modules only.".into()),
    }
    if let Ok(t) = std::fs::read_to_string(format!("/lib/modules/{rel}/modules.builtin")) {
        for l in t.lines() {
            let name = Path::new(l).file_name().map(|s| s.to_string_lossy().split('.').next().unwrap_or("").replace('-', "_")).unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let e = map.entry(name.clone()).or_insert_with(|| Module { name, ..Default::default() });
            e.builtin = true;
            e.state = "Built-in".into();
            e.path = l.to_string();
        }
    }
    for m in map.values_mut() {
        let base = format!("/sys/module/{}", m.name);
        m.version = read(&format!("{base}/version")).unwrap_or_default();
        m.srcversion = read(&format!("{base}/srcversion")).unwrap_or_default();
        m.taint = read(&format!("{base}/taint")).unwrap_or_default();
        m.source = if m.taint.contains('P') {
            "Proprietary".into()
        } else if m.taint.contains('O') {
            "Out-of-tree".into()
        } else if m.taint.contains('E') {
            "Unsigned".into()
        } else if m.path.starts_with("kernel/") || m.path.contains("/kernel/") || m.builtin {
            "In-tree".into()
        } else {
            "Out-of-tree".into()
        };
    }
    let slab = super::system::mem().slab;
    ModuleResult { modules: map.into_values().collect(), kernel_release: rel, notes, slab }
}

pub fn unload_hint(m: &Module) -> String {
    if m.builtin {
        "This module is built into the kernel image, so it cannot be unloaded.".into()
    } else if !m.loaded {
        "This module is not loaded.".into()
    } else if m.refs > 0 || !m.used_by.is_empty() {
        format!("{} other modules or kernel subsystems hold references to this module, so the kernel would refuse to unload it.", m.refs.max(m.used_by.len() as i64))
    } else {
        "Unloading a kernel module requires administrator rights (modprobe -r).".into()
    }
}

// ───────────────────────────── Installed apps ─────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct App {
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub scope: &'static str,
    pub source: &'static str,
    pub command: String,
    pub file: String,
    pub packages_only: bool,
}

fn parse_desktop(path: &Path) -> Option<HashMap<String, String>> {
    let t = std::fs::read_to_string(path).ok()?;
    let mut m = HashMap::new();
    let mut in_entry = false;
    for l in t.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            in_entry = l == "[Desktop Entry]";
            continue;
        }
        if in_entry && !l.starts_with('#') {
            if let Some((k, v)) = l.split_once('=') {
                m.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
            }
        }
    }
    Some(m)
}

pub fn installed_apps() -> Vec<App> {
    let h = home();
    let dirs: Vec<(PathBuf, &'static str, &'static str)> = vec![
        (PathBuf::from("/usr/share/applications"), "System installed", "Desktop entry"),
        (PathBuf::from("/usr/local/share/applications"), "System installed", "Desktop entry"),
        (PathBuf::from("/var/lib/flatpak/exports/share/applications"), "System installed", "flatpak"),
        (PathBuf::from("/var/lib/snapd/desktop/applications"), "System installed", "snap"),
        (h.join(".local/share/applications"), "User installed", "Desktop entry"),
        (h.join(".local/share/flatpak/exports/share/applications"), "User installed", "flatpak"),
    ];
    let mut apps = Vec::new();
    let mut seen = HashSet::new();
    for (d, scope, source) in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().map_or(true, |x| x != "desktop") {
                continue;
            }
            let Some(m) = parse_desktop(&p) else { continue };
            if m.get("Type").map_or(true, |t| t != "Application") || m.get("NoDisplay").map_or(false, |v| v == "true") || m.get("Hidden").map_or(false, |v| v == "true") {
                continue;
            }
            let Some(name) = m.get("Name") else { continue };
            if !seen.insert(e.file_name().to_string_lossy().into_owned()) {
                continue;
            }
            apps.push(App {
                name: name.clone(),
                version: m.get("X-AppVersion").or(m.get("Version")).cloned().unwrap_or_default(),
                publisher: m.get("X-Publisher").cloned().unwrap_or_default(),
                scope,
                source,
                command: m.get("Exec").cloned().unwrap_or_default(),
                file: p.to_string_lossy().into_owned(),
                packages_only: false,
            });
        }
    }
    if which("flatpak") {
        if let Some(t) = run("flatpak", &["list", "--app", "--columns=name,version,origin"]) {
            for l in t.lines() {
                let f: Vec<&str> = l.split('\t').collect();
                if let Some(a) = apps.iter_mut().find(|a| a.source == "flatpak" && Some(&a.name.as_str()) == f.first()) {
                    a.version = f.get(1).unwrap_or(&"").to_string();
                    a.publisher = f.get(2).unwrap_or(&"").to_string();
                }
            }
        }
    }
    // Packages without a desktop entry
    if let Some(t) = read("/var/lib/dpkg/status") {
        for block in t.split("\n\n") {
            let g = |k: &str| block.lines().find_map(|l| l.strip_prefix(k).map(|v| v.trim().to_string())).unwrap_or_default();
            if block.contains("Status: install ok installed") {
                apps.push(App { name: g("Package:"), version: g("Version:"), publisher: g("Maintainer:"), scope: "System installed", source: "deb (dpkg)", packages_only: true, ..Default::default() });
            }
        }
    } else if which("rpm") {
        if let Some(t) = run("rpm", &["-qa", "--qf", "%{NAME}\t%{VERSION}-%{RELEASE}\t%{VENDOR}\n"]) {
            for l in t.lines() {
                let f: Vec<&str> = l.split('\t').collect();
                if f.len() == 3 {
                    apps.push(App { name: f[0].into(), version: f[1].into(), publisher: if f[2] == "(none)" { String::new() } else { f[2].into() }, scope: "System installed", source: "rpm", packages_only: true, ..Default::default() });
                }
            }
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

/// What uninstalling an app would run: a label for the confirmation and the command line.
pub struct UninstallPlan {
    pub package: String,
    pub manager: &'static str,
    pub argv: Vec<String>,
}

fn can_escalate() -> bool {
    which("pkexec") && (std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some())
}

fn as_root(argv: Vec<&str>) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = argv.into_iter().map(String::from).collect();
    if unsafe { libc::geteuid() } == 0 {
        return Ok(v);
    }
    if can_escalate() {
        v.insert(0, "pkexec".into());
        return Ok(v);
    }
    Err(format!("Administrator rights are needed and no graphical prompt is available. Run: sudo {}", v.join(" ")))
}

pub fn uninstall_plan(a: &App) -> Result<UninstallPlan, String> {
    let stem = Path::new(&a.file).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match a.source {
        "flatpak" => {
            if !which("flatpak") || stem.is_empty() {
                return Err("flatpak is not available.".into());
            }
            let scope = if a.scope == "User installed" { "--user" } else { "--system" };
            let argv = ["flatpak", "uninstall", "-y", "--noninteractive", scope, &stem].iter().map(|s| s.to_string()).collect();
            Ok(UninstallPlan { package: stem, manager: "flatpak", argv })
        }
        "snap" => {
            let name = stem.split('_').next().unwrap_or("").to_string();
            if name.is_empty() {
                return Err("Could not tell which snap owns this entry.".into());
            }
            let argv = as_root(vec!["snap", "remove", &name])?;
            Ok(UninstallPlan { package: name, manager: "snap", argv })
        }
        _ => {
            if a.scope == "User installed" && !a.packages_only {
                return Err("This entry lives in your home folder and is not owned by a package manager.".into());
            }
            let owner = |tool: &str, args: &[&str]| run(tool, args).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
            if a.source == "rpm" || (!a.packages_only && which("rpm") && !Path::new("/var/lib/dpkg/status").exists()) {
                let pkg = if a.packages_only { a.name.clone() } else { owner("rpm", &["-qf", "--qf", "%{NAME}", &a.file]).ok_or("No installed rpm package owns this file.")? };
                if Path::new("/run/ostree-booted").exists() && which("rpm-ostree") {
                    let argv = as_root(vec!["rpm-ostree", "uninstall", &pkg])?;
                    return Ok(UninstallPlan { package: pkg, manager: "rpm-ostree (layered packages only; takes effect after reboot)", argv });
                }
                if which("dnf") {
                    let argv = as_root(vec!["dnf", "remove", "-y", &pkg])?;
                    return Ok(UninstallPlan { package: pkg, manager: "dnf", argv });
                }
                let argv = as_root(vec!["rpm", "-e", &pkg])?;
                return Ok(UninstallPlan { package: pkg, manager: "rpm", argv });
            }
            if a.source.starts_with("deb") || which("dpkg") {
                let pkg = if a.packages_only {
                    a.name.clone()
                } else {
                    let o = owner("dpkg", &["-S", &a.file]).ok_or("No installed deb package owns this file.")?;
                    o.lines().next().and_then(|l| l.split(':').next()).unwrap_or("").to_string()
                };
                if pkg.is_empty() {
                    return Err("Could not tell which package owns this file.".into());
                }
                let argv = as_root(vec!["apt-get", "remove", "-y", &pkg])?;
                return Ok(UninstallPlan { package: pkg, manager: "apt", argv });
            }
            Err("No supported package manager found for this entry.".into())
        }
    }
}

pub fn uninstall(argv: &[String]) -> Result<String, String> {
    let (cmd, args) = argv.split_first().ok_or("empty command")?;
    match std::process::Command::new(cmd).args(args).stdin(std::process::Stdio::null()).output() {
        Ok(o) if o.status.success() => Ok("Uninstalled.".into()),
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr);
            let last: Vec<&str> = e.lines().rev().filter(|l| !l.trim().is_empty()).take(2).collect();
            Err(format!("Uninstall failed: {}", if last.is_empty() { "the command reported an error".into() } else { last.into_iter().rev().collect::<Vec<_>>().join(" ") }))
        }
        Err(e) => Err(format!("Could not run {cmd}: {e}")),
    }
}

// ───────────────────────────── Startup apps ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Startup {
    pub name: String,
    pub command: String,
    pub file: String,
    pub enabled: bool,
    pub origin: &'static str,
}

fn autostart_dirs() -> Vec<(PathBuf, &'static str)> {
    let cfg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
    let mut v = vec![(cfg.join("autostart"), "User")];
    let sys = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    for d in sys.split(':').filter(|d| !d.is_empty()) {
        v.push((PathBuf::from(d).join("autostart"), "System"));
    }
    v
}

pub fn startup_apps() -> Vec<Startup> {
    let mut out: Vec<Startup> = Vec::new();
    let mut seen = HashSet::new();
    for (d, origin) in autostart_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut es: Vec<_> = rd.flatten().collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            let p = e.path();
            if p.extension().map_or(true, |x| x != "desktop") {
                continue;
            }
            let fname = e.file_name().to_string_lossy().into_owned();
            if !seen.insert(fname.clone()) {
                continue; // user copy overrides system
            }
            let Some(m) = parse_desktop(&p) else { continue };
            let enabled = m.get("Hidden").map_or(true, |v| v != "true") && m.get("X-GNOME-Autostart-enabled").map_or(true, |v| v != "false");
            out.push(Startup { name: m.get("Name").cloned().unwrap_or(fname.clone()), command: m.get("Exec").cloned().unwrap_or_default(), file: p.to_string_lossy().into_owned(), enabled, origin });
        }
    }
    out
}

/// Enables/disables by writing a user override in ~/.config/autostart.
pub fn set_startup_enabled(s: &Startup, enable: bool) -> Result<(), String> {
    let cfg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
    let dir = cfg.join("autostart");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let fname = Path::new(&s.file).file_name().ok_or("bad path")?;
    let target = dir.join(fname);
    let src = std::fs::read_to_string(&s.file).map_err(|e| e.to_string())?;
    let mut out = String::new();
    let mut inserted = false;
    for l in src.lines() {
        if l.starts_with("Hidden=") || l.starts_with("X-GNOME-Autostart-enabled=") {
            continue;
        }
        out.push_str(l);
        out.push('\n');
        if l.trim() == "[Desktop Entry]" && !inserted {
            out.push_str(&format!("Hidden={}\nX-GNOME-Autostart-enabled={}\n", !enable, enable));
            inserted = true;
        }
    }
    if !inserted {
        return Err("not a desktop entry".into());
    }
    std::fs::write(target, out).map_err(|e| e.to_string())
}

// ───────────────────────────── Users / sessions ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub uid: u32,
    pub seat: String,
    pub tty: String,
}

pub fn sessions() -> Vec<Session> {
    let mut out = Vec::new();
    if let Some(t) = run("loginctl", &["list-sessions", "--no-legend", "--no-pager"]) {
        for l in t.lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() >= 3 {
                out.push(Session { id: f[0].into(), uid: f[1].parse().unwrap_or(0), seat: f.get(3).unwrap_or(&"").to_string(), tty: f.get(4).unwrap_or(&"").to_string() });
            }
        }
    }
    out
}

pub fn lock_session(id: &str) -> Result<(), String> {
    cmd_result("loginctl", &["lock-session", id], "Could not lock the session.")
}

pub fn terminate_session(id: &str) -> Result<(), String> {
    cmd_result("loginctl", &["terminate-session", id], "Could not sign off the session.")
}

pub fn manage_accounts() -> Result<(), String> {
    for (bin, args) in [("gnome-control-center", vec!["user-accounts"]), ("systemsettings", vec!["kcm_users"]), ("systemsettings5", vec!["kcm_users"]), ("cinnamon-settings", vec!["user"]), ("mate-users-admin", vec![]), ("lxqt-admin-user", vec![])] {
        if which(bin) {
            return std::process::Command::new(bin).args(args).spawn().map(|_| ()).map_err(|_| "Could not open the user accounts settings.".into());
        }
    }
    Err("No user accounts settings panel is installed.".into())
}

fn cmd_result(cmd: &str, args: &[&str], fail: &str) -> Result<(), String> {
    match std::process::Command::new(cmd).args(args).output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            Err(if e.is_empty() { fail.into() } else { format!("{fail} {e}") })
        }
        Err(_) => Err(fail.into()),
    }
}

// ───────────────────────────── Services ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Service {
    pub unit: String,
    pub active: String,
    pub sub: String,
    pub description: String,
    pub enabled: String,
    pub pid: Option<i32>,
}

pub fn services() -> Vec<Service> {
    let mut out = Vec::new();
    let enabled: HashMap<String, String> = run("systemctl", &["list-unit-files", "--type=service", "--no-legend", "--no-pager", "--plain"])
        .map(|t| t.lines().filter_map(|l| { let mut s = l.split_whitespace(); Some((s.next()?.to_string(), s.next()?.to_string())) }).collect())
        .unwrap_or_default();
    if let Some(t) = run("systemctl", &["list-units", "--type=service", "--all", "--no-legend", "--no-pager", "--plain"]) {
        for l in t.lines() {
            let mut it = l.split_whitespace();
            let (Some(unit), Some(_load), Some(active), Some(sub)) = (it.next(), it.next(), it.next(), it.next()) else { continue };
            let desc: Vec<&str> = it.collect();
            out.push(Service { unit: unit.into(), active: active.into(), sub: sub.into(), description: desc.join(" "), enabled: enabled.get(unit).cloned().unwrap_or_else(|| "-".into()), pid: None });
        }
    }
    // MainPID for active units, one systemctl call
    let active: Vec<&str> = out.iter().filter(|s| s.active == "active").map(|s| s.unit.as_str()).collect();
    if !active.is_empty() {
        let mut args = vec!["show", "-p", "Id,MainPID", "--no-pager"];
        args.extend(active.iter().copied());
        if let Some(t) = run("systemctl", &args) {
            let (mut id, mut pids) = (String::new(), HashMap::new());
            for l in t.lines() {
                if let Some(v) = l.strip_prefix("Id=") {
                    id = v.into();
                } else if let Some(v) = l.strip_prefix("MainPID=") {
                    if let Ok(p) = v.parse::<i32>() {
                        if p > 0 {
                            pids.insert(id.clone(), p);
                        }
                    }
                }
            }
            for s in out.iter_mut() {
                s.pid = pids.get(&s.unit).copied();
            }
        }
    }
    out
}

pub fn service_action(unit: &str, action: &str) -> Result<(), String> {
    let mut args = vec![action, "--no-ask-password", unit];
    let mut r = cmd_result("systemctl", &args, &format!("Could not {action} \"{unit}\"."));
    if r.is_err() && which("pkexec") && std::env::var_os("DISPLAY").is_some() | std::env::var_os("WAYLAND_DISPLAY").is_some() {
        args.remove(1);
        let mut a = vec!["systemctl"];
        a.extend(args);
        r = cmd_result("pkexec", &a, &format!("Could not {action} \"{unit}\"."));
    }
    r
}
