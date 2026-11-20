use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

pub fn read(p: &str) -> Option<String> {
    let mut s = std::fs::read_to_string(p).ok()?;
    // trim in place: no second allocation
    s.truncate(s.trim_end().len());
    let lead = s.len() - s.trim_start().len();
    if lead > 0 {
        s.drain(..lead);
    }
    Some(s)
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

/// Whether `cmd` is in PATH. The PATH walk is memoised: installed tools do not change while the app runs.
pub fn which(cmd: &str) -> bool {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(&v) = cache.lock().ok().and_then(|c| c.get(cmd).copied()).as_ref() {
        return v;
    }
    let found = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false);
    if let Ok(mut c) = cache.lock() {
        c.insert(cmd.to_string(), found);
    }
    found
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

/// Runs a command line with `sh -c`, output discarded; a thread waits for it so it does not stay a zombie.
pub fn run_detached(cmd: &str) -> std::io::Result<()> {
    let mut child = Command::new("sh").arg("-c").arg(cmd).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Process groups of the commands `output_with_timeout` is running (0 = free slot). They are not
/// in RustyTM's group, so the terminal's Ctrl-C (or a SIGTERM) does not reach them by itself:
/// `forward_signal` passes it on. Atomics and kill(2) only, so it is safe in a signal handler.
static RUNNING_GROUPS: [std::sync::atomic::AtomicI32; 32] = [const { std::sync::atomic::AtomicI32::new(0) }; 32];

/// Commands being started whose group is not registered yet. A signal arriving meanwhile waits in
/// `PENDING_SIGNAL` and is passed on once they are registered (see `Spawning`), instead of being lost.
static SPAWNING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PENDING_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn forward_signal(sig: libc::c_int) {
    use std::sync::atomic::Ordering::SeqCst;
    if SPAWNING.load(SeqCst) > 0 {
        PENDING_SIGNAL.store(sig, SeqCst);
        // the last start may have ended between the check and the store, and so missed the
        // signal: check again, and handle it here if nobody is left to do it
        if SPAWNING.load(SeqCst) > 0 {
            return;
        }
        let sig = PENDING_SIGNAL.swap(0, SeqCst);
        if sig == 0 {
            // `Spawning::drop` took it: it is being handled there
            return;
        }
    }
    pass_on_and_die(sig);
}

/// Sends `sig` to every running command's group, then ends RustyTM as `sig` would have.
/// Atomics, kill(2), signal(2) and raise(3) only: it also runs inside the signal handler.
fn pass_on_and_die(sig: libc::c_int) {
    for g in &RUNNING_GROUPS {
        let pgid = g.load(std::sync::atomic::Ordering::SeqCst);
        if pgid > 0 {
            // SAFETY: kill(2) is async-signal-safe
            unsafe {
                libc::kill(-pgid, sig);
            }
        }
    }
    // then end as the signal would have: default action, raised again
    // SAFETY: signal(2) and raise(3) are async-signal-safe
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// Installs `forward_signal` for SIGINT and SIGTERM once, unless the signal was ignored on purpose.
fn forward_signals() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for sig in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: installs a handler that only uses async-signal-safe calls
            unsafe {
                if libc::signal(sig, forward_signal as extern "C" fn(libc::c_int) as libc::sighandler_t) == libc::SIG_IGN {
                    libc::signal(sig, libc::SIG_IGN);
                }
            }
        }
    });
}

/// Held while a command is started and its group registered: a signal arriving meanwhile is held
/// back, then handled when the last command being started is registered.
struct Spawning;

impl Spawning {
    fn new() -> Self {
        forward_signals();
        SPAWNING.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Spawning
    }
}

impl Drop for Spawning {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::SeqCst;
        // the last one out handles a signal held back; one arriving after this sees SPAWNING at 0
        // and is handled straight away by the handler
        if SPAWNING.fetch_sub(1, SeqCst) == 1 {
            let sig = PENDING_SIGNAL.swap(0, SeqCst);
            if sig != 0 {
                pass_on_and_die(sig);
            }
        }
    }
}

