//! `rustytm update`: compare the running version with crates.io and reinstall through cargo.

use std::process::Command;
use std::time::Duration;

const CRATE: &str = "rustytm";

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>());
    Some((it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?))
}

fn latest_version() -> Result<String, String> {
    let url = format!("https://crates.io/api/v1/crates/{CRATE}");
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
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

/// Returns the process exit code.
pub fn run() -> i32 {
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: {current}");
    let latest = match latest_version() {
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
