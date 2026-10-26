use crate::app::*;
use crate::bench::BenchResult;
use crate::collect::proc::Proc;
use crate::collect::system::*;
use crate::diskscan::CATEGORIES;
use crate::util::*;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Row, Sparkline, Table, Wrap};
use ratatui::Frame;
use std::collections::VecDeque;
use std::io::Write;

pub fn copy_osc52(text: &str) {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = text.as_bytes();
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    let mut so = std::io::stdout();
    let _ = write!(so, "\x1b]52;c;{out}\x07");
    let _ = so.flush();
}

fn accent(a: &App) -> Color {
    if a.s.light_theme { Color::Blue } else { Color::Cyan }
}

fn level_color(p: f32) -> Color {
    if p >= 85.0 { Color::Red } else if p >= 60.0 { Color::Yellow } else { Color::Green }
}

pub fn bar(p: f32, w: usize) -> String {
    let p = p.clamp(0.0, 100.0);
    let n = ((p / 100.0) * w as f32).round() as usize;
    format!("{}{}", "█".repeat(n.min(w)), "░".repeat(w - n.min(w)))
}

fn block<'a>(title: impl Into<String>, a: &App) -> Block<'a> {
    Block::default().borders(Borders::ALL).title(Span::styled(format!(" {} ", title.into()), Style::new().fg(accent(a)).add_modifier(Modifier::BOLD)))
}

fn vec_of(h: &VecDeque<u64>) -> Vec<u64> {
    h.iter().copied().collect()
}

fn spark(f: &mut Frame, area: Rect, title: String, data: &VecDeque<u64>, max: Option<u64>, color: Color, a: &App) {
    let v = vec_of(data);
    let mut sp = Sparkline::default().block(block(title, a)).data(&v).style(Style::new().fg(color));
    if let Some(m) = max {
        sp = sp.max(m);
    }
    f.render_widget(sp, area);
}

fn line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![Span::styled(format!("{label:<26}"), Style::new().fg(Color::DarkGray)), Span::raw(value)])
}

pub fn draw(f: &mut Frame, a: &mut App) {
    let area = f.area();
    let rows = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(3), Constraint::Length(if a.s.status_bar { 1 } else { 0 })]).split(area);
    let cols = if a.s.sidebar {
        Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(20), Constraint::Min(20)]).split(rows[0])
    } else {
        Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(0), Constraint::Min(20)]).split(rows[0])
    };
    if a.s.sidebar {
        sidebar(f, cols[0], a);
    }
    let c = cols[1];
    match a.page {
        P_SUMMARY => summary(f, c, a),
        P_PROCESSES => processes(f, c, a),
        P_PERF => performance(f, c, a),
        P_APPHIST => app_history(f, c, a),
        P_STARTUP => startup(f, c, a),
        P_USERS => users(f, c, a),
        P_SERVICES => services(f, c, a),
        P_SYSINFO => sysinfo(f, c, a),
        P_POWER => power(f, c, a),
        P_RECORDER => recorder(f, c, a),
        P_CONN => connections(f, c, a),
        P_APPS => installed(f, c, a),
        P_DRIVERS => drivers(f, c, a),
        P_DISK => disk_space(f, c, a),
        P_BENCH => benchmarks(f, c, a),
        _ => settings(f, c, a),
    }
    if a.s.status_bar {
        status(f, rows[1], a);
    }
    overlays(f, area, a);
}

fn sidebar(f: &mut Frame, area: Rect, a: &App) {
    let lines: Vec<Line> = PAGES
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let busy = if a.loading(i) { " …" } else { "" };
            let rec = if i == P_RECORDER && a.rec.as_ref().map_or(false, |r| r.active) { " ●" } else { "" };
            let st = if i == a.page { Style::new().fg(Color::Black).bg(accent(a)).add_modifier(Modifier::BOLD) } else { Style::new() };
            Line::from(Span::styled(format!(" {n}{busy}{rec}"), st))
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(block("RustyTM", a)), area);
}

fn hints(a: &App) -> &'static str {
    match a.page {
        P_PROCESSES => "Enter details  x terminate  X tree  K kill  p/c stop/cont  s signal  n nice  a affinity  t tree  e exited  </> sort  o order  / filter  ? help",
        P_PERF => "↑↓ device  g graph mode  K kernel times  c combined  ? help",
        P_STARTUP => "Space enable/disable  r reload  / filter",
        P_USERS => "l lock  d disconnect  M manage accounts  r reload",
        P_SERVICES => "s start  x stop  R restart  e enable/disable  g process  r reload  / filter",
        P_SYSINFO => "↑↓ scroll  E export  r reload",
        P_RECORDER => "r record/stop  s save  d discard  Enter replay  o open file  (replay: Space play  ,/. step  x close)",
        P_CONN => "a all  l listening  p public  t IPC  g process  r reload  / filter",
        P_APPS => "p packages without desktop entries  r reload  / filter",
        P_DRIVERS => "Enter details  l loaded only  r reload  / filter",
        P_DISK => "Enter scan mount  c choose folder  Space pause  x stop  v view",
        P_BENCH => "a all  c cpu  d disk  i internet  f folder  h history  C clear  Esc cancel",
        P_SETTINGS => "Enter/Space change",
        P_APPHIST => "C clear  / filter",
        _ => "Tab/←→ pages  ? help  q quit",
    }
}

fn status(f: &mut Frame, area: Rect, a: &App) {
    let left = match &a.msg {
        Some((m, _)) => Span::styled(m.clone(), Style::new().fg(Color::Yellow)),
        None => Span::styled(hints(a), Style::new().fg(Color::DarkGray)),
    };
    let right = format!(" CPU {:.0}%  MEM {}  Up {} ", a.cpu.overall, bytes(a.mem.used as f64), uptime_str(uptime_secs()));
    let w = right.chars().count() as u16;
    let parts = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Min(1), Constraint::Length(w)]).split(area);
    f.render_widget(Paragraph::new(Line::from(left)), parts[0]);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), parts[1]);
}

// ───────────── generic table ─────────────

#[allow(clippy::too_many_arguments)]
pub fn table(f: &mut Frame, area: Rect, title: String, headers: &[&str], widths: &[Constraint], rows: Vec<Vec<String>>, sel: usize, off: &mut usize, a: &App) {
    let h = area.height.saturating_sub(3) as usize;
    if sel < *off {
        *off = sel;
    }
    if h > 0 && sel >= *off + h {
        *off = sel + 1 - h;
    }
    let total = rows.len();
    let body: Vec<Row> = rows
        .into_iter()
        .enumerate()
        .skip(*off)
        .take(h)
        .map(|(i, r)| {
            let st = if i == sel { Style::new().fg(Color::Black).bg(accent(a)) } else { Style::new() };
            Row::new(r).style(st)
        })
        .collect();
    let t = Table::new(body, widths.to_vec())
        .header(Row::new(headers.iter().map(|h| h.to_string()).collect::<Vec<_>>()).style(Style::new().add_modifier(Modifier::BOLD).fg(accent(a))))
        .block(block(format!("{title} ({total})"), a))
        .column_spacing(1);
    f.render_widget(t, area);
}

fn page_title(a: &App, page: usize, extra: &str) -> String {
    let mut t = PAGES[page].to_string();
    if !a.filter[page].is_empty() {
        t += &format!(" — filter “{}”", a.filter[page]);
    }
    if a.loading(page) {
        t += " — loading…";
    }
    if !extra.is_empty() {
        t += " — ";
        t += extra;
    }
    t
}