/// A process group listed in `RUNNING_GROUPS` while it lives.
struct GroupSlot(Option<usize>);

impl GroupSlot {
    fn new(pgid: i32) -> Self {
        use std::sync::atomic::Ordering::SeqCst;
        forward_signals();
        // when all slots are taken the group is simply not tracked
        GroupSlot(RUNNING_GROUPS.iter().position(|g| g.compare_exchange(0, pgid, SeqCst, SeqCst).is_ok()))
    }
}

impl Drop for GroupSlot {
    fn drop(&mut self) {
        if let Some(i) = self.0 {
            RUNNING_GROUPS[i].store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Runs a command and collects its output, killing it after `limit` (an unanswered `pkexec`
/// password dialog would otherwise wait forever). `Err` holds a message for the user.
pub fn output_with_timeout(cmd: &str, args: &[&str], limit: std::time::Duration) -> Result<std::process::Output, String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    // Ctrl-C / SIGTERM sent to RustyTM still reach the command (see `forward_signal`); one that
    // arrives before its group is registered is held back until then (see `Spawning`)
    let spawning = Spawning::new();
    // its own process group, so a timeout can stop whatever it started as well
    let mut child = Command::new(cmd).args(args).process_group(0).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("{cmd}: {e}"))?;
    let slot = GroupSlot::new(child.id() as i32);
    drop(spawning);
    // drain the pipes while waiting, so a chatty command cannot block on a full pipe. The readers
    // report over a channel instead of being joined: a grandchild that inherited the pipes could
    // keep them open long after the command itself has exited.
    // What was read so far is shared, so output written before the command exited is kept.
    let pipe = |p: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = channel::<()>();
        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let b = buf.clone();
        std::thread::spawn(move || {
            if let Some(mut p) = p {
                let mut chunk = [0u8; 8192];
                loop {
                    match p.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => b.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            }
            let _ = tx.send(());
        });
        (rx, buf)
    };
    let out = pipe(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = pipe(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() >= limit => {
                // Stop the command and everything it started (their pipes then close and the reader
                // threads end). Once pkexec has accepted the password, the command runs as root and
                // kill fails with EPERM: never wait for it here, a thread reaps it whenever it ends.
                let killed = child.kill().is_ok();
                // SAFETY: plain kill(2) on the process group created above (pgid = child's pid)
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                std::thread::spawn(move || {
                    let _ = child.wait();
                    drop(slot);
                });
                return Err(if killed {
                    format!("no answer within {} s (an authentication dialog may be waiting); the command was stopped.", limit.as_secs())
                } else {
                    format!("no answer within {} s; the command runs with administrator rights and could not be stopped, it goes on in the background.", limit.as_secs())
                });
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    drop(slot);
    // the command has exited: give the readers a moment to finish, then keep what they have
    let grace = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let collect = |(rx, buf): (Receiver<()>, std::sync::Arc<std::sync::Mutex<Vec<u8>>>)| {
        let _ = rx.recv_timeout(grace.saturating_duration_since(std::time::Instant::now()));
        std::mem::take(&mut *buf.lock().unwrap_or_else(|e| e.into_inner()))
    };
    Ok(std::process::Output { status, stdout: collect(out), stderr: collect(err) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked_alive(mark: &str) -> bool {
        std::fs::read_dir("/proc").unwrap().flatten().any(|e| std::fs::read(e.path().join("cmdline")).map_or(false, |c| String::from_utf8_lossy(&c).contains(mark)))
    }

    /// Run by `ctrl_c_reaches_running_commands` in a separate process: blocks in a command.
    #[test]
    #[ignore]
    fn ctrl_c_helper() {
        if let Ok(mark) = std::env::var("RUSTYTM_CTRL_C_MARK") {
            let _ = output_with_timeout("sh", &["-c", &format!("sleep 30; : {mark}")], std::time::Duration::from_secs(60));
        }
    }

    /// Run by `ctrl_c_while_starting_waits_its_turn` in a separate process.
    #[test]
    #[ignore]
    fn ctrl_c_while_starting_helper() {
        if std::env::var("RUSTYTM_CTRL_C_STARTING").is_ok() {
            let starting = Spawning::new();
            // SAFETY: raise(3) on ourselves; the handler holds it back while a command is starting
            unsafe {
                libc::raise(libc::SIGINT);
            }
            println!("still alive while starting");
            drop(starting);
            println!("survived the end of the start");
        }
    }

    /// A Ctrl-C that lands while a command is being started is held back, then ends RustyTM.
    #[test]
    fn ctrl_c_while_starting_waits_its_turn() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "util::tests::ctrl_c_while_starting_helper", "--nocapture", "--test-threads=1"])
            .env("RUSTYTM_CTRL_C_STARTING", "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        use std::os::unix::process::ExitStatusExt;
        assert!(text.contains("still alive while starting"), "{text}");
        assert!(!text.contains("survived the end of the start"), "{text}");
        assert_eq!(out.status.signal(), Some(libc::SIGINT));
    }

    /// Ctrl-C (SIGINT) sent to RustyTM still stops a command running in its own process group.
    #[test]
    fn ctrl_c_reaches_running_commands() {
        let mark = format!("rustytm-ctrlc-{}", std::process::id());
        let mut helper = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "util::tests::ctrl_c_helper"])
            .env("RUSTYTM_CTRL_C_MARK", &mark)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let t = std::time::Instant::now();
        while !marked_alive(&format!(": {mark}")) {
            assert!(t.elapsed() < std::time::Duration::from_secs(10), "the command never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // no wait here on purpose: the helper may not have registered the command's group yet, and
        // the signal must reach the command all the same
        // SAFETY: plain kill(2) on our own child process
        unsafe {
            libc::kill(helper.id() as i32, libc::SIGINT);
        }
        let status = helper.wait().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGINT), "RustyTM must still end on Ctrl-C");
        assert!(!marked_alive(&format!(": {mark}")), "the running command survived Ctrl-C");
    }

    #[test]
    fn slow_commands_are_stopped() {
        let t = std::time::Instant::now();
        let r = output_with_timeout("sleep", &["5"], std::time::Duration::from_millis(300));
        assert!(r.unwrap_err().contains("the command was stopped"));
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        // on a timeout, what the command started is stopped too
        let mark = format!("rustytm-test-{}", std::process::id());
        let r = output_with_timeout("sh", &["-c", &format!("sh -c 'sleep 30; : {mark}' & wait")], std::time::Duration::from_millis(300));
        assert!(r.is_err());
        std::thread::sleep(std::time::Duration::from_millis(200));
        let left = std::fs::read_dir("/proc").unwrap().flatten().any(|e| std::fs::read(e.path().join("cmdline")).map_or(false, |c| String::from_utf8_lossy(&c).contains(&mark)));
        assert!(!left, "the grandchild survived the timeout");
        let o = output_with_timeout("sh", &["-c", "echo hi; echo oops >&2; exit 3"], std::time::Duration::from_secs(5)).unwrap();
        assert_eq!((o.status.code(), &o.stdout[..], &o.stderr[..]), (Some(3), &b"hi\n"[..], &b"oops\n"[..]));
        // a grandchild holding the pipes open does not keep us waiting once the command has exited
        let t = std::time::Instant::now();
        let o = output_with_timeout("sh", &["-c", "sleep 10 & echo done"], std::time::Duration::from_secs(20)).unwrap();
        assert!(o.status.success());
        assert_eq!(&o.stdout[..], &b"done\n"[..], "output written before the exit is kept");
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "waited for the grandchild");
    }
}
