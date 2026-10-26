use crate::util::data_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_SAMPLES: usize = 36_000;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Sample {
    pub t_ms: u64,
    pub cpu: Vec<f32>,
    pub cpu_overall: f32,
    pub mem_used: u64,
    pub temps: Vec<f32>,
    pub gpu: Vec<f32>,
    pub power_w: Option<f32>,
    pub net_rx: f64,
    pub net_tx: f64,
    pub disk_read: f64,
    pub disk_write: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Trace {
    pub version: u32,
    pub recorded_at: u64,
    pub host: String,
    pub logical: usize,
    pub mem_total: u64,
    pub temp_names: Vec<String>,
    pub gpu_names: Vec<String>,
    pub samples: Vec<Sample>,
}

pub struct Recorder {
    pub trace: Trace,
    start: Instant,
    pub active: bool,
    pub limit_reached: bool,
    pub saved: Option<PathBuf>,
}

impl Recorder {
    pub fn start(host: String, logical: usize, mem_total: u64, temp_names: Vec<String>, gpu_names: Vec<String>) -> Self {
        let recorded_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Recorder { trace: Trace { version: FORMAT_VERSION, recorded_at, host, logical, mem_total, temp_names, gpu_names, samples: vec![] }, start: Instant::now(), active: true, limit_reached: false, saved: None }
    }

    pub fn push(&mut self, mut s: Sample) {
        if !self.active {
            return;
        }
        if self.trace.samples.len() >= MAX_SAMPLES {
            self.limit_reached = true;
            self.active = false;
            return;
        }
        s.t_ms = if self.trace.samples.is_empty() { 0 } else { self.start.elapsed().as_millis() as u64 };
        self.trace.samples.push(s);
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    pub fn save(&mut self) -> Result<PathBuf, String> {
        let dir = data_dir().join("traces");
        std::fs::create_dir_all(&dir).map_err(|_| "The recording folder could not be created.".to_string())?;
        let p = dir.join(format!("trace-{}.rustytmtrace", self.trace.recorded_at));
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&self.trace).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())?;
        self.saved = Some(p.clone());
        Ok(p)
    }
}

pub fn list_traces() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(data_dir().join("traces"))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "rustytmtrace")).collect())
        .unwrap_or_default();
    v.sort();
    v
}

pub fn load(path: &Path) -> Result<Trace, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let t: Trace = serde_json::from_slice(&bytes).map_err(|e| format!("The trace is not valid JSON: {e}"))?;
    validate(&t)?;
    Ok(t)
}

pub fn validate(t: &Trace) -> Result<(), String> {
    if t.version == 0 {
        return Err("This trace has an invalid format version.".into());
    }
    if t.version > FORMAT_VERSION {
        return Err("This trace was written by a newer version and cannot be read.".into());
    }
    if t.logical == 0 || t.logical > 4096 {
        return Err("The trace header contains an invalid hardware count.".into());
    }
    if t.samples.is_empty() {
        return Err("That trace contains no samples.".into());
    }
    if t.samples.len() > MAX_SAMPLES {
        return Err("That trace contains too many samples.".into());
    }
    if t.samples[0].t_ms != 0 {
        return Err("The trace must begin at timestamp zero.".into());
    }
    let mut last = 0;
    for s in &t.samples {
        if s.cpu.len() != t.logical || s.temps.len() != t.temp_names.len() || s.gpu.len() != t.gpu_names.len() {
            return Err("A trace sample does not match the header dimensions.".into());
        }
        if s.t_ms < last {
            return Err("The trace timestamps are invalid or out of order.".into());
        }
        last = s.t_ms;
        if s.cpu.iter().chain(&s.gpu).any(|v| !v.is_finite() || *v < 0.0 || *v > 100.5) {
            return Err("The trace contains an invalid utilization value.".into());
        }
        if s.temps.iter().any(|v| !v.is_finite() || *v < -100.0 || *v > 1000.0) {
            return Err("The trace contains an invalid temperature value.".into());
        }
    }
    Ok(())
}