// ───────────── summary ─────────────

fn summary(f: &mut Frame, area: Rect, a: &App) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(7), Constraint::Length(7), Constraint::Min(5), Constraint::Length(3)]).split(area);
    let top = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(50), Constraint::Percentage(50)]).split(v[0]);
    spark(f, top[0], format!("CPU {:.0}% — {} ({} logical)", a.cpu.overall, a.cpu_static.model, a.cpu_static.logical), &a.hist.cpu, Some(100), level_color(a.cpu.overall), a);
    let mp = if a.mem.total > 0 { a.mem.used as f32 / a.mem.total as f32 * 100.0 } else { 0.0 };
    spark(f, top[1], format!("Memory {} / {} ({:.0}%)", bytes(a.mem.used as f64), bytes(a.mem.total as f64), mp), &a.hist.mem, Some(100), level_color(mp), a);
    let mid = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(34), Constraint::Percentage(33), Constraint::Percentage(33)]).split(v[1]);
    let dr: f64 = a.disks.iter().map(|d| d.read_rate).sum();
    let dw: f64 = a.disks.iter().map(|d| d.write_rate).sum();
    spark(f, mid[0], format!("Disk R {} W {}", rate(dr), rate(dw)), &a.hist.disk, Some(100), Color::Blue, a);
    let (rx, tx) = a.nets.iter().filter(|n| n.name != "lo").fold((0.0, 0.0), |s, n| (s.0 + n.rx_rate, s.1 + n.tx_rate));
    spark(f, mid[1], format!("Network ↓ {} ↑ {}", rate(rx), rate(tx)), &a.hist.rx, None, Color::Magenta, a);
    let gt = match a.gpu_now.first() {
        Some(g) => format!("GPU {:.0}% {}", g.util.unwrap_or(0.0), g.temp.map(|t| a.s.temp(t as f64)).unwrap_or_default()),
        None => "GPU (none detected)".into(),
    };
    spark(f, mid[2], gt, &a.hist.gpu, Some(100), Color::Green, a);
    // top processes + system pressure
    let bot = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Min(30), Constraint::Length(36)]).split(v[2]);
    let mut top5: Vec<&Proc> = a.procs.iter().collect();
    top5.sort_by(|x, y| y.cpu.partial_cmp(&x.cpu).unwrap_or(std::cmp::Ordering::Equal));
    let rows: Vec<Row> = top5
        .iter()
        .take(bot[0].height.saturating_sub(3) as usize)
        .map(|p| Row::new(vec![p.name.clone(), p.pid.to_string(), format!("{:.1}%", p.cpu), bytes(p.rss as f64)]))
        .collect();
    f.render_widget(
        Table::new(rows, [Constraint::Min(12), Constraint::Length(8), Constraint::Length(8), Constraint::Length(10)])
            .header(Row::new(["Top processes", "PID", "CPU", "Memory"]).style(Style::new().add_modifier(Modifier::BOLD)))
            .block(block("Top processes by CPU", a)),
        bot[0],
    );
    let pressure = a.cpu.overall.max((a.mem.psi_some10 * 5.0).min(100.0)).max(a.disks.iter().map(|d| d.active_pct).fold(0.0, f32::max));
    let lines = vec![
        Line::from(format!("System pressure {:>5.1}%", pressure)),
        Line::from(Span::styled(bar(pressure, 30), Style::new().fg(level_color(pressure)))),
        Line::from(""),
        line("Memory stall (PSI)", format!("{:.2}%", a.mem.psi_some10)),
        line("Swap in use", bytes(a.mem.swap_used as f64)),
        line("Kernel CPU", format!("{:.1}%", a.cpu.kernel)),
    ];
    f.render_widget(Paragraph::new(lines).block(block("Whole machine", a)), bot[1]);
    let (l1, l5, l15, tasks) = load_avg();
    let foot = format!(
        " Uptime {}   Load {l1:.2} {l5:.2} {l15:.2}   Tasks {tasks}   Processes {}   Threads {}   Services {}   Sessions {}",
        uptime_str(uptime_secs()),
        a.procs.len(),
        a.procs.iter().map(|p| p.threads as u64).sum::<u64>(),
        if a.services.is_empty() { "—".into() } else { format!("{} running", a.services.iter().filter(|s| s.sub == "running").count()) },
        a.sessions.len()
    );
    f.render_widget(Paragraph::new(foot).block(block("System", a)), v[3]);
}

// ───────────── processes ─────────────

fn processes(f: &mut Frame, area: Rect, a: &mut App) {
    a.fix_selection();
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(5), Constraint::Length(4)]).split(area);
    let view = a.proc_view();
    let cur = view.iter().position(|(_, i)| a.plist[*i].pid == a.proc_sel).unwrap_or(0);
    let mut rows = Vec::with_capacity(view.len());
    let parents: std::collections::HashSet<i32> = a.plist.iter().map(|p| p.ppid).collect();
    for (d, i) in &view {
        let p = &a.plist[*i];
        let mark = if a.s.process_tree && a.filter[P_PROCESSES].is_empty() {
            let m = if parents.contains(&p.pid) { if a.collapsed.contains(&p.pid) { "▸ " } else { "▾ " } } else { "  " };
            format!("{}{m}", "  ".repeat((*d).min(12)))
        } else {
            String::new()
        };
        let mem = if a.proc_values || a.mem.total == 0 { bytes(p.rss as f64) } else { format!("{:.1}%", p.rss as f64 / a.mem.total as f64 * 100.0) };
        rows.push(vec![
            format!("{mark}{}", p.name),
            p.pid.to_string(),
            format!("{:.1}%", p.cpu),
            mem,
            if p.read_rate > 0.0 { rate(p.read_rate) } else { "-".into() },
            if p.write_rate > 0.0 { rate(p.write_rate) } else { "-".into() },
            p.user.clone(),
            p.state_label().to_string(),
            p.threads.to_string(),
        ]);
    }
    let arrow = if a.s.sort_desc { "▼" } else { "▲" };
    let heads: Vec<String> = PROC_COLS.iter().enumerate().map(|(i, c)| if i == a.s.sort_col && !(a.s.process_tree && a.filter[P_PROCESSES].is_empty()) { format!("{c} {arrow}") } else { c.to_string() }).collect();
    let hs: Vec<&str> = heads.iter().map(|s| s.as_str()).collect();
    let extra = format!("{} total CPU {:.0}%{}", a.procs.len(), a.cpu.overall, if a.s.process_tree { " — tree" } else { "" });
    let title = page_title(a, P_PROCESSES, &extra);
    let mut off = a.offset[P_PROCESSES];
    table(
        f,
        v[0],
        title,
        &hs,
        &[Constraint::Min(18), Constraint::Length(8), Constraint::Length(8), Constraint::Length(10), Constraint::Length(11), Constraint::Length(11), Constraint::Length(12), Constraint::Length(9), Constraint::Length(7)],
        rows,
        cur,
        &mut off,
        a,
    );
    a.offset[P_PROCESSES] = off;
    let det = match a.selected_proc() {
        Some(p) => vec![
            Line::from(format!("PID {}  PPID {}  nice {}  {}  {}", p.pid, p.ppid, p.nice, p.user, p.state_label())),
            Line::from(format!("{}", if p.cmdline.is_empty() { format!("[{}]", p.name) } else { p.cmdline.clone() })),
        ],
        None => vec![Line::from("No process selected")],
    };
    f.render_widget(Paragraph::new(det).wrap(Wrap { trim: true }).block(block("Selection", a)), v[1]);
}

