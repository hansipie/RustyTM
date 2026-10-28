mod app;
mod bench;
mod collect;
mod diskscan;
mod draw;
mod recorder;
mod settings;
mod update;
mod util;

use app::*;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::Terminal;
use std::time::Duration;

const HELP: &str = "RustyTM — task manager for Linux

USAGE: rustytm [OPTIONS]
       rustytm update       check crates.io and reinstall the latest version (via cargo)

  --page=<name>        open on a page (summary, processes, performance, app-history, startup,
                       users, services, system-info, power-freq, thermals, flight-recorder,
                       connections, installed-apps, drivers, disk-space, benchmarks, disk-health,
                       settings)
  --dump=<page>        render one page as plain text (80x40..) and exit; no terminal needed
  --size=WxH           size used by --dump (default 150x45)
  --self-test          collect everything once, render every page headlessly, print a report
  --record[=SECS]      headless Flight Recorder: sample once a second (default 10 s) and save a trace
  --replay=<file>      validate a .rustytmtrace file and print its summary
  --bench=<kind>       run a benchmark without UI: cpu | disk | gpu | internet | all
  --reset-settings     delete the settings file
  --version, --help
";

fn arg<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.iter().find_map(|a| a.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

fn has(args: &[String], key: &str) -> bool {
    args.iter().any(|a| a == key || a.starts_with(&format!("{key}=")))
}

fn load_settings() -> settings::Settings {
    match settings::Settings::load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Settings could not be loaded: {e} (using defaults)");
            settings::Settings::default()
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("update") {
        std::process::exit(update::run());
    }
    if has(&args, "--help") || has(&args, "-h") {
        print!("{HELP}");
        return;
    }
    if has(&args, "--version") {
        println!("rustytm {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if has(&args, "--reset-settings") {
        println!("{}", settings::reset());
        return;
    }
    if let Some(f) = arg(&args, "--replay") {
        return replay_summary(f);
    }
    if let Some(k) = arg(&args, "--bench") {
        return headless_bench(k);
    }
    if has(&args, "--record") {
        let secs = arg(&args, "--record").and_then(|s| s.parse().ok()).unwrap_or(10);
        return headless_record(secs);
    }
    let (w, h) = arg(&args, "--size").and_then(|s| s.split_once('x')).and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?))).unwrap_or((150u16, 45u16));
    if has(&args, "--self-test") {
        return self_test(w, h);
    }
    if let Some(p) = arg(&args, "--dump") {
        let Some(page) = page_from_name(p) else {
            eprintln!("unknown page '{p}'");
            std::process::exit(2);
        };
        let mut a = ready_app(page);
        print!("{}", render_text(&mut a, w, h));
        return;
    }
    let mut s = load_settings();
    if let Some(p) = arg(&args, "--page") {
        match page_from_name(p) {
            Some(i) => s.last_page = i,
            None => {
                eprintln!("unknown page '{p}'");
                std::process::exit(2);
            }
        }
    }
    let mut a = App::new(s);
    let mut term = ratatui::init();
    let res = (|| -> std::io::Result<()> {
        loop {
            term.draw(|f| draw::draw(f, &mut a))?;
            let interval = Duration::from_millis(a.s.refresh_ms.max(250));
            let wait = interval.saturating_sub(a.last_tick.elapsed()).min(Duration::from_millis(200));
            if event::poll(wait)? {
                if let Event::Key(k) = event::read()? {
                    if k.kind != KeyEventKind::Release {
                        a.on_key(k);
                    }
                }
            }
            if a.last_tick.elapsed() >= interval {
                a.tick();
            }
            if a.quit {
                return Ok(());
            }
        }
    })();
    ratatui::restore();
    a.persist_on_exit();
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Builds an app with a second sample taken (rates need two samples) and all of `page`'s data loaded.
fn ready_app(page: usize) -> App {
    let mut a = App::new(settings::Settings::default());
    a.page = page;
    a.refresh_page(page);
    std::thread::sleep(Duration::from_millis(1200));
    a.tick();
    for _ in 0..200 {
        if !a.loading(page) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        a.tick();
    }
    a.tick();
    a
}

