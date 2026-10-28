use crate::util::config_dir;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub fahrenheit: bool,
    pub show_exited: bool,
    pub smooth_cpu: bool,
    pub process_tree: bool,
    pub per_core_cpu: bool,
    pub combined_gpus: bool,
    pub combined_disks: bool,
    pub combined_network: bool,
    pub net_bits: bool,
    pub status_bar: bool,
    pub sidebar: bool,
    pub refresh_ms: u64,
    pub sort_col: usize,
    pub sort_desc: bool,
    pub light_theme: bool,
    pub confirm_stop_scan: bool,
    pub last_page: usize,
    pub bench_dir: String,
    pub collapsed: Vec<i32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            fahrenheit: false,
            show_exited: false,
            smooth_cpu: true,
            process_tree: false,
            per_core_cpu: false,
            combined_gpus: false,
            combined_disks: false,
            combined_network: false,
            net_bits: false,
            status_bar: true,
            sidebar: true,
            refresh_ms: 1000,
            sort_col: 2,
            sort_desc: true,
            light_theme: false,
            confirm_stop_scan: true,
            last_page: 0,
            bench_dir: String::new(),
            collapsed: vec![],
        }
    }
}

pub fn path() -> std::path::PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Result<Settings, String> {
        match std::fs::read_to_string(path()) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let p = path();
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(tmp, p).map_err(|e| e.to_string())
    }

    pub fn temp(&self, c: f64) -> String {
        if self.fahrenheit {
            format!("{:.0}°F", c * 9.0 / 5.0 + 32.0)
        } else {
            format!("{c:.0}°C")
        }
    }

    /// A Celsius reading in the chosen unit, for graph scales.
    pub fn temp_value(&self, c: f64) -> f64 {
        if self.fahrenheit { c * 9.0 / 5.0 + 32.0 } else { c }
    }

    pub fn temp_unit(&self) -> &'static str {
        if self.fahrenheit { "°F" } else { "°C" }
    }
}

pub fn reset() -> String {
    let p = path();
    match std::fs::remove_file(&p) {
        Ok(_) => format!("Cleared {}.", p.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => format!("No settings file at {}.", p.display()),
        Err(e) => format!("error: could not clear {}: {e}", p.display()),
    }
}