// ───────────── performance ─────────────

enum Dev {
    Cpu,
    Mem,
    Disk(Option<usize>),
    Net(Option<usize>),
    Gpu(Option<usize>),
}

fn dev_kinds(a: &App) -> Vec<Dev> {
    let mut v = vec![Dev::Cpu, Dev::Mem];
    if a.s.combined_disks {
        v.push(Dev::Disk(None));
    } else {
        v.extend((0..a.disks.len()).map(|i| Dev::Disk(Some(i))));
    }
    let nets: Vec<usize> = a.nets.iter().enumerate().filter(|(_, n)| n.name != "lo").map(|(i, _)| i).collect();
    if a.s.combined_network {
        v.push(Dev::Net(None));
    } else {
        v.extend(nets.into_iter().map(|i| Dev::Net(Some(i))));
    }
    if a.s.combined_gpus && a.gpu_now.len() > 1 {
        v.push(Dev::Gpu(None));
    } else {
        v.extend((0..a.gpu_now.len()).map(|i| Dev::Gpu(Some(i))));
    }
    v
}

fn performance(f: &mut Frame, area: Rect, a: &mut App) {
    let cols = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(24), Constraint::Min(20)]).split(area);
    let devs = a.perf_devices();
    a.perf_dev = a.perf_dev.min(devs.len().saturating_sub(1));
    let items: Vec<Line> = devs
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let (val, p) = match dev_kinds(a).get(i) {
                Some(Dev::Cpu) => (format!("{:.0}%", a.cpu.overall), a.cpu.overall),
                Some(Dev::Mem) => (format!("{:.0}%", if a.mem.total > 0 { a.mem.used as f32 / a.mem.total as f32 * 100.0 } else { 0.0 }), if a.mem.total > 0 { a.mem.used as f32 / a.mem.total as f32 * 100.0 } else { 0.0 }),
                Some(Dev::Disk(Some(d))) => (format!("{:.0}%", a.disks[*d].active_pct), a.disks[*d].active_pct),
                Some(Dev::Gpu(Some(g))) => (format!("{:.0}%", a.gpu_now[*g].util.unwrap_or(0.0)), a.gpu_now[*g].util.unwrap_or(0.0)),
                _ => (String::new(), 0.0),
            };
            let st = if i == a.perf_dev { Style::new().fg(Color::Black).bg(accent(a)) } else { Style::new().fg(level_color(p)) };
            Line::from(Span::styled(format!(" {n:<16}{val:>4}"), st))
        })
        .collect();
    f.render_widget(Paragraph::new(items).block(block("Performance", a)), cols[0]);
    let c = cols[1];
    let kinds = dev_kinds(a);
    match kinds.get(a.perf_dev) {
        Some(Dev::Cpu) | None => perf_cpu(f, c, a),
        Some(Dev::Mem) => perf_mem(f, c, a),
        Some(Dev::Disk(i)) => perf_disk(f, c, a, *i),
        Some(Dev::Net(i)) => perf_net(f, c, a, *i),
        Some(Dev::Gpu(i)) => perf_gpu(f, c, a, *i),
    }
}

fn perf_cpu(f: &mut Frame, c: Rect, a: &App) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(8), Constraint::Length(10)]).split(c);
    match a.cpu_graph {
        CpuGraph::Overall => {
            let mut t = format!("CPU — {}", a.cpu_static.model);
            if a.show_kernel {
                t += &format!(" (kernel {:.0}%)", a.cpu.kernel);
            }
            spark(f, v[0], t, &a.hist.cpu, Some(100), level_color(a.cpu.overall), a);
        }
        CpuGraph::Logical => {
            let n = a.cpu.per.len();
            let half = (n + 1) / 2;
            let mut l: Vec<Line> = Vec::new();
            let w = (v[0].width.saturating_sub(4) / 2).saturating_sub(18) as usize;
            for i in 0..half {
                let mut spans = Vec::new();
                for j in [i, i + half] {
                    if j < n {
                        let p = a.cpu.per[j];
                        let e = if a.cpu_static.efficiency.get(j).copied().unwrap_or(false) { "E" } else { "P" };
                        spans.push(Span::raw(format!("{j:>2}{e} ")));
                        spans.push(Span::styled(bar(p, w.max(4)), Style::new().fg(level_color(p))));
                        spans.push(Span::raw(format!(" {:>4.0}% {:>4.0}M  ", p, a.freqs.get(j).copied().unwrap_or(0.0))));
                    }
                }
                l.push(Line::from(spans));
            }
            f.render_widget(Paragraph::new(l).block(block("% Utilization by logical processor", a)), v[0]);
        }
        CpuGraph::Classes => {
            let (mut pc, mut pn, mut ec, mut en) = (0.0, 0, 0.0, 0);
            for (i, p) in a.cpu.per.iter().enumerate() {
                if a.cpu_static.efficiency.get(i).copied().unwrap_or(false) {
                    ec += p;
                    en += 1;
                } else {
                    pc += p;
                    pn += 1;
                }
            }
            let (pa, ea) = (if pn > 0 { pc / pn as f32 } else { 0.0 }, if en > 0 { ec / en as f32 } else { 0.0 });
            let l = vec![
                Line::from(format!("Performance cores ({pn})  {pa:>5.1}%")),
                Line::from(Span::styled(bar(pa, 50), Style::new().fg(level_color(pa)))),
                Line::from(""),
                Line::from(if en > 0 { format!("Efficiency cores ({en})  {ea:>5.1}%") } else { "Efficiency cores: none (this CPU is not hybrid)".to_string() }),
                Line::from(Span::styled(bar(ea, 50), Style::new().fg(level_color(ea)))),
            ];
            f.render_widget(Paragraph::new(l).block(block("% Utilization by processor class", a)), v[0]);
        }
    }
    let avg = if a.freqs.is_empty() { 0.0 } else { a.freqs.iter().sum::<f64>() / a.freqs.len() as f64 };
    let (l1, _, _, _) = load_avg();
    let st = &a.cpu_static;
    let lines = vec![
        line("Utilization", format!("{:.1}%   kernel {:.1}%", a.cpu.overall, a.cpu.kernel)),
        line("Speed (avg / base)", format!("{avg:.0} MHz / {}", st.base_mhz.map(|m| format!("{m:.0} MHz")).unwrap_or_else(|| "n/a".into()))),
        line("Processes / threads", format!("{} / {}", a.procs.len(), a.procs.iter().map(|p| p.threads as u64).sum::<u64>())),
        line("Up time", uptime_str(uptime_secs())),
        line("Cores / logical / sockets", format!("{} / {} / {}", st.physical, st.logical, st.sockets)),
        line("Caches", st.caches.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join("  ")),
        line("Governor / driver", format!("{} / {}", st.governor, st.driver)),
        line("Virtualization", st.virt.clone()),
        line("Load (1 min)", format!("{l1:.2}")),
    ];
    f.render_widget(Paragraph::new(lines).block(block("CPU details  (g: graph mode, K: kernel times)", a)), v[1]);
}

