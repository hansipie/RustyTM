use crate::util::*;
use std::process::Command;

#[derive(Clone, Debug, Default)]
pub struct DiskHealth {
    pub dev: String,
    pub model: String,
    pub serial: String,
    pub firmware: String,
    pub size: u64,
    pub kind: String,
    /// None when SMART could not be read.
    pub passed: Option<bool>,
    pub temp: Option<i64>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    /// Wear used (NVMe percentage_used) or remaining life derived from SSD attributes.
    pub wear_used: Option<u64>,
    pub details: Vec<(String, String)>,
    pub note: String,
}

#[derive(Clone, Debug, Default)]
pub struct HealthResult {
    pub disks: Vec<DiskHealth>,
    pub notes: Vec<String>,
}

fn block_devices() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir("/sys/block")
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.retain(|n| {
        let rest = n.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        (n.starts_with("sd") && rest.is_empty()) || (n.starts_with("hd") && rest.is_empty()) || (n.starts_with("nvme") && n.contains('n')) || n.starts_with("mmcblk") && !n.contains("boot")
    });
    v.sort();
    v
}

fn smartctl(dev: &str) -> Option<serde_json::Value> {
    let path = format!("/dev/{dev}");
    let attempts: [Vec<&str>; 2] = [vec!["smartctl", "-j", "-a", &path], vec!["sudo", "-n", "smartctl", "-j", "-a", &path]];
    for (i, a) in attempts.iter().enumerate() {
        if i == 1 && unsafe { libc::geteuid() } == 0 {
            break;
        }
        // smartctl's exit status is a bit mask that is non-zero for many healthy-but-noteworthy disks, so only the JSON matters.
        let Ok(o) = Command::new(a[0]).args(&a[1..]).stdin(std::process::Stdio::null()).output() else { continue };
        if let Ok(j) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
            if j.get("smart_status").is_some() || j.get("model_name").is_some() {
                return Some(j);
            }
        }
    }
    None
}

fn attr(j: &serde_json::Value, id: u64) -> Option<u64> {
    j["ata_smart_attributes"]["table"].as_array()?.iter().find(|a| a["id"].as_u64() == Some(id))?["raw"]["value"].as_u64()
}

pub fn disks() -> HealthResult {
    let mut res = HealthResult::default();
    if !which("smartctl") {
        res.notes.push("smartctl is not installed (package smartmontools); showing basic disk information only.".into());
    }
    for dev in block_devices() {
        let sys = format!("/sys/block/{dev}");
        let rotational = read(&format!("{sys}/queue/rotational")).map_or(false, |v| v == "1");
        let mut d = DiskHealth {
            dev: dev.clone(),
            model: read(&format!("{sys}/device/model")).unwrap_or_default(),
            size: read(&format!("{sys}/size")).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0) * 512,
            kind: if dev.starts_with("nvme") { "NVMe".into() } else if dev.starts_with("mmcblk") { "MMC".into() } else if rotational { "HDD".into() } else { "SSD".into() },
            ..Default::default()
        };
        if which("smartctl") {
            match smartctl(&dev) {
                Some(j) => fill(&mut d, &j),
                None => d.note = "SMART data could not be read. It usually needs administrator rights: run rustytm with sudo, or allow passwordless `sudo smartctl`.".into(),
            }
        }
        res.disks.push(d);
    }
    if res.disks.is_empty() {
        res.notes.push("No physical disks were found.".into());
    }
    res
}

fn fill(d: &mut DiskHealth, j: &serde_json::Value) {
    let s = |k: &str| j[k].as_str().unwrap_or("").to_string();
    if !s("model_name").is_empty() {
        d.model = s("model_name");
    }
    d.serial = s("serial_number");
    d.firmware = s("firmware_version");
    if let Some(b) = j["user_capacity"]["bytes"].as_u64() {
        d.size = b;
    }
    d.passed = j["smart_status"]["passed"].as_bool();
    d.temp = j["temperature"]["current"].as_i64();
    d.power_on_hours = j["power_on_time"]["hours"].as_u64();
    d.power_cycles = j["power_cycle_count"].as_u64();
    let mut det = Vec::new();
    let mut add = |k: &str, v: String| det.push((k.to_string(), v));
    let nvme = &j["nvme_smart_health_information_log"];
    if nvme.is_object() {
        d.wear_used = nvme["percentage_used"].as_u64();
        let n = |k: &str| nvme[k].as_u64();
        if let Some(v) = n("available_spare") {
            add("Available spare", format!("{v}%{}", n("available_spare_threshold").map(|t| format!(" (threshold {t}%)")).unwrap_or_default()));
        }
        if let Some(v) = n("data_units_written") {
            add("Data written", bytes(v as f64 * 512_000.0));
        }
        if let Some(v) = n("data_units_read") {
            add("Data read", bytes(v as f64 * 512_000.0));
        }
        if let Some(v) = n("media_errors") {
            add("Media errors", v.to_string());
        }
        if let Some(v) = n("num_err_log_entries") {
            add("Error log entries", v.to_string());
        }
        if let Some(v) = n("unsafe_shutdowns") {
            add("Unsafe shutdowns", v.to_string());
        }
        if let Some(v) = n("critical_warning") {
            add("Critical warning flags", format!("{v:#x}"));
        }
    } else {
        for (id, name) in [(5, "Reallocated sectors"), (197, "Pending sectors"), (198, "Offline uncorrectable"), (199, "UDMA CRC errors")] {
            if let Some(v) = attr(j, id) {
                add(name, v.to_string());
            }
        }
        // Wear indicators: normalized value counts down from 100.
        if let Some(a) = j["ata_smart_attributes"]["table"].as_array() {
            if let Some(w) = a.iter().find(|a| matches!(a["id"].as_u64(), Some(177 | 231 | 233))) {
                if let Some(v) = w["value"].as_u64() {
                    d.wear_used = Some(100u64.saturating_sub(v.min(100)));
                    add("Wear indicator (remaining)", format!("{v}%"));
                }
            }
        }
    }
    let log_errs = j["ata_smart_error_log"]["summary"]["count"].as_u64();
    if let Some(e) = log_errs {
        add("ATA error log count", e.to_string());
    }
    if let Some(rr) = j["rotation_rate"].as_u64().filter(|r| *r > 0) {
        add("Rotation rate", format!("{rr} rpm"));
    }
    if let Some(tr) = j["interface_speed"]["current"]["string"].as_str() {
        add("Link speed", tr.to_string());
    }
    d.details = det;
}

pub fn verdict(d: &DiskHealth) -> &'static str {
    match d.passed {
        Some(true) => {
            let bad = d.details.iter().any(|(k, v)| matches!(k.as_str(), "Reallocated sectors" | "Pending sectors" | "Offline uncorrectable" | "Media errors") && v != "0");
            if bad || d.wear_used.map_or(false, |w| w >= 90) {
                "Warning"
            } else {
                "Healthy"
            }
        }
        Some(false) => "Failing",
        None => "Unknown",
    }
}
