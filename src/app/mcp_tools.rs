//! The MCP tools, run on the UI thread against the app's latest data (see `crate::mcp`).

use super::*;
use crate::mcp::{self, Outcome};
use serde_json::{json, Value};

/// `Ok(None)`: the data is still being collected (the transport asks again).
type R = Result<Option<Value>, String>;

fn arg_str<'a>(a: &'a Value, k: &str) -> Option<&'a str> {
    a.get(k).and_then(Value::as_str)
}

fn arg_bool(a: &Value, k: &str) -> Option<bool> {
    a.get(k).and_then(Value::as_bool)
}

fn arg_i64(a: &Value, k: &str) -> Option<i64> {
    a.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
}

fn limit(a: &Value, default: usize) -> usize {
    arg_i64(a, "limit").map(|n| n.clamp(1, 5000) as usize).unwrap_or(default)
}

fn pid_arg(a: &Value) -> Result<i32, String> {
    match arg_i64(a, "pid") {
        // 0 and negative values address process groups or every process: never
        Some(p) if p >= 1 && p <= i32::MAX as i64 => Ok(p as i32),
        _ => Err("pid must be a positive process ID".into()),
    }
}

fn required<'a>(a: &'a Value, k: &str) -> Result<&'a str, String> {
    arg_str(a, k).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| format!("missing argument: {k}"))
}

/// Case-insensitive substring match of any cell (an empty filter matches everything).
fn hit(q: &str, cells: &[&str]) -> bool {
    q.is_empty() || cells.iter().any(|c| c.to_lowercase().contains(q))
}

fn filter_arg(a: &Value) -> String {
    arg_str(a, "filter").unwrap_or("").trim().to_lowercase()
}

fn parse_signal(s: &str) -> Option<i32> {
    match s.trim().to_uppercase().trim_start_matches("SIG") {
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
    }
}

/// A process listed by `list_processes`.
fn proc_json(p: &Proc, gpu: &HashMap<u32, (Option<f32>, Option<u64>)>) -> Value {
    let g = gpu.get(&(p.pid as u32));
    let mut cmd = p.cmdline.clone();
    if cmd.len() > 400 {
        let mut cut = 400;
        while !cmd.is_char_boundary(cut) {
            cut -= 1;
        }
        cmd.truncate(cut);
        cmd.push('…');
    }
    json!({
        "pid": p.pid, "ppid": p.ppid, "name": p.name, "user": p.user, "uid": p.uid, "status": p.state_label(),
        "cpu_pct": (p.cpu * 10.0).round() / 10.0, "memory": p.rss, "threads": p.threads, "nice": p.nice,
        "disk_read_rate": p.read_rate.round(), "disk_write_rate": p.write_rate.round(), "disk_read_total": p.read_bytes, "disk_write_total": p.write_bytes,
        "gpu_pct": g.and_then(|x| x.0), "vram": g.and_then(|x| x.1), "cmdline": cmd,
    })
}

/// A trace in the Flight Recorder folder, by file name or full path. Anything else is refused with
/// the same message, so the tool cannot be used to probe other files.
fn mcp_trace_path(given: &str) -> Result<PathBuf, String> {
    let refused = || "no saved trace with that name in the Flight Recorder folder (see get_recorder)".to_string();
    let dir = data_dir().join("traces").canonicalize().map_err(|_| refused())?;
    let p = if given.contains('/') { PathBuf::from(given) } else { dir.join(given) };
    // canonical: `..` and symlinks resolved before the folder check
    let p = p.canonicalize().map_err(|_| refused())?;
    if !p.starts_with(&dir) || p.extension().and_then(|e| e.to_str()) != Some("rustytmtrace") {
        return Err(refused());
    }
    // 64 MiB: about twice a full recording (36 000 samples)
    let md = std::fs::metadata(&p).map_err(|_| refused())?;
    if !md.is_file() || md.len() > 64 << 20 {
        return Err(refused());
    }
    Ok(p)
}

impl App {
    /// Starts, stops or restarts the HTTP server to match the settings. A stdio server is left alone.
    pub fn sync_mcp(&mut self) {
        if self.mcp.as_ref().is_some_and(|h| h.addr.is_none()) {
            if let Some(h) = &self.mcp {
                h.set_allow_actions(self.s.mcp_allow_actions);
            }
            return;
        }
        if !self.s.mcp_enabled {
            if self.mcp.take().is_some() {
                self.say("MCP server stopped.");
            }
            self.mcp_error = None;
            return;
        }
        if self.s.mcp_token.is_empty() {
            self.s.mcp_token = mcp::new_token();
            self.save_settings();
        }
        if self.mcp.as_ref().is_some_and(|h| h.port != self.s.mcp_port) {
            self.mcp = None;
        }
        if self.mcp.is_none() {
            // One attempt, never a wait on the UI thread. A failure is shown on the Settings page and the
            // next settings change tries again. A new token is applied in place, without a rebind.
            match mcp::serve_http(self.s.mcp_port, self.s.mcp_token.clone(), self.s.mcp_allow_actions) {
                Ok(h) => {
                    self.say(format!("MCP server listening on {}", h.url().unwrap_or_default()));
                    self.mcp = Some(h);
                    self.mcp_error = None;
                }
                Err(e) => {
                    self.say(format!("MCP server: {e}"));
                    self.mcp_error = Some(e);
                }
            }
        }
        if let Some(h) = &self.mcp {
            h.set_allow_actions(self.s.mcp_allow_actions);
            h.set_token(&self.s.mcp_token);
        }
    }