fn perf_mem(f: &mut Frame, c: Rect, a: &App) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(6), Constraint::Length(5), Constraint::Length(12)]).split(c);
    let m = &a.mem;
    spark(f, v[0], format!("Memory in use — {} of {}", bytes(m.used as f64), bytes(m.total as f64)), &a.hist.mem, Some(100), Color::Yellow, a);
    let tot = m.total.max(1) as f64;
    let pct = |x: u64| (x as f64 / tot * 100.0) as f32;
    let comp = vec![
        Line::from(vec![Span::styled(bar(pct(m.used.saturating_sub(m.shared)), 40), Style::new().fg(Color::Yellow)), Span::raw(format!(" used {}", bytes(m.used as f64)))]),
        Line::from(vec![Span::styled(bar(pct(m.cached + m.buffers), 40), Style::new().fg(Color::Blue)), Span::raw(format!(" cached+buffers {}", bytes((m.cached + m.buffers) as f64)))]),
        Line::from(vec![Span::styled(bar(pct(m.available), 40), Style::new().fg(Color::Green)), Span::raw(format!(" available {}", bytes(m.available as f64)))]),
    ];
    f.render_widget(Paragraph::new(comp).block(block("Composition", a)), v[1]);
    let lines = vec![
        line("Physical memory", bytes(m.total as f64)),
        line("In use", bytes(m.used as f64)),
        line("Cached", bytes(m.cached as f64)),
        line("Buffers", bytes(m.buffers as f64)),
        line("Shared", bytes(m.shared as f64)),
        line("Kernel slab", bytes(m.slab as f64)),
        line("Swap in use / total", format!("{} / {}", bytes(m.swap_used as f64), bytes(m.swap_total as f64))),
        line("Committed / limit", format!("{} / {}", bytes(m.committed as f64), bytes(m.commit_limit as f64))),
        line("Memory pressure (PSI some 10s)", format!("{:.2}%", m.psi_some10)),
        line("Page ins / outs (since boot)", format!("{} / {}", bytes(m.page_in_kb as f64 * 1024.0), bytes(m.page_out_kb as f64 * 1024.0))),
    ];
    f.render_widget(Paragraph::new(lines).block(block("Memory details", a)), v[2]);
}

fn perf_disk(f: &mut Frame, c: Rect, a: &App, i: Option<usize>) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(5), Constraint::Min(4), Constraint::Length(12)]).split(c);
    let devs: Vec<&DiskStat> = match i {
        Some(i) => a.disks.get(i).into_iter().collect(),
        None => a.disks.iter().collect(),
    };
    let key = |p: &str| match i {
        Some(i) => a.disks.get(i).map(|d| format!("{p}:{}", d.name)).unwrap_or_default(),
        None => String::new(),
    };
    let empty = VecDeque::new();
    spark(f, v[0], "Active time %".into(), if i.is_some() { a.dh.get(&key("disk")).unwrap_or(&empty) } else { &a.hist.disk }, Some(100), Color::Blue, a);
    let (rr, ww): (f64, f64) = (devs.iter().map(|d| d.read_rate).sum(), devs.iter().map(|d| d.write_rate).sum());
    let hh = if i.is_some() { a.dh.get(&key("dr")).unwrap_or(&empty).clone() } else { VecDeque::new() };
    if i.is_some() {
        spark(f, v[1], format!("Read {}   Write {}", rate(rr), rate(ww)), &hh, None, Color::Green, a);
    } else {
        f.render_widget(Paragraph::new(format!("Read {}   Write {}", rate(rr), rate(ww))).block(block("Throughput", a)), v[1]);
    }
    let mut lines = Vec::new();
    for d in devs {
        lines.push(line(&format!("{} {}", d.name, d.model.trim()), format!("{}{}", bytes(d.size as f64), if d.system { "  [system disk]" } else { "" })));
        lines.push(line("  Type / scheduler", format!("{} / {}", if d.rotational { "Rotational" } else { "Solid state" }, d.scheduler)));
        lines.push(line("  Active time / response", format!("{:.0}% / {:.1} ms", d.active_pct, d.resp_ms)));
        lines.push(line("  Read rate / total", format!("{} / {}", rate(d.read_rate), bytes(d.total_read as f64))));
        lines.push(line("  Write rate / total", format!("{} / {}", rate(d.write_rate), bytes(d.total_written as f64))));
    }
    f.render_widget(Paragraph::new(lines).block(block("Disk details", a)), v[2]);
}

fn perf_net(f: &mut Frame, c: Rect, a: &App, i: Option<usize>) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Percentage(35), Constraint::Percentage(35), Constraint::Min(8)]).split(c);
    let empty = VecDeque::new();
    let sel: Vec<&NetStat> = match i {
        Some(i) => a.nets.get(i).into_iter().collect(),
        None => a.nets.iter().filter(|n| n.name != "lo").collect(),
    };
    let (rx, tx): (f64, f64) = (sel.iter().map(|n| n.rx_rate).sum(), sel.iter().map(|n| n.tx_rate).sum());
    let (hr, ht) = match i.and_then(|i| a.nets.get(i)) {
        Some(n) => (a.dh.get(&format!("rx:{}", n.name)).unwrap_or(&empty), a.dh.get(&format!("tx:{}", n.name)).unwrap_or(&empty)),
        None => (&a.hist.rx, &a.hist.tx),
    };
    spark(f, v[0], format!("Receive {}", rate(rx)), hr, None, Color::Magenta, a);
    spark(f, v[1], format!("Send {}", rate(tx)), ht, None, Color::LightBlue, a);
    let mut l = Vec::new();
    for n in sel {
        l.push(line(&format!("{}{}", n.name, if n.up { "" } else { " (down)" }), if n.wireless { "Wi-Fi".into() } else { "Ethernet / other".into() }));
        l.push(line("  Hardware address", n.mac.clone()));
        l.push(line("  IPv4 address", n.ipv4.join(", ")));
        l.push(line("  IPv6 address", n.ipv6.join(", ")));
        l.push(line("  Link speed", n.link_mbps.map(|m| format!("{m} Mb/s")).unwrap_or_else(|| "n/a".into())));
        l.push(line("  Total received / sent", format!("{} / {}", bytes(n.rx_total as f64), bytes(n.tx_total as f64))));
    }
    f.render_widget(Paragraph::new(l).block(block("Network details", a)), v[2]);
}

fn perf_gpu(f: &mut Frame, c: Rect, a: &App, i: Option<usize>) {
    if a.gpu_now.is_empty() {
        f.render_widget(Paragraph::new("No GPU telemetry available (nvidia-smi / amdgpu sysfs not found).").block(block("GPU", a)), c);
        return;
    }
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Percentage(40), Constraint::Percentage(25), Constraint::Min(8)]).split(c);
    let idx = i.unwrap_or(0).min(a.gpu_now.len() - 1);
    let g = &a.gpu_now[idx];
    let empty = VecDeque::new();
    spark(f, v[0], format!("{} — utilization {:.0}%", g.name, g.util.unwrap_or(0.0)), a.dh.get(&format!("gpu:{idx}")).unwrap_or(&empty), Some(100), Color::Green, a);
    spark(f, v[1], "GPU memory in use %".into(), a.dh.get(&format!("gmem:{idx}")).unwrap_or(&empty), Some(100), Color::Yellow, a);
    let fm = |x: Option<f32>, u: &str| x.map(|v| format!("{v:.0} {u}")).unwrap_or_else(|| "n/a".into());
    let mut l = vec![
        line("Driver / PCI", format!("{} / {}", g.driver, g.pci)),
        line("Memory", format!("{} / {}", g.mem_used.map(|x| bytes(x as f64)).unwrap_or("n/a".into()), g.mem_total.map(|x| bytes(x as f64)).unwrap_or("n/a".into()))),
        line("Graphics clock / max", format!("{} / {}", fm(g.clock_mhz, "MHz"), fm(g.clock_max_mhz, "MHz"))),
        line("Memory clock / max", format!("{} / {}", fm(g.mem_clock_mhz, "MHz"), fm(g.mem_clock_max_mhz, "MHz"))),
        line("Power draw / limit", format!("{} / {}", fm(g.power, "W"), fm(g.power_limit, "W"))),
        line("Temperature", g.temp.map(|t| a.s.temp(t as f64)).unwrap_or_else(|| "n/a".into())),
        line("Video encode / decode", format!("{} / {}", fm(g.enc, "%"), fm(g.dec, "%"))),
    ];
    if a.gpu_now.len() > 1 && i.is_none() {
        for (k, o) in a.gpu_now.iter().enumerate() {
            l.push(line(&format!("GPU {k}"), format!("{} — {:.0}%", o.name, o.util.unwrap_or(0.0))));
        }
    }
    f.render_widget(Paragraph::new(l).block(block("GPU details", a)), v[2]);
}

