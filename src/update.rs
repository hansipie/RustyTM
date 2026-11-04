//! `rustytm update`: compare the running version with crates.io and reinstall through cargo.

use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const CRATE: &str = "rustytm";
/// Longest the quit-time version check may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>());
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?))
}

fn latest_version(timeout: Duration) -> Result<String, String> {
    let url = format!("https://crates.io/api/v1/crates/{CRATE}");
    let agent = ureq::AgentBuilder::new()
        .timeout(timeout)
        .user_agent(&format!("rustytm/{} (update check)", env!("CARGO_PKG_VERSION")))
        .build();
    let text = agent
        .get(&url)
        .call()
        .map_err(|e| format!("cannot reach crates.io: {e}"))?
        .into_string()
        .map_err(|e| format!("cannot read crates.io response: {e}"))?;
    let body: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("unexpected crates.io response: {e}"))?;
    let c = &body["crate"];
    c["max_stable_version"]
        .as_str()
        .or_else(|| c["max_version"].as_str())
        .map(str::to_string)
        .ok_or_else(|| "no version found on crates.io".to_string())
}

/// True when `latest` is a higher version than `current`.
fn is_newer(current: &str, latest: &str) -> bool {
    matches!((parse_version(current), parse_version(latest)), (Some(c), Some(l)) if l > c)
}

/// Starts the version check in the background so that neither start-up nor quitting waits on the network.
/// The channel receives the latest version only when it is newer than the running one.
/// Set `RUSTYTM_NO_UPDATE_CHECK` to skip the check (offline machines, packaged installs).
pub fn start_check() -> Option<Receiver<String>> {
    if std::env::var_os("RUSTYTM_NO_UPDATE_CHECK").is_some() {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // errors (offline, slow network, crates.io down) are ignored on purpose: the notice is best effort
        if let Ok(latest) = latest_version(CHECK_TIMEOUT) {
            if is_newer(env!("CARGO_PKG_VERSION"), &latest) {
                let _ = tx.send(latest);
            }
        }
    });
    Some(rx)
}

/// Called after the terminal is restored: waits 2 seconds at most for the check, says nothing if it fails or times out, then tells the user on stdout.
pub fn announce(rx: Option<Receiver<String>>) {
    let Some(rx) = rx else { return };
    if let Ok(latest) = rx.recv_timeout(CHECK_TIMEOUT) {
        println!("A new version of RustyTM is available: {} -> {latest}", env!("CARGO_PKG_VERSION"));
        println!("Update with: rustytm update");
    }
}

/// Returns the process exit code.
pub fn run() -> i32 {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: {current}");
    let latest = match latest_version(Duration::from_secs(15)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Update check failed: {e}");
            return 1;
        }
    };
    println!("Latest version:  {latest}");
    match (parse_version(current), parse_version(&latest)) {
        (Some(c), Some(l)) if l <= c => {
            println!("RustyTM is up to date.");
            return 0;
        }
        (Some(_), Some(_)) => {}
        _ => {
            eprintln!("Cannot compare versions '{current}' and '{latest}'.");
            return 1;
        }
    }
    println!("Updating: cargo install {CRATE} --locked --force");
    match Command::new("cargo").args(["install", CRATE, "--locked", "--force"]).status() {
        Ok(s) if s.success() => {
            println!("Updated to {latest}.");
            0
        }
        Ok(s) => {
            eprintln!("cargo install failed ({s}).");
            1
        }
        Err(e) => {
            eprintln!("Cannot run cargo ({e}). Install Rust from https://rustup.rs, then run: cargo install {CRATE} --locked --force");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions() {
        assert!(is_newer("1.5.0", "1.6.0"));
        assert!(is_newer("1.5.0", "1.5.1"));
        assert!(is_newer("1.9.0", "2.0.0"));
        assert!(!is_newer("1.5.0", "1.5.0"));
        assert!(!is_newer("1.5.0", "1.4.9"));
        assert!(!is_newer("1.5.0", "garbage"));
        assert!(is_newer("1.5.0", "1.10.0")); // numeric, not textual, comparison
    }
}