fn render_text(a: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).expect("test terminal");
    term.draw(|f| draw::draw(f, a)).expect("draw");
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn self_test(w: u16, h: u16) {
    let mut fails = 0;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!("[{}] {name}: {detail}", if ok { " ok " } else { "FAIL" });
        if !ok {
            fails += 1;
        }
    };
    let mut a = ready_app(P_SUMMARY);
    check("processes", a.procs.len() > 5, format!("{} processes, {} threads", a.procs.len(), a.procs.iter().map(|p| p.threads).sum::<u32>()));
    check("cpu", a.cpu_static.logical > 0 && a.cpu.per.len() == a.cpu_static.logical, format!("{} ({} logical / {} physical), {:.1}% busy", a.cpu_static.model, a.cpu_static.logical, a.cpu_static.physical, a.cpu.overall));
    check("memory", a.mem.total > 0, format!("{} total, {} used", util::bytes(a.mem.total as f64), util::bytes(a.mem.used as f64)));
    check("disks", !a.disks.is_empty(), a.disks.iter().map(|d| format!("{} {}", d.name, util::bytes(d.size as f64))).collect::<Vec<_>>().join(", "));
    check("network", a.nets.iter().any(|n| n.name != "lo"), a.nets.iter().map(|n| n.name.clone()).collect::<Vec<_>>().join(", "));
    check("gpu", true, format!("{} device(s) {}", a.gpu_now.len(), a.gpu_now.iter().map(|g| g.name.clone()).collect::<Vec<_>>().join(", ")));
    check("sensors", true, format!("{} sensors, {} restricted source(s)", a.sensors.len(), a.restricted.len()));
    for page in 0..PAGES.len() {
        if page != P_SUMMARY {
            a = ready_app(page);
        }
        let txt = render_text(&mut a, w, h);
        let rows = a.row_count(page);
        let n_nonblank = txt.lines().filter(|l| !l.trim().is_empty()).count();
        let detail = match page {
            P_STARTUP => format!("{} entries", a.startup.len()),
            P_USERS => format!("{} sessions, {} users", a.sessions.len(), a.users_view().len()),
            P_SERVICES => format!("{} services", a.services.len()),
            P_CONN => format!("{} sockets in scope", a.scope_conns().len()),
            P_APPS => format!("{} apps (+{} packages)", a.apps.iter().filter(|x| !x.packages_only).count(), a.apps.iter().filter(|x| x.packages_only).count()),
            P_DRIVERS => a.mods.as_ref().map(|m| format!("{} modules, {} loaded", m.modules.len(), m.modules.iter().filter(|x| x.loaded).count())).unwrap_or_default(),
            P_HEALTH => a.health.as_ref().map(|h| format!("{} disks", h.disks.len())).unwrap_or_default(),
            P_SYSINFO => format!("{} sections", a.sysinfo.len()),
            P_POWER => format!("{} sensors", a.sensors.len()),
            P_THERMAL => format!("{} temperature sensors", a.thermals().len()),
            _ => format!("{rows} rows"),
        };
        check(&format!("page {}", PAGES[page]), n_nonblank > 3, format!("rendered {n_nonblank} non-empty lines; {detail}"));
    }
    // disk scan of a small tree
    let mut sc = diskscan::Scanner::new();
    sc.start(std::path::PathBuf::from("/usr/share/applications"));
    for _ in 0..100 {
        if !sc.running() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let s = sc.snapshot();
    check("disk scan", s.done && s.files > 0, format!("{} files, {}", s.files, util::bytes(s.bytes as f64)));
    // recorder round trip
    let mut r = recorder::Recorder::start("test".into(), 2, 1 << 30, vec!["t".into()], vec![]);
    for i in 0..3 {
        r.push(recorder::Sample { cpu: vec![10.0 * i as f32, 5.0], temps: vec![40.0], ..Default::default() });
        std::thread::sleep(Duration::from_millis(20));
    }
    let ok = recorder::validate(&r.trace).is_ok();
    let mut bad = r.trace.clone();
    bad.samples[1].cpu = vec![1.0];
    check("recorder validation", ok && recorder::validate(&bad).is_err(), "valid trace accepted, mismatched trace rejected".into());
    let quick = bench::BenchResult { cpu_single_mops: Some(900.0), cpu_multi_gops: Some(6.0), disk_read_mbs: Some(1500.0), disk_write_mbs: Some(1200.0), ..Default::default() };
    let mut q = quick.clone();
    q.compute_score();
    check("score baseline", q.score.map_or(false, |s| (s - 100.0).abs() < 0.01), format!("{:?}", q.score));
    check("cpu list parser", collect::proc::parse_cpu_list("0-2,5", 8) == Ok(vec![0, 1, 2, 5]) && collect::proc::parse_cpu_list("9", 8).is_err(), "ranges and bounds".into());
    println!("{}", if fails == 0 { "\nall checks passed" } else { "\nSOME CHECKS FAILED" });
    if fails > 0 {
        std::process::exit(1);
    }
}