// ───────────── simple table pages ─────────────

fn app_history(f: &mut Frame, area: Rect, a: &mut App) {
    let rows: Vec<Vec<String>> = a.app_hist_view().iter().map(|(n, (cpu, r, w))| vec![n.to_string(), format!("{:.1} s", cpu), bytes(*r as f64), bytes(*w as f64)]).collect();
    let t = page_title(a, P_APPHIST, "cumulative since first run (persisted)");
    let mut off = a.offset[P_APPHIST];
    table(f, area, t, &["Name", "CPU time", "Disk read", "Disk write"], &[Constraint::Min(20), Constraint::Length(14), Constraint::Length(12), Constraint::Length(12)], rows, a.sel[P_APPHIST], &mut off, a);
    a.offset[P_APPHIST] = off;
}

fn startup(f: &mut Frame, area: Rect, a: &mut App) {
    let rows: Vec<Vec<String>> = a.startup_view().iter().map(|s| vec![s.name.clone(), if s.enabled { "Enabled".into() } else { "Disabled".into() }, s.origin.into(), s.command.clone()]).collect();
    let t = page_title(a, P_STARTUP, "XDG autostart entries");
    let mut off = a.offset[P_STARTUP];
    table(f, area, t, &["Name", "Status", "Origin", "Command"], &[Constraint::Min(22), Constraint::Length(9), Constraint::Length(8), Constraint::Min(30)], rows, a.sel[P_STARTUP], &mut off, a);
    a.offset[P_STARTUP] = off;
}

fn users(f: &mut Frame, area: Rect, a: &mut App) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(5), Constraint::Length(8)]).split(area);
    let uv = a.users_view();
    let rows: Vec<Vec<String>> = uv.iter().map(|u| vec![u.1.clone(), u.0.to_string(), u.2.len().to_string(), u.3.to_string(), format!("{:.1}%", u.4), bytes(u.5 as f64)]).collect();
    let det: Vec<Line> = match uv.get(a.sel[P_USERS]) {
        Some(u) if !u.2.is_empty() => u.2.iter().map(|s| Line::from(format!("Session {}  seat {}  tty {}", s.id, s.seat, s.tty))).collect(),
        Some(_) => vec![Line::from("No login session (service account).")],
        None => vec![],
    };
    drop(uv);
    let t = page_title(a, P_USERS, "");
    let mut off = a.offset[P_USERS];
    table(f, v[0], t, &["User", "UID", "Sessions", "Processes", "CPU", "Memory"], &[Constraint::Min(16), Constraint::Length(7), Constraint::Length(9), Constraint::Length(10), Constraint::Length(8), Constraint::Length(10)], rows, a.sel[P_USERS], &mut off, a);
    a.offset[P_USERS] = off;
    f.render_widget(Paragraph::new(det).block(block("Sessions of the selected user", a)), v[1]);
}

fn services(f: &mut Frame, area: Rect, a: &mut App) {
    let sv = a.service_view();
    let rows: Vec<Vec<String>> = sv.iter().map(|s| vec![s.unit.clone(), s.active.clone(), s.sub.clone(), s.enabled.clone(), s.pid.map(|p| p.to_string()).unwrap_or_default(), s.description.clone()]).collect();
    drop(sv);
    let t = page_title(a, P_SERVICES, "systemd");
    let mut off = a.offset[P_SERVICES];
    table(f, area, t, &["Service", "Active", "Sub", "Enabled", "PID", "Description"], &[Constraint::Min(26), Constraint::Length(9), Constraint::Length(10), Constraint::Length(9), Constraint::Length(8), Constraint::Min(20)], rows, a.sel[P_SERVICES], &mut off, a);
    a.offset[P_SERVICES] = off;
}

fn power(f: &mut Frame, area: Rect, a: &mut App) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(6), Constraint::Length(if a.restricted.is_empty() { 3 } else { 3 + a.restricted.len() as u16 })]).split(area);
    let rows: Vec<Vec<String>> = a
        .power_view()
        .iter()
        .map(|s| {
            let val = match s.unit {
                "°C" => a.s.temp(s.value),
                "MHz" | "RPM" => format!("{:.0} {}", s.value, s.unit),
                "%" => format!("{:.0} %", s.value),
                _ => format!("{:.2} {}", s.value, s.unit),
            };
            vec![s.group.to_string(), s.device.clone(), s.label.clone(), val]
        })
        .collect();
    let t = page_title(a, P_POWER, "power and frequency sensors");
    let mut off = a.offset[P_POWER];
    table(f, v[0], t, &["Group", "Device", "Sensor", "Value"], &[Constraint::Length(22), Constraint::Min(18), Constraint::Min(22), Constraint::Length(14)], rows, a.sel[P_POWER], &mut off, a);
    a.offset[P_POWER] = off;
    let mut l: Vec<Line> = Vec::new();
    if a.restricted.is_empty() {
        l.push(Line::from("All sensor sources are readable."));
    } else {
        for r in &a.restricted {
            l.push(Line::from(Span::styled(format!("Access needed: {r}"), Style::new().fg(Color::Yellow))));
        }
    }
    f.render_widget(Paragraph::new(l).block(block("Source access", a)), v[1]);
}

