use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

pub fn read(p: &str) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

pub fn read_u64(p: &str) -> Option<u64> {
    read(p)?.parse().ok()
}

pub fn read_f64(p: &str) -> Option<f64> {
    read(p)?.parse().ok()
}

pub fn bytes(n: f64) -> String {
    const U: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = n;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{:.0} {}", v, U[i])
    } else if v >= 100.0 {
        format!("{:.0} {}", v, U[i])
    } else {
        format!("{:.1} {}", v, U[i])
    }
}

pub fn rate(n: f64) -> String {
    format!("{}/s", bytes(n))
}

static NET_BITS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_net_bits(on: bool) {
    NET_BITS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Network throughput, in bits per second (decimal units) or bytes per second depending on the setting.
pub fn net_rate(n: f64) -> String {
    if !NET_BITS.load(std::sync::atomic::Ordering::Relaxed) {
        return rate(n);
    }
    const U: [&str; 5] = ["b", "Kb", "Mb", "Gb", "Tb"];
    let mut v = n * 8.0;
    let mut i = 0;
    while v >= 1000.0 && i < U.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if i == 0 || v >= 100.0 { format!("{:.0} {}/s", v, U[i]) } else { format!("{:.1} {}/s", v, U[i]) }
}

pub fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("rustytm")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("rustytm")
}

pub fn uptime_str(secs: u64) -> String {
    format!("{}:{:02}:{:02}:{:02}", secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

/// A one-shot background job whose result is polled from the UI thread.
pub struct Bg<T> {
    rx: Option<Receiver<T>>,
}

impl<T: Send + 'static> Bg<T> {
    pub fn new() -> Self {
        Bg { rx: None }
    }
    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }
    pub fn spawn(&mut self, f: impl FnOnce() -> T + Send + 'static) {
        if self.rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
    }
    pub fn poll(&mut self) -> Option<T> {
        let rx = self.rx.as_ref()?;
        match rx.try_recv() {
            Ok(v) => {
                self.rx = None;
                Some(v)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                None
            }
        }
    }
}