    /// Runs the tool calls waiting from MCP clients; true when there were some.
    pub fn mcp_poll(&mut self) -> bool {
        // actions that ran off the UI thread and have ended: show the result, reload their page
        let finished: Vec<super::ActionDone> = self.action_done_rx.try_iter().collect();
        if !finished.is_empty() {
            for d in &finished {
                for t in &d.targets {
                    self.actions_running.remove(t);
                }
            }
            self.say_results(&finished);
        }
        let mut pages: Vec<usize> = finished.iter().map(|d| d.page).collect();
        pages.sort_unstable();
        pages.dedup();
        for page in &pages {
            if self.loading(*page) {
                // a collection started before the action ended may miss it: the next MCP read waits
                // for a new one, and the page reloads once the running collection is done
                self.loaded_at.remove(page);
                self.reload_after.insert(*page);
            } else {
                self.refresh_page(*page);
            }
        }
        let ready: Vec<usize> = self.reload_after.iter().copied().filter(|p| !self.loading(*p)).collect();
        for p in &ready {
            self.reload_after.remove(p);
            self.refresh_page(*p);
        }
        let jobs: Vec<mcp::Job> = match &self.mcp {
            Some(h) => h.rx.try_iter().collect(),
            None => return !finished.is_empty() || !ready.is_empty(),
        };
        let any = !jobs.is_empty() || !finished.is_empty() || !ready.is_empty();
        for j in jobs {
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.mcp_call(&j.tool, &j.args, j.first)))
                .unwrap_or_else(|_| Outcome::Done(Err(format!("internal error in {}", j.tool))));
            let _ = j.reply.send(out);
        }
        any
    }

    pub fn mcp_call(&mut self, tool: &str, args: &Value, first: bool) -> Outcome {
        if mcp::is_action(tool) && !self.s.mcp_allow_actions {
            return Outcome::Done(Err("Actions are turned off in RustyTM's settings.".into()));
        }
        self.poll_bg();
        self.mcp_run = None;
        let r = self.mcp_dispatch(tool, args, first);
        // slow work the tool left for the transport thread
        if let Some(f) = self.mcp_run.take() {
            if matches!(r, Ok(Some(_))) {
                return Outcome::Run(f);
            }
        }
        match r {
            Ok(Some(v)) => Outcome::Done(Ok(v)),
            Ok(None) => Outcome::Pending,
            Err(e) => Outcome::Done(Err(e)),
        }
    }

    /// Makes sure a background-collected page has data no older than `max_age` seconds;
    /// false while it is being collected.
    fn mcp_ensure(&mut self, page: usize, max_age: u64, args: &Value, first: bool) -> bool {
        if first && !self.loading(page) {
            let stale = self.loaded_at.get(&page).is_none_or(|t| t.elapsed().as_secs() >= max_age);
            if stale || arg_bool(args, "refresh").unwrap_or(false) {
                self.refresh_page(page);
            }
        }
        self.poll_bg();
        !self.loading(page)
    }

    fn mcp_dispatch(&mut self, tool: &str, a: &Value, first: bool) -> R {
        let v = match tool {
            "get_overview" => self.mcp_overview(),
            "get_cpu" => {
                let l = load_avg();
                json!({
                "model": self.cpu_static.model, "logical": self.cpu_static.logical, "physical": self.cpu_static.physical, "sockets": self.cpu_static.sockets,
                "numa_nodes": self.cpu_static.numa, "caches": self.cpu_static.caches, "base_mhz": self.cpu_static.base_mhz, "governor": self.cpu_static.governor,
                "driver": self.cpu_static.driver, "energy_preference": self.cpu_static.epp, "virtualization": self.cpu_static.virt, "efficiency_cores": self.cpu_static.efficiency,
                "usage_pct": self.cpu.overall, "kernel_pct": self.cpu.kernel, "per_cpu_pct": self.cpu.per, "per_cpu_kernel_pct": self.cpu.per_kernel,
                "frequencies_mhz": self.freqs, "load_average": [l.0, l.1, l.2],
                "history_pct": self.hist.cpu, "sample_interval_ms": self.s.refresh_ms,
            })
            }
            "get_memory" => json!({ "memory": self.mem, "history_pct": self.hist.mem, "sample_interval_ms": self.s.refresh_ms }),
            "get_disks" => json!({ "disks": self.disks, "filesystems": mounts() }),
            "get_network" => {
                let (rx, tx) = self.net_totals();
                json!({ "interfaces": self.nets, "total_rx_rate": rx, "total_tx_rate": tx })
            }
            "get_gpus" => {
                let mut out = json!({ "gpus": *self.gpu_now });
                if arg_bool(a, "include_processes").unwrap_or(true) {
                    let shown = matches!(self.page, P_PROCESSES | P_PERF);
                    let running = shown || self.mcp_gpu_last.is_some_and(|t| t.elapsed() < Duration::from_secs(30));
                    if !running {
                        self.mcp_gpu_since = Some(Instant::now());
                    }
                    self.mcp_gpu_last = Some(Instant::now());
                    self.sync_gpu_demand();
                    // the worker needs two passes for engine loads
                    if !shown && self.mcp_gpu_since.is_some_and(|t| t.elapsed() < Duration::from_millis(3200)) {
                        return Ok(None);
                    }
                    self.sync_gpu_procs();
                    out["processes"] = json!(*self.gpu_procs_now);
                }
                out
            }
            "get_history" => {
                let h = &self.hist;
                let mut v = json!({
                    "sample_interval_ms": self.s.refresh_ms,
                    "cpu_pct": h.cpu, "memory_pct": h.mem, "busiest_disk_pct": h.disk, "net_rx_rate": h.rx, "net_tx_rate": h.tx, "gpu0_pct": h.gpu, "power_w": h.power,
                });
                if arg_bool(a, "include_devices").unwrap_or(false) {
                    v["devices"] = json!(self.dh);
                    v["device_keys"] = json!("disk:<dev> active %, dr:/dw:<dev> read/write bytes/s, rx:/tx:<if> bytes/s, gpu:<i> %, gmem:<i> memory %, temp:<sensor> tenths of °C");
                }
                v
            }
            "list_processes" => self.mcp_processes(a),
            "get_process" => self.mcp_process(a)?,
            "get_sensors" => {
                let g = arg_str(a, "group").unwrap_or("").to_lowercase();
                let q = filter_arg(a);
                let list: Vec<&Sensor> = self.sensors.iter().filter(|s| (g.is_empty() || s.group.to_lowercase() == g) && hit(&q, &[&s.device, &s.label, s.group])).collect();
                let groups: std::collections::BTreeSet<&str> = self.sensors.iter().map(|s| s.group).collect();
                json!({ "sensors": list, "groups": groups, "restricted_sources": self.restricted })
            }
            "get_batteries" => {
                let list: Vec<Value> = self
                    .batteries
                    .iter()
                    .map(|b| {
                        let mut v = json!(b);
                        v["health_pct"] = json!(b.health());
                        v["time_estimate_secs"] = json!(self.battery_eta(b));
                        v
                    })
                    .collect();
                json!({ "batteries": list, "ac_online": self.ac })
            }
            "get_connections" => {
                if !self.mcp_ensure(P_CONN, 3, a, first) {
                    return Ok(None);
                }
                let Some(c) = &self.conns else { return Ok(None) };
                let scope = arg_str(a, "scope").unwrap_or("all");
                let ipc = arg_bool(a, "include_ipc").unwrap_or(false);
                let q = filter_arg(a);
                let all: Vec<Value> = c
                    .conns
                    .iter()
                    .filter(|x| ipc || x.exposure != "Local IPC")
                    .filter(|x| match scope {
                        "listening" => x.listening,
                        "public" => x.listening && (x.exposure == "All interfaces" || x.exposure == "Public"),
                        _ => true,
                    })
                    .filter(|x| {
                        let owners = x.owners.iter().map(|o| format!("{} {}", o.1, o.0)).collect::<Vec<_>>().join(" ");
                        hit(&q, &[&x.local, &x.remote, &x.service, &x.state, x.proto, &owners])
                    })
                    .map(|x| {
                        json!({
                            "proto": x.proto, "state": x.state, "local": x.local, "remote": x.remote, "service": x.service, "exposure": x.exposure,
                            "listening": x.listening, "uid": x.uid, "processes": x.owners.iter().map(|o| json!({"pid": o.0, "name": o.1})).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                let n = all.len();
                json!({ "count": n, "connections": all.into_iter().take(limit(a, 300)).collect::<Vec<_>>(), "warnings": c.warnings })
            }
            "get_kernel_modules" => {
                if !self.mcp_ensure(P_DRIVERS, 10, a, first) {
                    return Ok(None);
                }
                let Some(m) = &self.mods else { return Ok(None) };
                if let Some(name) = arg_str(a, "name").filter(|n| !n.trim().is_empty()) {
                    let Some(x) = m.modules.iter().find(|x| x.name.eq_ignore_ascii_case(name.trim()) || x.name.replace('-', "_").eq_ignore_ascii_case(&name.trim().replace('-', "_"))) else {
                        return Err(format!("no kernel module named {name}"));
                    };
                    let mut v = json!(x);
                    v["unload_hint"] = json!(inv::unload_hint(x));
                    return Ok(Some(v));
                }
                let q = filter_arg(a);
                let loaded = arg_bool(a, "loaded_only").unwrap_or(false);
                let list: Vec<Value> = m
                    .modules
                    .iter()
                    .filter(|x| (!loaded || x.loaded) && hit(&q, &[&x.name, &x.source, &x.state]))
                    .map(|x| json!({"name": x.name, "loaded": x.loaded, "builtin": x.builtin, "state": x.state, "source": x.source, "size": x.size, "refs": x.refs, "used_by": x.used_by}))
                    .collect();
                json!({ "kernel_release": m.kernel_release, "count": list.len(), "modules": list.into_iter().take(limit(a, 300)).collect::<Vec<_>>(), "notes": m.notes, "slab_bytes": m.slab })
            }
            "get_installed_apps" => {
                if !self.mcp_ensure(P_APPS, 600, a, first) {
                    return Ok(None);
                }
                let q = filter_arg(a);
                let pk = arg_bool(a, "include_packages").unwrap_or(false);
                let list: Vec<&InstalledApp> = self.apps.iter().filter(|x| (pk || !x.packages_only) && hit(&q, &[&x.name, &x.publisher, x.source])).collect();
                json!({ "count": list.len(), "apps": list.into_iter().take(limit(a, 300)).collect::<Vec<_>>(), "uninstall_running": self.uninstall_bg.busy() })
            }
            "get_disk_health" => {
                if !self.mcp_ensure(P_HEALTH, 60, a, first) {
                    return Ok(None);
                }
                json!(self.health)
            }
            "get_startup_apps" => {
                if !self.mcp_ensure(P_STARTUP, 10, a, first) {
                    return Ok(None);
                }
                let q = filter_arg(a);
                json!({ "startup_apps": self.startup.iter().filter(|s| hit(&q, &[&s.name, &s.command])).collect::<Vec<_>>() })
            }
            "get_services" => {
                if !self.mcp_ensure(P_SERVICES, 5, a, first) {
                    return Ok(None);
                }
                let q = filter_arg(a);
                let st = arg_str(a, "state").unwrap_or("").to_lowercase();
                let mut by_state: std::collections::BTreeMap<&str, usize> = Default::default();
                for s in &self.services {
                    *by_state.entry(s.active.as_str()).or_default() += 1;
                }
                let list: Vec<&Service> = self.services.iter().filter(|s| (st.is_empty() || s.active.to_lowercase() == st) && hit(&q, &[&s.unit, &s.description, &s.active, &s.sub])).collect();
                json!({ "count": list.len(), "by_active_state": by_state, "services": list.into_iter().take(limit(a, 500)).collect::<Vec<_>>() })
            }
            "get_users" => {
                if !self.mcp_ensure(P_USERS, 5, a, first) {
                    return Ok(None);
                }
                let mut m: HashMap<u32, (String, usize, f32, u64)> = HashMap::new();
                for p in self.procs() {
                    let e = m.entry(p.uid).or_insert_with(|| (p.user.clone(), 0, 0.0, 0));
                    e.1 += 1;
                    e.2 += p.cpu;
                    e.3 += p.rss;
                }
                let mut v: Vec<_> = m.into_iter().collect();
                v.sort_by(|x, y| y.1 .2.partial_cmp(&x.1 .2).unwrap_or(std::cmp::Ordering::Equal));
                let users: Vec<Value> = v
                    .into_iter()
                    .map(|(uid, (name, n, cpu, rss))| json!({"uid": uid, "user": name, "processes": n, "cpu_pct": cpu, "memory": rss, "sessions": self.sessions.iter().filter(|s| s.uid == uid).collect::<Vec<_>>()}))
                    .collect();
                json!({ "users": users })
            }
            "get_system_info" => {
                if !self.mcp_ensure(P_SYSINFO, 300, a, first) {
                    return Ok(None);
                }
                json!({ "sections": self.sysinfo.iter().map(|(n, items)| json!({"name": n, "items": items})).collect::<Vec<_>>() })
            }
            "get_app_history" => {
                let q = filter_arg(a);
                let mut v: Vec<_> = self.app_hist.iter().filter(|(k, _)| hit(&q, &[k])).collect();
                v.sort_by(|x, y| y.1 .0.partial_cmp(&x.1 .0).unwrap_or(std::cmp::Ordering::Equal));
                let n = v.len();
                json!({ "count": n, "apps": v.into_iter().take(limit(a, 50)).map(|(k, x)| json!({"name": k, "cpu_secs": (x.0 * 10.0).round() / 10.0, "disk_read": x.1, "disk_written": x.2})).collect::<Vec<_>>() })
            }
            "get_disk_scan" => {
                let s = self.scanner.snapshot();
                let n = limit(a, 30);
                let mut ext: Vec<_> = s.by_ext.iter().collect();
                ext.sort_by_key(|x| std::cmp::Reverse(x.1 .1));
                json!({
                    "root": s.root, "running": s.running, "paused": s.paused, "done": s.done, "stopped": s.stopped,
                    "files": s.files, "bytes": s.bytes, "unreadable": s.unreadable, "current": s.current,
                    "by_category": crate::diskscan::CATEGORIES.iter().zip(s.by_cat).map(|(c, b)| json!({"category": c, "bytes": b})).collect::<Vec<_>>(),
                    "top_extensions": ext.into_iter().take(n).map(|(e, (f, b))| json!({"ext": e, "files": f, "bytes": b})).collect::<Vec<_>>(),
                    "largest_files": s.top_files.iter().take(n).map(|(b, p)| json!({"bytes": b, "path": p})).collect::<Vec<_>>(),
                    "largest_folders": s.top_dirs.iter().take(n).map(|(b, p)| json!({"bytes": b, "path": p})).collect::<Vec<_>>(),
                    "filesystems": mounts(),
                })
            }
            "get_benchmarks" => {
                self.bench_hist = bench::load_history();
                json!({ "state": self.bench.snapshot(), "history": self.bench_hist, "folder": self.bench_dir, "note": "score 100 = this tool's reference machine; disk MB/s, CPU Mops/Gops per second, GPU GFLOPS and GB/s, internet Mb/s" })
            }
            "get_recorder" => {
                let rec = self.rec.as_ref().map(|r| json!({"active": r.active, "samples": r.trace.samples.len(), "elapsed_secs": r.elapsed_secs(), "limit_reached": r.limit_reached, "saved": r.saved, "host": r.trace.host}));
                let replay = self.replay.as_ref().map(|r| json!({"position": r.pos, "samples": r.trace.samples.len(), "playing": r.playing, "host": r.trace.host, "temp_names": r.trace.temp_names, "gpu_names": r.trace.gpu_names, "sample": r.trace.samples.get(r.pos)}));
                self.traces = recorder::list_traces();
                json!({ "recording": rec, "replay": replay, "saved_traces": self.traces, "max_samples": recorder::MAX_SAMPLES })
            }
            "get_trace" => {
                let p = mcp_trace_path(required(a, "path")?)?;
                let max = arg_i64(a, "max_samples").map(|m| m.clamp(1, 36_000) as usize).unwrap_or(120);
                // parsing a long trace takes a while: the transport thread does it
                self.mcp_run = Some(Box::new(move || {
                    let t = recorder::load(&p)?;
                    let n = t.samples.len();
                    let step = n.div_ceil(max).max(1);
                    let avg = t.samples.iter().map(|s| s.cpu_overall as f64).sum::<f64>() / n.max(1) as f64;
                    let peak = t.samples.iter().map(|s| s.cpu_overall).fold(0.0f32, f32::max);
                    Ok(json!({
                        "path": p, "host": t.host, "recorded_at": t.recorded_at, "logical_cpus": t.logical, "mem_total": t.mem_total, "temp_names": t.temp_names, "gpu_names": t.gpu_names,
                        "samples": n, "duration_secs": t.samples.last().map(|s| s.t_ms as f64 / 1000.0).unwrap_or(0.0), "cpu_avg_pct": avg, "cpu_peak_pct": peak,
                        "every_nth_sample": step, "data": t.samples.iter().step_by(step).collect::<Vec<_>>(),
                    }))
                }));
                Value::Null
            }
            "get_settings" => {
                let mut v = json!(self.s);
                if let Some(o) = v.as_object_mut() {
                    o.remove("mcp_token");
                    o.remove("collapsed");
                }
                v["mcp_server"] = self.mcp_status();
                v
            }
            _ => return self.mcp_action(tool, a, first),
        };
        Ok(Some(v))
    }

    pub fn mcp_status(&self) -> Value {
        json!({
            "transport": self.mcp.as_ref().map(|h| if h.addr.is_some() { "http" } else { "stdio" }),
            "url": self.mcp.as_ref().and_then(|h| h.url()),
            "allow_actions": self.s.mcp_allow_actions,
            "error": self.mcp_error,
        })
    }

    fn mcp_overview(&self) -> Value {
        let (host, kernel, arch) = uname();
        let l = load_avg();
        let (rx, tx) = self.net_totals();
        let procs = self.procs();
        let gpu = self.gpu_by_pid();
        let mut by_cpu: Vec<&Proc> = procs.iter().collect();
        by_cpu.sort_by(|x, y| y.cpu.partial_cmp(&x.cpu).unwrap_or(std::cmp::Ordering::Equal));
        let mut by_mem: Vec<&Proc> = procs.iter().collect();
        by_mem.sort_by_key(|p| std::cmp::Reverse(p.rss));
        let brief = |v: Vec<&Proc>| v.into_iter().take(5).map(|p| proc_json(p, &gpu)).collect::<Vec<_>>();
        json!({
            "host": host, "kernel": kernel, "arch": arch, "uptime_secs": uptime_secs(), "load_average": [l.0, l.1, l.2], "runnable_total": l.3,
            "cpu": {"model": self.cpu_static.model, "logical": self.cpu_static.logical, "usage_pct": self.cpu.overall, "kernel_pct": self.cpu.kernel},
            "memory": {"total": self.mem.total, "used": self.mem.used, "available": self.mem.available, "swap_total": self.mem.swap_total, "swap_used": self.mem.swap_used},
            "gpus": self.gpu_now.iter().map(|g| json!({"name": g.name, "usage_pct": g.util, "memory_used": g.mem_used, "memory_total": g.mem_total, "temp_c": g.temp, "power_w": g.power})).collect::<Vec<_>>(),
            "disks": self.disks.iter().map(|d| json!({"name": d.name, "active_pct": d.active_pct, "read_rate": d.read_rate, "write_rate": d.write_rate})).collect::<Vec<_>>(),
            "network": {"rx_rate": rx, "tx_rate": tx},
            "batteries": self.batteries.iter().map(|b| json!({"name": b.name, "percent": b.percent, "status": b.status, "peripheral": b.peripheral})).collect::<Vec<_>>(),
            "ac_online": self.ac,
            "hottest_temp_c": self.thermals().iter().map(|t| t.celsius).fold(None, |m: Option<f64>, c| Some(m.map_or(c, |m| m.max(c)))),
            "processes": procs.len(), "threads": procs.iter().map(|p| p.threads as u64).sum::<u64>(),
            "top_cpu": brief(by_cpu), "top_memory": brief(by_mem),
            "rustytm": {
                "version": env!("CARGO_PKG_VERSION"), "page": PAGES[self.page], "recording": self.rec.as_ref().is_some_and(|r| r.active),
                "benchmark_running": self.bench.running(), "disk_scan_running": self.scanner.running(), "message": self.msg.as_ref().map(|m| &m.0),
            },
        })
    }

    fn mcp_processes(&self, a: &Value) -> Value {
        let f = arg_str(a, "filter").unwrap_or("").trim().to_string();
        let q = f.to_lowercase();
        let mut list: Vec<&Proc> = self.procs().iter().collect();
        if arg_bool(a, "include_exited").unwrap_or(false) {
            list.extend(self.exited.values().map(|(p, _)| p));
        }
        list.retain(|p| q.is_empty() || p.pid.to_string() == f || hit(&q, &[&p.name, &p.user, &p.cmdline]));
        let gpu = self.gpu_by_pid();
        let key = arg_str(a, "sort_by").unwrap_or("cpu");
        let g = |p: &Proc| gpu.get(&(p.pid as u32)).copied().unwrap_or((None, None));
        let fcmp = |x: f64, y: f64| x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
        list.sort_by(|x, y| match key {
            "name" => x.name.to_lowercase().cmp(&y.name.to_lowercase()),
            "pid" => x.pid.cmp(&y.pid),
            "memory" => x.rss.cmp(&y.rss),
            "disk_read" => fcmp(x.read_rate, y.read_rate),
            "disk_write" => fcmp(x.write_rate, y.write_rate),
            "user" => x.user.cmp(&y.user),
            "status" => x.state_label().cmp(y.state_label()),
            "threads" => x.threads.cmp(&y.threads),
            "gpu" => fcmp(g(x).0.unwrap_or(-1.0) as f64, g(y).0.unwrap_or(-1.0) as f64),
            "vram" => g(x).1.cmp(&g(y).1),
            _ => fcmp(x.cpu as f64, y.cpu as f64),
        });
        if arg_bool(a, "descending").unwrap_or(true) {
            list.reverse();
        }
        let n = list.len();
        json!({
            "count": n, "total_processes": self.procs().len(),
            "units": "memory bytes (resident), disk rates bytes/s, cpu % (100 = all cores unless per_core_cpu)",
            "processes": list.into_iter().take(limit(a, 50)).map(|p| proc_json(p, &gpu)).collect::<Vec<_>>(),
        })
    }

    fn mcp_process(&self, a: &Value) -> Result<Value, String> {
        let pid = pid_arg(a)?;
        let p = self.plist.iter().find(|p| p.pid == pid);
        if p.is_none() && !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return Err(format!("no process with PID {pid}"));
        }
        let gpu = self.gpu_by_pid();
        let mut detail = proc::detail(pid);
        if detail.len() > 30_000 {
            let mut cut = 30_000;
            while !detail.is_char_boundary(cut) {
                cut -= 1;
            }
            detail.truncate(cut);
            detail.push_str("\n… (truncated)");
        }
        Ok(json!({
            "process": p.map(|p| proc_json(p, &gpu)),
            "cmdline": p.map(|p| &p.cmdline),
            "affinity": proc::get_affinity(pid),
            "children": self.procs().iter().filter(|c| c.ppid == pid).map(|c| json!({"pid": c.pid, "name": c.name})).collect::<Vec<_>>(),
            "descendants": proc::descendants(self.procs(), pid).len(),
            "gpu": self.gpu_procs_now.procs.iter().filter(|g| g.pid == pid as u32).collect::<Vec<_>>(),
            "details": detail,
        }))
    }

    // ───────────── actions ─────────────

    fn mcp_action(&mut self, tool: &str, a: &Value, first: bool) -> R {
        let done = |m: String| Ok(Some(json!({ "ok": true, "message": m })));
        match tool {
            "signal_process" => {
                let pid = pid_arg(a)?;
                let sig = parse_signal(arg_str(a, "signal").unwrap_or("TERM")).ok_or("unknown signal")?;
                let mut pids = vec![pid];
                if arg_bool(a, "include_descendants").unwrap_or(false) {
                    pids.extend(proc::descendants(self.procs(), pid));
                }
                let mut failed = Vec::new();
                for &p in &pids {
                    if let Err(e) = proc::signal(p, sig) {
                        failed.push(json!({"pid": p, "error": e}));
                    }
                }
                let name = self.plist.iter().find(|p| p.pid == pid).map(|p| p.name.clone()).unwrap_or_default();
                self.say(format!("MCP: signal {sig} sent to {} ({pid}){}", name, if pids.len() > 1 { format!(" and {} descendants", pids.len() - 1) } else { String::new() }));
                Ok(Some(json!({ "ok": failed.is_empty(), "signal": sig, "sent": pids.len() - failed.len(), "failed": failed })))
            }
            "set_process_priority" => {
                let pid = pid_arg(a)?;
                let n = arg_i64(a, "nice").filter(|n| (-20..=19).contains(n)).ok_or("nice must be between -20 and 19")? as i32;
                proc::set_nice(pid, n)?;
                self.say(format!("MCP: priority of {pid} set to {n}"));
                done(format!("nice of {pid} set to {n}"))
            }
            "set_process_affinity" => {
                let pid = pid_arg(a)?;
                if self.cpu_static.logical > 1024 {
                    return Err("unavailable on systems with more than 1024 processors".into());
                }
                let list = proc::parse_cpu_list(required(a, "cpus")?, self.cpu_static.logical)?;
                let (ok, total) = proc::set_affinity(pid, &list)?;
                self.say(format!("MCP: affinity of {pid} set"));
                done(format!("affinity applied to {ok} of {total} threads"))
            }
            "write_process_report" => {
                let p = proc::sample_report(self.procs(), &data_dir().join("reports"))?;
                done(format!("{}", p.display()))
            }
            "run_program" => {
                let c = required(a, "command")?.to_string();
                run_detached(&c).map_err(|e| format!("could not run \"{c}\": {e}"))?;
                self.say(format!("MCP: started {c}"));
                done(format!("started: {c}"))
            }
            "service_action" => {
                let unit = required(a, "unit")?;
                let act = required(a, "action")?;
                if !["start", "stop", "restart", "enable", "disable"].contains(&act) {
                    return Err("action must be start, stop, restart, enable or disable".into());
                }
                if unit.starts_with('-') || unit.chars().any(char::is_whitespace) {
                    return Err("invalid unit name".into());
                }
                let unit = unit.to_string();
                let act = act.to_string();
                let ticket = self.begin_action(P_SERVICES, vec![format!("service:{unit}")])?;
                self.say(format!("MCP: {act} {unit}…"));
                // systemctl (and pkexec's password dialog) can take long: run it off the UI thread,
                // then tell the UI how it ended so it shows the result and reloads the services
                self.mcp_run = Some(Box::new(move || {
                    let r = inv::service_action(&unit, &act);
                    ticket.finish(match &r { Ok(_) => Ok(format!("MCP: {act} {unit} succeeded")), Err(e) => Err(format!("MCP: {e}")) });
                    r?;
                    Ok(json!({ "ok": true, "message": format!("{act} {unit} succeeded") }))
                }));
                Ok(Some(Value::Null))
            }
            "set_startup_app_enabled" => {
                if !self.mcp_ensure(P_STARTUP, 10, a, first) {
                    return Ok(None);
                }
                let name = required(a, "name")?;
                let en = arg_bool(a, "enabled").ok_or("missing argument: enabled")?;
                let s = self.startup.iter().find(|s| s.name.eq_ignore_ascii_case(name) || s.file == name).cloned().ok_or_else(|| format!("no startup app named {name}"))?;
                inv::set_startup_enabled(&s, en)?;
                self.say(format!("MCP: {} {}", if en { "enabled" } else { "disabled" }, s.name));
                self.refresh_page(P_STARTUP);
                done(format!("{} {}", if en { "enabled" } else { "disabled" }, s.name))
            }
            "lock_session" | "terminate_session" => {
                if !self.mcp_ensure(P_USERS, 5, a, first) {
                    return Ok(None);
                }
                let id = required(a, "session_id")?.to_string();
                if !self.sessions.iter().any(|s| s.id == id) {
                    return Err(format!("no login session {id} (see get_users)"));
                }
                let lock = tool == "lock_session";
                let ticket = self.begin_action(P_USERS, vec![format!("session:{id}")])?;
                self.say(format!("MCP: {} session {id}", if lock { "locking" } else { "signing off" }));
                // loginctl off the UI thread as well; the UI learns the result when it ends
                self.mcp_run = Some(Box::new(move || {
                    let r = if lock { inv::lock_session(&id) } else { inv::terminate_session(&id) };
                    let what = format!("session {id}: {}", if lock { "locked" } else { "signed off" });
                    ticket.finish(match &r { Ok(_) => Ok(format!("MCP: {what}")), Err(e) => Err(format!("MCP: {e}")) });
                    r?;
                    Ok(json!({ "ok": true, "message": what }))
                }));
                Ok(Some(Value::Null))
            }
            "uninstall_app" => {
                if !self.mcp_ensure(P_APPS, 600, a, first) {
                    return Ok(None);
                }
                if self.uninstall_bg.busy() {
                    return Err("an uninstall is already running".into());
                }
                let name = required(a, "name")?;
                let found: Vec<&InstalledApp> = self.apps.iter().filter(|x| x.name.eq_ignore_ascii_case(name)).collect();
                let app = match found.as_slice() {
                    [one] => (*one).clone(),
                    [] => return Err(format!("no installed app named {name}")),
                    many => return Err(format!("several apps are named {name}: {}", many.iter().map(|x| x.source).collect::<Vec<_>>().join(", "))),
                };
                let p = inv::uninstall_plan(&app)?;
                let argv = p.argv.clone();
                self.say(format!("MCP: uninstalling {}…", app.name));
                self.uninstall_bg.spawn(move || inv::uninstall(&argv));
                Ok(Some(json!({ "ok": true, "started": true, "manager": p.manager, "package": p.package, "command": p.argv.join(" "), "message": "running in the background; get_installed_apps shows uninstall_running" })))
            }
            "start_disk_scan" => {
                if self.scanner.running() {
                    return Err("a scan is already running".into());
                }
                let p = PathBuf::from(arg_str(a, "path").map(str::trim).filter(|s| !s.is_empty()).unwrap_or("/"));
                if !p.is_dir() {
                    return Err(format!("{} is not a folder", p.display()));
                }
                self.scanner.start(p.clone());
                self.say(format!("MCP: measuring {}", p.display()));
                done(format!("scanning {}", p.display()))
            }
            "pause_disk_scan" => {
                let want = arg_bool(a, "paused").ok_or("missing argument: paused")?;
                let s = self.scanner.snapshot();
                if !s.running {
                    return Err("no scan is running".into());
                }
                if s.paused != want {
                    self.scanner.toggle_pause();
                }
                done(if want { "paused".into() } else { "resumed".into() })
            }
            "stop_disk_scan" => {
                if !self.scanner.running() {
                    return Err("no scan is running".into());
                }
                self.scanner.stop();
                done("stopped".into())
            }
            "start_benchmark" => {
                let k = match required(a, "kind")? {
                    "cpu" => Kind::Cpu,
                    "disk" => Kind::Disk,
                    "gpu" => Kind::Gpu,
                    "internet" => Kind::Internet,
                    "all" => Kind::All,
                    o => return Err(format!("unknown benchmark {o} (cpu, disk, gpu, internet, all)")),
                };
                if self.bench.running() {
                    return Err("a benchmark is already running".into());
                }
                let dir = match arg_str(a, "folder").map(str::trim).filter(|s| !s.is_empty()) {
                    Some(d) if PathBuf::from(d).is_dir() => PathBuf::from(d),
                    Some(d) => return Err(format!("{d} is not a folder")),
                    None => self.bench_dir.clone(),
                };
                self.bench.start(k, dir);
                self.say(format!("MCP: {k:?} benchmark started"));
                done(format!("{k:?} benchmark started; follow it with get_benchmarks"))
            }
            "cancel_benchmark" => {
                if !self.bench.running() {
                    return Err("no benchmark is running".into());
                }
                self.bench.cancel();
                done("cancelled".into())
            }
            "clear_benchmark_history" => {
                self.bench_hist.clear();
                bench::save_history(&[])?;
                self.say("MCP: benchmark history cleared");
                done("benchmark history cleared".into())
            }
            "start_recording" => {
                if self.rec.as_ref().is_some_and(|r| r.active) {
                    return Err("already recording".into());
                }
                if self.rec.is_some() {
                    return Err("save or discard the captured trace first".into());
                }
                let (names, _) = self.rec_temps();
                let gnames = self.gpu_now.iter().map(|g| g.name.clone()).collect();
                self.rec = Some(Recorder::start(uname().0, self.cpu_static.logical, self.mem.total, names, gnames));
                self.say("MCP: Flight Recorder started");
                done("recording".into())
            }
            "stop_recording" => {
                let r = self.rec.as_mut().filter(|r| r.active).ok_or("not recording")?;
                r.active = false;
                let n = r.trace.samples.len();
                self.say("MCP: recording stopped");
                done(format!("stopped after {n} samples; save_recording or discard_recording"))
            }
            "save_recording" => {
                let r = self.rec.as_mut().ok_or("no recording to save")?;
                let p = r.save().map_err(|e| format!("the trace could not be saved: {e}"))?;
                self.rec = None;
                self.traces = recorder::list_traces();
                self.say(format!("MCP: saved {}", p.display()));
                done(format!("{}", p.display()))
            }
            "discard_recording" => {
                self.rec.take().ok_or("no recording to discard")?;
                self.say("MCP: recording discarded");
                done("discarded".into())
            }
            "export_system_info" => {
                if !self.mcp_ensure(P_SYSINFO, 300, a, first) {
                    return Ok(None);
                }
                let dir = data_dir().join("exports");
                let _ = std::fs::create_dir_all(&dir);
                let p = dir.join("system-information.txt");
                std::fs::write(&p, export_info(&self.sysinfo)).map_err(|e| format!("export failed: {e}"))?;
                done(format!("{}", p.display()))
            }
            "clear_app_history" => {
                self.app_hist.clear();
                self.say("MCP: app history cleared");
                done("app history cleared".into())
            }
            "change_setting" => {
                const ALLOWED: [&str; 15] = [
                    "fahrenheit", "show_exited", "smooth_cpu", "process_tree", "per_core_cpu", "combined_gpus", "combined_disks", "combined_network", "net_bits", "status_bar", "sidebar", "light_theme", "confirm_stop_scan", "refresh_ms", "bench_dir",
                ];
                let name = required(a, "name")?;
                if !ALLOWED.contains(&name) {
                    return Err(format!("{name} cannot be changed (allowed: {})", ALLOWED.join(", ")));
                }
                let value = a.get("value").cloned().ok_or("missing argument: value")?;
                if name == "refresh_ms" && !value.as_u64().is_some_and(|v| (250..=10_000).contains(&v)) {
                    return Err("refresh_ms must be between 250 and 10000".into());
                }
                if name == "bench_dir" && !value.as_str().is_some_and(|d| PathBuf::from(d).is_dir()) {
                    return Err("bench_dir must be an existing folder".into());
                }
                let mut v = serde_json::to_value(&self.s).map_err(|e| e.to_string())?;
                v[name] = value;
                let s: Settings = serde_json::from_value(v).map_err(|e| format!("invalid value for {name}: {e}"))?;
                self.s = s;
                if name == "bench_dir" {
                    self.bench_dir = PathBuf::from(&self.s.bench_dir);
                }
                crate::util::set_net_bits(self.s.net_bits);
                self.save_settings();
                self.say(format!("MCP: setting {name} changed"));
                done(format!("{name} = {}", serde_json::to_value(&self.s).ok().and_then(|v| v.get(name).cloned()).unwrap_or_default()))
            }
            "show_page" => {
                let p = page_from_name(required(a, "page")?).ok_or("unknown page")?;
                self.goto(p);
                done(format!("showing {}", PAGES[p]))
            }
            _ => Err(format!("unknown tool: {tool}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(a: &mut App, tool: &str, args: Value) -> Result<Value, String> {
        for i in 0..200 {
            match a.mcp_call(tool, &args, i == 0) {
                Outcome::Done(r) => return r,
                Outcome::Run(f) => return f(),
                Outcome::Pending => {
                    std::thread::sleep(Duration::from_millis(50));
                    a.poll_bg();
                }
            }
        }
        Err("timed out".into())
    }

    #[test]
    fn read_tools_answer_without_a_terminal() {
        let mut a = App::new(Settings::default());
        let o = call(&mut a, "get_overview", json!({})).unwrap();
        assert!(o["processes"].as_u64().unwrap() > 0);
        let me = std::process::id() as i64;
        let l = call(&mut a, "list_processes", json!({"filter": me.to_string()})).unwrap();
        // other tests run commands whose command line holds this pid too: look for the exact row
        assert!(l["processes"].as_array().unwrap().iter().any(|p| p["pid"] == me), "{l}");
        let p = call(&mut a, "get_process", json!({"pid": me})).unwrap();
        assert!(p["details"].as_str().unwrap().len() > 10);
        assert!(call(&mut a, "get_process", json!({"pid": 0})).is_err());
        let s = call(&mut a, "get_settings", json!({})).unwrap();
        assert!(s.get("mcp_token").is_none());
        for t in ["get_cpu", "get_memory", "get_disks", "get_network", "get_history", "get_sensors", "get_batteries", "get_app_history", "get_disk_scan", "get_recorder", "get_startup_apps"] {
            assert!(call(&mut a, t, json!({})).is_ok(), "{t}");
        }
    }

    /// The TUI path: the server started from the settings answers HTTP while the app polls.
    #[test]
    fn http_server_started_from_settings() {
        use std::io::{Read, Write};
        let mut a = App::new(Settings { mcp_enabled: true, mcp_port: 0, mcp_token: "secret".into(), ..Default::default() });
        a.sync_mcp();
        let addr = a.mcp.as_ref().and_then(|h| h.addr).expect("listening");
        let client = std::thread::spawn(move || {
            let body = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"get_memory","arguments":{}}}"#;
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            write!(s, "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            out
        });
        for _ in 0..200 {
            a.mcp_poll();
            if client.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = client.join().unwrap();
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        assert!(out.contains("\"id\":7") && out.contains("swap_total"), "{out}");
        // the settings page shows the address and renders with the server on
        a.page = P_SETTINGS;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(150, 40)).unwrap();
        term.draw(|f| crate::draw::draw(f, &mut a)).unwrap();
        a.s.mcp_enabled = false;
        a.sync_mcp();
        assert!(a.mcp.is_none());
    }

    /// An action finishing on the transport thread reaches the UI: status message and data reload.
    #[test]
    fn finished_actions_update_the_ui() {
        let mut a = App::new(Settings { mcp_allow_actions: true, ..Default::default() });
        let f = match a.mcp_call("service_action", &json!({"unit": "rustytm-made-up-test.service", "action": "frobnicate"}), true) {
            Outcome::Done(r) => r,
            _ => panic!("bad action must fail at once"),
        };
        assert!(f.is_err());
        // idle page: the result starts a reload at once
        assert!(!a.loading(P_SERVICES) && !a.loaded_at.contains_key(&P_SERVICES));
        a.begin_action(P_SERVICES, vec![]).unwrap().finish(Ok("MCP: restart x succeeded".into()));
        assert!(a.mcp_poll());
        assert_eq!(a.msg.as_ref().unwrap().0, "MCP: restart x succeeded");
        assert!(a.loaded_at.contains_key(&P_SERVICES), "a reload must have started");
        // a collection already running may miss the change: its timestamp is dropped so the next read redoes it
        let (release, wait) = std::sync::mpsc::channel::<()>();
        a.svc_bg = crate::util::Bg::new();
        a.svc_bg.spawn(move || {
            let _ = wait.recv();
            Vec::new()
        });
        a.loaded_at.insert(P_SERVICES, std::time::Instant::now());
        assert!(a.loading(P_SERVICES));
        a.begin_action(P_SERVICES, vec![]).unwrap().finish(Ok("MCP: stop x succeeded".into()));
        assert!(a.mcp_poll());
        // the earlier result is still on screen: the new one is added to it
        assert_eq!(a.msg.as_ref().unwrap().0, "MCP: restart x succeeded · MCP: stop x succeeded");
        assert!(!a.loaded_at.contains_key(&P_SERVICES), "a running collection must not count as fresh");
        // nothing reloads while that collection runs...
        a.mcp_poll();
        assert!(!a.loaded_at.contains_key(&P_SERVICES));
        // ...then, once it is done, the page reloads by itself
        drop(release);
        let t = std::time::Instant::now();
        while !a.loaded_at.contains_key(&P_SERVICES) {
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "the page never reloaded");
            std::thread::sleep(std::time::Duration::from_millis(10));
            a.poll_bg();
            a.mcp_poll();
        }
        assert!(a.reload_after.is_empty());
    }

    /// TUI actions (services, sessions) run off the UI thread and report back through the same channel.
    #[test]
    fn tui_actions_run_off_the_ui_thread() {
        let mut a = App::new(Settings::default());
        let (go, wait) = std::sync::mpsc::channel::<()>();
        a.run_action("Locking session 1…".into(), P_USERS, vec!["session:1".into()], move || {
            let _ = wait.recv();
            Err("Could not lock the session.".into())
        });
        // the call returned while the action is still blocked
        assert_eq!(a.msg.as_ref().unwrap().0, "Locking session 1…");
        assert!(!a.mcp_poll());
        // a second action on the same session is refused while the first runs, from the TUI and from MCP
        a.run_action("again".into(), P_USERS, vec!["session:1".into()], || Ok("ran twice".into()));
        assert_eq!(a.msg.as_ref().unwrap().0, "An action on 1 is still running.");
        assert!(a.begin_action(P_USERS, vec!["session:1".into()]).is_err());
        go.send(()).unwrap();
        let t = std::time::Instant::now();
        while !a.mcp_poll() {
            assert!(t.elapsed() < std::time::Duration::from_secs(5), "the result never came back");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(a.msg.as_ref().unwrap().0, "Could not lock the session.");
        // once it has ended, the session is free again
        assert!(a.actions_running.is_empty());
        drop(a.begin_action(P_USERS, vec!["session:1".into()]).unwrap());
    }

    /// Results ending together are all shown, failures first; an unfinished action still frees its target.
    #[test]
    fn several_results_are_all_shown() {
        let mut a = App::new(Settings::default());
        a.begin_action(P_HEALTH, vec!["service:a".into()]).unwrap().finish(Ok("start a succeeded".into()));
        a.begin_action(P_HEALTH, vec!["service:b".into()]).unwrap().finish(Err("Could not start \"b\".".into()));
        // dropped without finishing (a panic, or a client gone before the work ran)
        drop(a.begin_action(P_HEALTH, vec!["service:c".into()]).unwrap());
        assert!(a.mcp_poll());
        assert_eq!(a.msg.as_ref().unwrap().0, "Could not start \"b\". · The action did not complete. · start a succeeded");
        assert!(a.actions_running.is_empty());
        // a new message that is not an action result is not added to
        a.say("something else");
        a.begin_action(P_HEALTH, vec![]).unwrap().finish(Ok("stop a succeeded".into()));
        a.mcp_poll();
        assert_eq!(a.msg.as_ref().unwrap().0, "stop a succeeded");
        // a burst of failures keeps the line short: the newest 3, and a count of the older ones
        for i in 0..6 {
            a.begin_action(P_HEALTH, vec![]).unwrap().finish(Err(format!("fail {i}")));
        }
        a.mcp_poll();
        assert_eq!(a.msg.as_ref().unwrap().0, "+3 earlier failures · fail 3 · fail 4 · fail 5");
        a.begin_action(P_HEALTH, vec![]).unwrap().finish(Err("fail 6".into()));
        a.mcp_poll();
        assert_eq!(a.msg.as_ref().unwrap().0, "+4 earlier failures · fail 4 · fail 5 · fail 6");
        // a success arriving then is still shown: the newest result always stays
        a.begin_action(P_HEALTH, vec![]).unwrap().finish(Ok("start x succeeded".into()));
        a.mcp_poll();
        assert_eq!(a.msg.as_ref().unwrap().0, "+5 earlier failures · fail 5 · fail 6 · start x succeeded");
        // and the next result replaces that older success first
        a.begin_action(P_HEALTH, vec![]).unwrap().finish(Ok("stop x succeeded".into()));
        a.mcp_poll();
        assert_eq!(a.msg.as_ref().unwrap().0, "+5 earlier failures · fail 5 · fail 6 · stop x succeeded");
    }

    #[test]
    fn trace_paths_stay_in_the_traces_folder() {
        let same = mcp_trace_path("/etc/passwd").unwrap_err();
        assert_eq!(mcp_trace_path("../../../../etc/passwd").unwrap_err(), same);
        assert_eq!(mcp_trace_path("/nonexistent/x.rustytmtrace").unwrap_err(), same);
        let traces = data_dir().join("traces");
        assert_eq!(mcp_trace_path(&format!("{}/../settings.rustytmtrace", traces.display())).unwrap_err(), same);
    }

    #[test]
    fn actions_need_the_setting() {
        let mut a = App::new(Settings::default());
        assert!(call(&mut a, "show_page", json!({"page": "drivers"})).is_err());
        a.s.mcp_allow_actions = true;
        assert!(call(&mut a, "show_page", json!({"page": "drivers"})).is_ok());
        assert_eq!(a.page, P_DRIVERS);
        assert!(call(&mut a, "signal_process", json!({"pid": -1, "signal": "KILL"})).is_err());
        assert!(call(&mut a, "change_setting", json!({"name": "mcp_allow_actions", "value": true})).is_err());
        assert!(call(&mut a, "change_setting", json!({"name": "refresh_ms", "value": 5})).is_err());
    }
}