fn connections(f: &mut Frame, area: Rect, a: &mut App) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(6), Constraint::Length(7)]).split(area);
    let cv = a.scope_conns();
    let rows: Vec<Vec<String>> = cv
        .iter()
        .map(|c| {
            let owner = if c.owners.is_empty() { "Kernel / unavailable".to_string() } else { let mut s = c.owners.iter().take(2).map(|o| format!("{} (PID {})", o.1, o.0)).collect::<Vec<_>>().join(", "); if c.owners.len() > 2 { s += &format!(" +{} more", c.owners.len() - 2); } s };
            vec![owner, c.proto.to_string(), c.state.clone(), c.local.clone(), if c.service.is_empty() { c.remote.clone() } else { format!("{} {}", c.remote, c.service) }, c.exposure.to_string()]
        })
        .collect();
    let det: Vec<Line> = match cv.get(a.sel[P_CONN]) {
        Some(c) => vec![
            Line::from(format!("Owners: {}", if c.owners.is_empty() { "unavailable (kernel or another user)".into() } else { c.owners.iter().map(|o| format!("{} (PID {})", o.1, o.0)).collect::<Vec<_>>().join(", ") })),
            Line::from(format!("UID: {}   Socket inode: {}", c.uid, c.inode)),
            Line::from(format!("Local: {}", c.local)),
            Line::from(format!("Remote: {}   Exposure: {}", c.remote, c.exposure)),
        ],
        None => vec![Line::from("Select a row to inspect its route and owner")],
    };
    let warn = a.conns.as_ref().map(|c| c.warnings.join(" ")).unwrap_or_default();
    drop(cv);
    let scope = ["all connections", "listening ports", "public listeners"][a.conn_scope];
    let t = page_title(a, P_CONN, &format!("{scope}{}", if a.show_ipc { " + local IPC" } else { "" }));
    let mut off = a.offset[P_CONN];
    table(f, v[0], t, &["Application", "Protocol", "State", "Local endpoint", "Remote / service", "Exposure"], &[Constraint::Min(22), Constraint::Length(13), Constraint::Length(12), Constraint::Min(22), Constraint::Min(22), Constraint::Length(15)], rows, a.sel[P_CONN], &mut off, a);
    a.offset[P_CONN] = off;
    let mut det = det;
    if !warn.is_empty() {
        det.push(Line::from(Span::styled(warn, Style::new().fg(Color::Yellow))));
    }
    f.render_widget(Paragraph::new(det).wrap(Wrap { trim: true }).block(block("Selected connection route", a)), v[1]);
}

fn installed(f: &mut Frame, area: Rect, a: &mut App) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Min(6), Constraint::Length(5)]).split(area);
    let av = a.app_view();
    let rows: Vec<Vec<String>> = av.iter().map(|x| vec![x.name.clone(), x.version.clone(), x.publisher.clone(), x.scope.into(), x.source.into()]).collect();
    let det: Vec<Line> = match av.get(a.sel[P_APPS]) {
        Some(x) => vec![Line::from(format!("Command: {}", x.command)), Line::from(format!("File: {}", x.file)), Line::from(format!("Installation scope: {}", x.scope))],
        None => vec![],
    };
    drop(av);
    let t = page_title(a, P_APPS, if a.show_pkgs { "with packages without desktop entries" } else { "desktop applications" });
    let mut off = a.offset[P_APPS];
    table(f, v[0], t, &["Name", "Version", "Publisher", "Scope", "Source"], &[Constraint::Min(24), Constraint::Length(18), Constraint::Min(18), Constraint::Length(17), Constraint::Length(14)], rows, a.sel[P_APPS], &mut off, a);
    a.offset[P_APPS] = off;
    f.render_widget(Paragraph::new(det).block(block("Application details", a)), v[1]);
}

fn drivers(f: &mut Frame, area: Rect, a: &mut App) {
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(4), Constraint::Min(6)]).split(area);
    let (head, notes) = match &a.mods {
        Some(m) => {
            let loaded: Vec<_> = m.modules.iter().filter(|x| x.loaded).collect();
            let size: u64 = loaded.iter().map(|x| x.size).sum();
            (format!("Kernel modules for Linux {} — {} loaded, {} known. Module memory {} · slab {}", m.kernel_release, loaded.len(), m.modules.len(), bytes(size as f64), bytes(m.slab as f64)), m.notes.join(" "))
        }
        None => ("Reading kernel modules…".into(), String::new()),
    };
    f.render_widget(Paragraph::new(vec![Line::from(head), Line::from(Span::styled(notes, Style::new().fg(Color::Yellow)))]).block(block("Kernel memory", a)), v[0]);
    let mv = a.module_view();
    let rows: Vec<Vec<String>> = mv.iter().map(|m| vec![m.name.clone(), if m.loaded { format!("{} ({})", m.refs, m.used_by.iter().take(3).cloned().collect::<Vec<_>>().join(",")) } else { String::new() }, if m.loaded { bytes(m.size as f64) } else { String::new() }, m.state.clone(), m.source.clone()]).collect();
    drop(mv);
    let t = page_title(a, P_DRIVERS, if a.mods_loaded_only { "loaded only" } else { "all" });
    let mut off = a.offset[P_DRIVERS];
    table(f, v[1], t, &["Module", "Used by", "Size", "State", "Source"], &[Constraint::Min(22), Constraint::Min(22), Constraint::Length(10), Constraint::Length(11), Constraint::Length(12)], rows, a.sel[P_DRIVERS], &mut off, a);
    a.offset[P_DRIVERS] = off;
}

// ───────────── system info ─────────────

fn sysinfo(f: &mut Frame, area: Rect, a: &mut App) {
    let mut l: Vec<Line> = Vec::new();
    if a.sysinfo.is_empty() {
        l.push(Line::from("Collecting system information…"));
    }
    for (title, rows) in &a.sysinfo {
        l.push(Line::from(Span::styled(title.clone(), Style::new().fg(accent(a)).add_modifier(Modifier::BOLD))));
        for (k, v) in rows {
            l.push(Line::from(vec![Span::styled(format!("  {k:<30}"), Style::new().fg(Color::DarkGray)), Span::raw(v.clone())]));
        }
        l.push(Line::from(""));
    }
    l.push(Line::from(Span::styled("Capabilities", Style::new().fg(accent(a)).add_modifier(Modifier::BOLD))));
    for (k, v) in capabilities(a) {
        l.push(Line::from(vec![Span::styled(format!("  {k:<30}"), Style::new().fg(Color::DarkGray)), Span::raw(v)]));
    }
    let max = l.len().saturating_sub(area.height.saturating_sub(2) as usize) as u16;
    a.sysinfo_scroll = a.sysinfo_scroll.min(max);
    f.render_widget(Paragraph::new(l).scroll((a.sysinfo_scroll, 0)).block(block(page_title(a, P_SYSINFO, ""), a)), area);
}

pub fn capabilities(a: &App) -> Vec<(String, String)> {
    let yes = |b: bool| if b { "Supported".to_string() } else { "Unsupported".to_string() };
    let root = unsafe { libc::geteuid() } == 0;
    vec![
        ("Running as root".into(), if root { "Yes".into() } else { "No — some per-process, RAPL and socket details need root".into() }),
        ("Process telemetry (/proc)".into(), yes(!a.procs.is_empty())),
        ("GPU telemetry".into(), if a.gpu_now.is_empty() { "Unsupported (no nvidia-smi / amdgpu sysfs data)".into() } else { format!("Supported ({} device(s))", a.gpu_now.len()) }),
        ("Hardware sensors (hwmon)".into(), yes(a.sensors.iter().any(|s| s.group == "Temperatures"))),
        ("Package power (RAPL)".into(), if a.restricted.is_empty() { yes(a.sensors.iter().any(|s| s.label.contains("package"))) } else { "Access needed (energy_uj is root-only)".into() }),
        ("systemd services".into(), yes(which("systemctl"))),
        ("Login sessions (loginctl)".into(), yes(which("loginctl"))),
        ("Kernel module inventory".into(), yes(std::path::Path::new("/proc/modules").exists())),
        ("Pressure stall info (PSI)".into(), yes(std::path::Path::new("/proc/pressure/memory").exists())),
        ("Sample interval".into(), format!("{} ms", a.s.refresh_ms)),
        ("Uptime of this program".into(), uptime_str(a.started.elapsed().as_secs())),
    ]
}

// ───────────── recorder ─────────────