fn headless_record(secs: u64) {
    let mut a = App::new(settings::Settings::default());
    a.page = P_RECORDER;
    a.on_key(ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Char('r')));
    for _ in 0..secs {
        std::thread::sleep(Duration::from_secs(1));
        a.tick();
    }
    a.on_key(ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Char('r')));
    match a.rec.as_mut().map(|r| r.save()) {
        Some(Ok(p)) => println!("Saved {} ({} samples)", p.display(), a.rec.as_ref().map(|r| r.trace.samples.len()).unwrap_or(0)),
        Some(Err(e)) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        None => {
            eprintln!("error: no recording was started");
            std::process::exit(1);
        }
    }
}

fn replay_summary(path: &str) {
    match recorder::load(std::path::Path::new(path)) {
        Ok(t) => {
            let last = t.samples.last().map(|s| s.t_ms).unwrap_or(0);
            let avg = t.samples.iter().map(|s| s.cpu_overall as f64).sum::<f64>() / t.samples.len() as f64;
            let peak = t.samples.iter().map(|s| s.cpu_overall).fold(0.0f32, f32::max);
            println!("{path}: host {}, {} logical CPUs, {} samples over {:.1}s, avg CPU {avg:.1}%, peak {peak:.1}%, {} temperature sensors, {} GPUs", t.host, t.logical, t.samples.len(), last as f64 / 1000.0, t.temp_names.len(), t.gpu_names.len());
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn headless_bench(kind: &str) {
    let k = match kind {
        "cpu" => bench::Kind::Cpu,
        "disk" => bench::Kind::Disk,
        "gpu" => bench::Kind::Gpu,
        "internet" => bench::Kind::Internet,
        "all" => bench::Kind::All,
        _ => {
            eprintln!("unknown benchmark '{kind}' (cpu | disk | gpu | internet | all)");
            std::process::exit(2);
        }
    };
    let b = bench::Bench::new();
    b.start(k, std::env::temp_dir());
    loop {
        std::thread::sleep(Duration::from_millis(300));
        let s = b.snapshot();
        eprint!("\r{:<28} {:>3.0}%", s.label, s.progress * 100.0);
        if !s.running {
            eprintln!();
            let r = &s.result;
            let f = |v: Option<f64>, u: &str| v.map(|x| format!("{x:.1} {u}")).unwrap_or_else(|| "-".into());
            println!("cpu single  {}", f(r.cpu_single_mops, "Mops/sec"));
            println!("cpu multi   {}", f(r.cpu_multi_gops, "Gop/sec"));
            println!("disk read   {}", f(r.disk_read_mbs, "MB/sec"));
            println!("disk write  {}", f(r.disk_write_mbs, "MB/sec"));
            println!("gpu compute {}{}", f(r.gpu_gflops, "GFLOPS"), if r.gpu_device.is_empty() { String::new() } else { format!(" ({})", r.gpu_device) });
            println!("gpu memory  {}", f(r.gpu_mem_gbs, "GB/sec"));
            println!("download    {}", f(r.net_down_mbps, "Mb/s"));
            println!("upload      {}", f(r.net_up_mbps, "Mb/s"));
            println!("score       {}", r.score.map(|x| format!("{x:.0}")).unwrap_or_else(|| "-".into()));
            for e in &s.errors {
                eprintln!("{e}");
            }
            return;
        }
    }
}