fn recorder(f: &mut Frame, area: Rect, a: &mut App) {
    if let Some(r) = &a.replay {
        let s = &r.trace.samples[r.pos];
        let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(4), Constraint::Min(6), Constraint::Length(7)]).split(area);
        let total_ms = r.trace.samples.last().map(|s| s.t_ms).unwrap_or(0);
        f.render_widget(
            Gauge::default().block(block(format!("REPLAY {} — {} samples — {:.1}s of {:.1}s ({})", r.trace.host, r.trace.samples.len(), s.t_ms as f64 / 1000.0, total_ms as f64 / 1000.0, if r.playing { "playing" } else { "paused" }), a)).ratio(r.pos as f64 / (r.trace.samples.len().max(2) - 1) as f64).gauge_style(Style::new().fg(accent(a))),
            v[0],
        );
        let hist: Vec<u64> = r.trace.samples[..=r.pos].iter().map(|s| s.cpu_overall.round() as u64).collect();
        let take = v[1].width.saturating_sub(2) as usize;
        let hist = if hist.len() > take { hist[hist.len() - take..].to_vec() } else { hist };
        f.render_widget(Sparkline::default().block(block(format!("CPU {:.0}%", s.cpu_overall), a)).data(&hist).max(100).style(Style::new().fg(Color::Green)), v[1]);
        let mut l = vec![
            line("Memory in use", bytes(s.mem_used as f64)),
            line("Network ↓ / ↑", format!("{} / {}", rate(s.net_rx), rate(s.net_tx))),
            line("Disk read / write", format!("{} / {}", rate(s.disk_read), rate(s.disk_write))),
            line("Package power", s.power_w.map(|p| format!("{p:.1} W")).unwrap_or_else(|| "n/a".into())),
        ];
        if let Some((i, t)) = s.temps.iter().enumerate().max_by(|x, y| x.1.partial_cmp(y.1).unwrap_or(std::cmp::Ordering::Equal)) {
            l.push(line("Hottest sensor", format!("{} {}", a.s.temp(*t as f64), r.trace.temp_names.get(i).cloned().unwrap_or_default())));
        }
        l.push(line("GPU utilization", s.gpu.iter().map(|g| format!("{g:.0}%")).collect::<Vec<_>>().join(" ")));
        f.render_widget(Paragraph::new(l).block(block("At the replay position   (Space play/pause, ,/. step, PgUp/PgDn ±30, x close)", a)), v[2]);
        return;
    }
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(6), Constraint::Min(5)]).split(area);
    let mut l = Vec::new();
    match &a.rec {
        Some(r) => {
            let state = if r.active { Span::styled("● RECORDING", Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)) } else { Span::styled("UNSAVED", Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)) };
            l.push(Line::from(vec![state, Span::raw(format!("   {} samples   {}s", r.trace.samples.len(), r.elapsed_secs()))]));
            if r.limit_reached {
                l.push(Line::from("Safe capture limit reached; stop to save the samples collected."));
            }
            l.push(Line::from("Press r to stop, s to save, d to discard."));
        }
        None => {
            l.push(Line::from("Not recording. Press r to record detailed system telemetry (CPU per logical processor, memory, sensors, GPU, network, disk)."));
            l.push(Line::from("Saved traces are replayed with Enter."));
        }
    }
    f.render_widget(Paragraph::new(l).wrap(Wrap { trim: true }).block(block("Flight Recorder", a)), v[0]);
    let rows: Vec<Vec<String>> = a
        .traces
        .iter()
        .map(|p| vec![p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), std::fs::metadata(p).map(|m| bytes(m.len() as f64)).unwrap_or_default(), p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()])
        .collect();
    let mut off = a.offset[P_RECORDER];
    table(f, v[1], "Saved traces (*.rustytmtrace)".into(), &["File", "Size", "Folder"], &[Constraint::Min(26), Constraint::Length(10), Constraint::Min(20)], rows, a.sel[P_RECORDER], &mut off, a);
    a.offset[P_RECORDER] = off;
}

// ───────────── disk space ─────────────

fn disk_space(f: &mut Frame, area: Rect, a: &mut App) {
    let mh = (a.mounts.len() as u16 + 3).min(10);
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(mh), Constraint::Length(4), Constraint::Min(5)]).split(area);
    let rows: Vec<Vec<String>> = a
        .mounts
        .iter()
        .map(|m| {
            let used = m.total.saturating_sub(m.free);
            let p = if m.total > 0 { used as f32 / m.total as f32 * 100.0 } else { 0.0 };
            vec![m.mount.clone(), m.fstype.clone(), format!("{} of {} used, {} free", bytes(used as f64), bytes(m.total as f64), bytes(m.free as f64)), bar(p, 20)]
        })
        .collect();
    let n = a.mounts.len();
    let mut off = a.offset[P_DISK];
    table(f, v[0], format!("All Disks — {n} mounted filesystems"), &["Mount", "Type", "Usage", ""], &[Constraint::Min(18), Constraint::Length(8), Constraint::Min(36), Constraint::Length(22)], rows, a.sel[P_DISK], &mut off, a);
    a.offset[P_DISK] = off;
    let s = a.scanner.snapshot();
    let status = if s.running && s.paused {
        format!("Paused {} — {} across {} files so far", s.root.display(), bytes(s.bytes as f64), s.files)
    } else if s.running {
        format!("Scanning {} — {} across {} files so far   {}", s.root.display(), bytes(s.bytes as f64), s.files, s.current)
    } else if s.stopped {
        format!("Stopped {} — {} across {} files measured", s.root.display(), bytes(s.bytes as f64), s.files)
    } else if s.done {
        format!("{} — {} across {} files", s.root.display(), bytes(s.bytes as f64), s.files)
    } else {
        "Press Enter to measure the selected disk, or c to choose a folder.".to_string()
    };
    let mut hl = vec![Line::from(status)];
    if s.unreadable > 0 {
        hl.push(Line::from(Span::styled(format!("{} item(s) could not be measured", s.unreadable), Style::new().fg(Color::Yellow))));
    }
    f.render_widget(Paragraph::new(hl).block(block("Scan", a)), v[1]);
    let cats: Vec<Line> = CATEGORIES.iter().enumerate().map(|(i, c)| {
        let p = if s.bytes > 0 { s.by_cat[i] as f32 / s.bytes as f32 * 100.0 } else { 0.0 };
        Line::from(format!("{c:<13} {} {:>5.1}%  {}", bar(p, 24), p, bytes(s.by_cat[i] as f64)))
    }).collect();
    let cols = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Length(56), Constraint::Min(20)]).split(v[2]);
    f.render_widget(Paragraph::new(cats).block(block("By category", a)), cols[0]);
    let (title, rows): (&str, Vec<Vec<String>>) = match a.disk_view {
        0 => {
            let mut e: Vec<_> = s.by_ext.iter().collect();
            e.sort_by(|x, y| y.1 .1.cmp(&x.1 .1));
            ("Largest Types (v: switch)", e.into_iter().take(50).map(|(k, (n, sz))| vec![if k.is_empty() { "(no extension)".into() } else { format!(".{k}") }, format!("{n} files"), bytes(*sz as f64)]).collect())
        }
        1 => ("Largest Folders (v: switch)", s.top_dirs.iter().take(50).map(|(sz, p)| vec![p.to_string_lossy().into_owned(), String::new(), bytes(*sz as f64)]).collect()),
        _ => ("Largest Files (v: switch)", s.top_files.iter().take(50).map(|(sz, p)| vec![p.to_string_lossy().into_owned(), String::new(), bytes(*sz as f64)]).collect()),
    };
    let mut off2 = 0;
    table(f, cols[1], title.into(), &["Name", "", "Size"], &[Constraint::Min(20), Constraint::Length(12), Constraint::Length(10)], rows, usize::MAX / 2, &mut off2, a);
}

// ───────────── benchmarks ─────────────

fn fmt_res(v: Option<f64>, unit: &str) -> String {
    v.map(|x| format!("{x:.1} {unit}")).unwrap_or_else(|| "—".into())
}

fn benchmarks(f: &mut Frame, area: Rect, a: &mut App) {
    let s = a.bench.snapshot();
    let v = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(3), Constraint::Length(12), Constraint::Min(4)]).split(area);
    f.render_widget(
        Gauge::default().block(block(format!("Benchmark progress — {}", if s.label.is_empty() { "Ready" } else { &s.label }), a)).ratio(s.progress.clamp(0.0, 1.0) as f64).gauge_style(Style::new().fg(accent(a))),
        v[0],
    );
    let r: &BenchResult = if s.finished || s.cancelled { &s.result } else { a.bench_hist.last().unwrap_or(&s.result) };
    let mut l = vec![
        Line::from(Span::styled(format!("Score {}", r.score.map(|x| format!("{x:.0}")).unwrap_or_else(|| "—".into())), Style::new().add_modifier(Modifier::BOLD).fg(accent(a)))),
        line("CPU single core", fmt_res(r.cpu_single_mops, "Mops/sec")),
        line("CPU all cores", fmt_res(r.cpu_multi_gops, "Gop/sec")),
        line("Disk read", fmt_res(r.disk_read_mbs, "MB/sec")),
        line("Disk write", fmt_res(r.disk_write_mbs, "MB/sec")),
        line("Internet download", fmt_res(r.net_down_mbps, "Mb/s")),
        line("Internet upload", fmt_res(r.net_up_mbps, "Mb/s")),
        line("GPU graphics / compute", "not available in this build".into()),
        line("Test folder", a.bench_dir.to_string_lossy().into_owned()),
    ];
    for e in &s.errors {
        l.push(Line::from(Span::styled(e.clone(), Style::new().fg(Color::Red))));
    }
    l.push(Line::from(Span::styled("Score is baselined to 100 for this program's own reference figures; compare only between runs of RustyTM.", Style::new().fg(Color::DarkGray))));
    f.render_widget(Paragraph::new(l).block(block("Results", a)), v[1]);
    let rows: Vec<Vec<String>> = a
        .bench_hist
        .iter()
        .rev()
        .map(|h| vec![fmt_time(h.at), h.score.map(|x| format!("{x:.0}")).unwrap_or_default(), fmt_res(h.cpu_single_mops, "M"), fmt_res(h.cpu_multi_gops, "G"), fmt_res(h.disk_read_mbs, "MB"), fmt_res(h.disk_write_mbs, "MB"), fmt_res(h.net_down_mbps, "Mb")])
        .collect();
    let mut off = 0;
    table(f, v[2], "Benchmark history (h)".into(), &["When", "Score", "1 core", "All cores", "Disk read", "Disk write", "Download"], &[Constraint::Length(17), Constraint::Length(6), Constraint::Length(14), Constraint::Length(12), Constraint::Length(14), Constraint::Length(14), Constraint::Length(14)], rows, usize::MAX / 2, &mut off, a);
}

pub fn fmt_time(secs: u64) -> String {
    unsafe {
        let t = secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
    }
}

// ───────────── settings ─────────────

fn settings(f: &mut Frame, area: Rect, a: &mut App) {
    let s = &a.s;
    let vals: Vec<String> = vec![
        if s.fahrenheit { "Fahrenheit".into() } else { "Celsius".into() },
        onoff(s.show_exited),
        onoff(s.smooth_cpu),
        onoff(s.process_tree),
        onoff(s.per_core_cpu),
        onoff(s.combined_gpus),
        onoff(s.combined_disks),
        onoff(s.combined_network),
        onoff(s.status_bar),
        onoff(s.sidebar),
        format!("{} ms", s.refresh_ms),
        onoff(s.light_theme),
        onoff(s.confirm_stop_scan),
        String::new(),
    ];
    let rows: Vec<Vec<String>> = SETTINGS_ITEMS.iter().zip(vals).map(|(n, v)| vec![n.to_string(), v]).collect();
    let mut off = a.offset[P_SETTINGS];
    let hint = format!("settings file: {}", crate::settings::path().display());
    table(f, area, format!("Settings — {hint}"), &["Setting", "Value"], &[Constraint::Min(40), Constraint::Length(16)], rows, a.sel[P_SETTINGS], &mut off, a);
    a.offset[P_SETTINGS] = off;
}

fn onoff(b: bool) -> String {
    if b { "On".into() } else { "Off".into() }
}

// ───────────── overlays ─────────────

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn overlays(f: &mut Frame, area: Rect, a: &App) {
    if let Some(o) = &a.overlay {
        let r = centered(area, area.width.saturating_sub(6), area.height.saturating_sub(4));
        f.render_widget(Clear, r);
        f.render_widget(Paragraph::new(o.text.clone()).scroll((o.scroll, 0)).block(block(format!("{} — Esc to close", o.title), a)), r);
    }
    if let Some(p) = &a.prompt {
        let r = Rect { x: area.x, y: area.y + area.height.saturating_sub(3), width: area.width, height: 3.min(area.height) };
        f.render_widget(Clear, r);
        f.render_widget(Paragraph::new(format!("{}▏", p.buf)).block(block(format!("{} — Enter to accept, Esc to cancel", p.label), a)), r);
    }
    if let Some(c) = &a.confirm {
        let text = match c {
            Confirm::Signal { text, .. } => text.clone(),
            Confirm::StopScan => "Stop scanning? Stopping keeps what has been measured so far on screen.".into(),
            Confirm::ClearHistory => "This removes every recorded run. There is no undo.".into(),
            Confirm::Disconnect(id, u) => format!("Sign off session {id} of {u}? This ends the session and closes everything running in it."),
            Confirm::Quit => "The captured Flight Recorder trace is unsaved. Quit and discard it?".into(),
            Confirm::DiscardTrace => "Close and discard the captured trace?".into(),
            Confirm::ResetSettings => "Reset every setting to its default?".into(),
        };
        let r = centered(area, 64, 7);
        f.render_widget(Clear, r);
        f.render_widget(Paragraph::new(vec![Line::from(text), Line::from(""), Line::from("y / Enter = yes     any other key = no")]).wrap(Wrap { trim: true }).block(block("Confirm", a)), r);
    }
    if a.help {
        let text = "Pages:   Tab / Shift-Tab / [ ] / ← →\nQuit:    q or Ctrl-C        Filter: /  (Esc clears)\nSidebar: b                   Status bar: B\nRun new task: N              Help: ?\n\nProcesses:  ↑↓ PgUp PgDn  Enter details  x terminate (SIGTERM)  K kill (SIGKILL)\n            X terminate/force the whole tree (x then X)  p suspend  c continue\n            s send signal  n nice  a affinity  t tree  Space fold  E fold all\n            e show exited  v values/percent  < > sort column  o order  C copy cmdline\n            Y copy name  W search online  m write a sample report\n\nEvery page ships complete: Pro-only pages of the original (Benchmarks, Connections,\nDisk Space, Drivers, Flight Recorder, Installed Apps, Power & Freq) are all enabled.";
        let r = centered(area, 86, 18);
        f.render_widget(Clear, r);
        f.render_widget(Paragraph::new(text).block(block("Help — press any key", a)), r);
    }
}
