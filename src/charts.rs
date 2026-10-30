//! One chart look for the whole application, built on [`tui_chart`].
//!
//! Every time series (CPU, memory, disks, network, GPU, temperatures, trace replay) is drawn by
//! [`Chart`], every bar by [`meter`] and every progress indicator by [`gauge`], so they share one
//! palette, one axis style, one level gradient and one statistics legend:
//!
//! - load and temperature series fill with a green → amber → red gradient by value, and their line
//!   takes the color of the latest reading;
//! - throughput series (disk read/write, network receive/send) keep one hue each, fading to dim
//!   towards the baseline, and are auto-scaled to a round maximum in the chosen unit;
//! - scales follow the settings: °C or °F, bytes or bits per second;
//! - the bottom border carries min / max / average over the visible window;
//! - axes and labels appear when the chart has room for them, small tiles stay uncluttered.

use crate::app::App;
use crate::util::{bytes, net_rate, rate};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui::Frame;
use std::collections::VecDeque;
use tui_chart::{Axis, Gauge, GaugeStyle, Gradient, LegendPosition, LineChart, LineMode, Series};

pub type Rgb = (u8, u8, u8);

/// Colors shared by every chart, tuned for dark or light terminal backgrounds.
#[derive(Clone, Copy)]
pub struct Palette {
    light: bool,
    ok: Rgb,
    warn: Rgb,
    crit: Rgb,
    pub read: Rgb,
    pub write: Rgb,
    pub rx: Rgb,
    pub tx: Rgb,
    pub accent: Rgb,
    pub used: Rgb,
    pub cached: Rgb,
    pub free: Rgb,
    axis: Color,
    label: Color,
    track: Color,
}

impl Palette {
    pub fn of(a: &App) -> Self {
        if a.s.light_theme {
            Palette {
                light: true,
                ok: (22, 150, 80),
                warn: (196, 140, 0),
                crit: (206, 48, 48),
                read: (0, 128, 160),
                write: (200, 90, 0),
                rx: (150, 60, 190),
                tx: (30, 110, 210),
                accent: (30, 100, 200),
                used: (196, 140, 0),
                cached: (30, 110, 210),
                free: (22, 150, 80),
                axis: Color::Gray,
                label: Color::DarkGray,
                track: Color::Gray,
            }
        } else {
            Palette {
                light: false,
                ok: (80, 210, 120),
                warn: (240, 200, 60),
                crit: (245, 80, 70),
                read: (70, 200, 230),
                write: (250, 150, 70),
                rx: (210, 120, 250),
                tx: (90, 160, 255),
                accent: (80, 200, 230),
                used: (240, 200, 60),
                cached: (90, 160, 255),
                free: (80, 210, 120),
                axis: Color::DarkGray,
                label: Color::Gray,
                track: Color::DarkGray,
            }
        }
    }

    /// Step color for a level reading (percent of capacity, or °C): green, amber from 60, red from 85.
    pub fn level(&self, p: f64) -> Color {
        rgb(if p >= 85.0 {
            self.crit
        } else if p >= 60.0 {
            self.warn
        } else {
            self.ok
        })
    }

    /// Continuous level color, for gradients: green up to 40, amber at 65, red from 90.
    fn heat(&self, p: f64) -> Rgb {
        let stops = [(40.0, self.ok), (65.0, self.warn), (90.0, self.crit)];
        if p <= stops[0].0 {
            return stops[0].1;
        }
        for w in stops.windows(2) {
            if p <= w[1].0 {
                return mix(w[0].1, w[1].1, (p - w[0].0) / (w[1].0 - w[0].0));
            }
        }
        self.crit
    }

    /// Area fill shade of a line color: darker on dark terminals, paler on light ones.
    fn shade(&self, c: Rgb, k: f64) -> Rgb {
        if self.light {
            mix(c, (255, 255, 255), k)
        } else {
            mix(c, (0, 0, 0), k)
        }
    }
}

pub fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// Level color of a reading (percent of capacity, or °C), shared by charts, meters and lists.
pub fn level(a: &App, p: f64) -> Color {
    Palette::of(a).level(p)
}

/// What a series measures: decides its scale, its labels and how it is colored.
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// 0–100 % of a capacity (CPU, memory, disk active time, GPU), level gradient.
    Percent,
    /// Temperatures, given in °C and shown in the chosen unit, level gradient.
    Temp,
    /// Bytes per second (disks).
    Rate,
    /// Network rate: bytes or bits per second depending on the setting.
    NetRate,
    /// Electrical power in watts (battery draw).
    Power,
}

impl Kind {
    fn level(self) -> bool {
        matches!(self, Kind::Percent | Kind::Temp)
    }

    /// A raw reading as text (temperatures are in °C).
    fn fmt(self, a: &App, v: f64) -> String {
        match self {
            Kind::Percent => format!("{v:.0}%"),
            Kind::Temp => a.s.temp(v),
            Kind::Rate => rate(v),
            Kind::NetRate => net_rate(v),
            Kind::Power => format!("{v:.1} W"),
        }
    }
}

struct Trend {
    name: String,
    values: Vec<f64>,
    hue: Option<Rgb>,
}

/// Time-series chart: newest sample at the right edge, one or more series in the same unit.
pub struct Chart {
    kind: Kind,
    block: Block<'static>,
    series: Vec<Trend>,
    capacity: usize,
    span_secs: Option<f64>,
    /// slots to the right of "now" reserved for a forecast, and the forecast itself
    future: usize,
    ahead: Vec<Trend>,
}

impl Chart {
    pub fn new(kind: Kind, block: Block<'static>) -> Self {
        Chart { kind, block, series: Vec::new(), capacity: crate::app::HIST_LEN, span_secs: None, future: 0, ahead: Vec::new() }
    }

    /// Add a series of raw readings (oldest first). `hue` is required for throughput charts and
    /// ignored by load and temperature charts, which are colored by level.
    pub fn series(mut self, name: impl Into<String>, values: Vec<f64>, hue: Option<Rgb>) -> Self {
        self.series.push(Trend { name: name.into(), values, hue });
        self
    }

    /// Add a series from a sample history, multiplied by `scale` (0.1 for temperatures stored in tenths).
    pub fn history(self, name: impl Into<String>, h: &VecDeque<u64>, scale: f64, hue: Option<Rgb>) -> Self {
        let v = h.iter().map(|&x| x as f64 * scale).collect();
        self.series(name, v, hue)
    }

    /// Reserve `slots` to the right of "now" and draw `values` there as a forecast line (the first
    /// value is the current reading). Use a capacity of `2 * slots + 1` so "now" sits mid-axis.
    pub fn forecast(mut self, slots: usize, name: impl Into<String>, values: Vec<f64>) -> Self {
        self.future = slots;
        self.ahead.push(Trend { name: name.into(), values, hue: None });
        self
    }

    /// Number of samples the x axis spans (default: the live history length).
    pub fn capacity(mut self, n: usize) -> Self {
        self.capacity = n.max(2);
        self
    }

    /// Time covered by the x axis, for its labels (default: capacity × sample interval).
    pub fn span_secs(mut self, s: f64) -> Self {
        self.span_secs = Some(s);
        self
    }

    pub fn render(self, f: &mut Frame, area: Rect, a: &App) {
        let pal = Palette::of(a);
        let kind = self.kind;
        let disp = |v: f64| if kind == Kind::Temp { a.s.temp_value(v) } else { v };
        let inner = self.block.inner(area);
        let show_y = inner.width >= 28 && inner.height >= 4;
        let show_x = inner.height >= 12 && inner.width >= 28;

        // vertical bounds, in display units
        let all = self.series.iter().flat_map(|s| s.values.iter().copied()).filter(|v| v.is_finite());
        let (mn, mx) = all.fold((f64::MAX, f64::MIN), |m, v| (m.0.min(v), m.1.max(v)));
        let empty = mn > mx;
        let (lo, hi) = match kind {
            Kind::Percent => (0.0, 100.0),
            Kind::Temp => {
                let (lo, hi) = if empty { (disp(20.0), disp(60.0)) } else { (disp(mn), disp(mx)) };
                // at least 10 degrees of range, so sensor noise does not look like a spike
                let pad = ((10.0 - (hi - lo)) / 2.0).max(0.0) + 1.0;
                ((lo - pad).floor(), (hi + pad).ceil())
            }
            Kind::Rate => (0.0, nice_ceiling(if empty { 0.0 } else { mx }, 1024.0, 1.0)),
            Kind::NetRate if a.s.net_bits => (0.0, nice_ceiling(if empty { 0.0 } else { mx }, 1000.0, 8.0)),
            Kind::NetRate => (0.0, nice_ceiling(if empty { 0.0 } else { mx }, 1024.0, 1.0)),
            Kind::Power => (0.0, ((if empty { 0.0 } else { mx } * 1.15).max(5.0) / 5.0).ceil() * 5.0),
        };
        let ylab = |v: f64| match kind {
            Kind::Percent => format!("{v:.0}%"),
            Kind::Temp => format!("{v:.0}{}", a.s.temp_unit()),
            Kind::Rate => compact(bytes(v)),
            Kind::NetRate => compact(net_rate(v).trim_end_matches("/s").to_string()),
            Kind::Power => format!("{v:.0}W"),
        };
        // level colors are sampled along the value scale, so the fill reads like a thermometer
        let level_at = |d: f64| {
            let c = if kind == Kind::Temp && a.s.fahrenheit { (d - 32.0) * 5.0 / 9.0 } else { d };
            pal.heat(c)
        };

        let start = |n: usize| self.capacity.saturating_sub(n + self.future) as f64;
        let mut out = Vec::with_capacity(self.series.len());
        for (i, s) in self.series.iter().enumerate() {
            let shown = &s.values[s.values.len().saturating_sub(self.capacity)..];
            let x0 = start(shown.len());
            let pts: Vec<(f64, f64)> = shown.iter().enumerate().map(|(k, &v)| (x0 + k as f64, disp(v).clamp(lo, hi))).collect();
            let (line, grad) = if kind.level() || s.hue.is_none() {
                let last = s.values.last().copied().unwrap_or(0.0);
                let stops = (0..12).map(|k| pal.shade(level_at(lo + (hi - lo) * k as f64 / 11.0), 0.25)).collect();
                (pal.level(last), Gradient::Custom(stops))
            } else {
                let h = s.hue.unwrap_or(pal.accent);
                (rgb(h), Gradient::Custom(vec![pal.shade(h, 0.8), pal.shade(h, 0.35)]))
            };
            // with two series the second is drawn as a plain line over the first one's area
            let mode = if i == 0 { LineMode::Area } else { LineMode::Line };
            out.push(Series::new(s.name.clone()).data(pts).color(line).fill_gradient(grad).mode(mode));
        }

        // the forecast is a dimmed line starting at "now"
        let now_x = self.capacity.saturating_sub(self.future + 1) as f64;
        for s in &self.ahead {
            let pts: Vec<(f64, f64)> = s.values.iter().take(self.future + 1).enumerate().map(|(k, &v)| (now_x + k as f64, disp(v).clamp(lo, hi))).collect();
            out.push(Series::new(s.name.clone()).data(pts).color(pal.label).mode(LineMode::Line));
        }

        let axis = |labels: Vec<String>| Axis::new().labels(labels).style(Style::new().fg(pal.axis)).label_style(Style::new().fg(pal.label)).grid_style(Style::new().fg(pal.axis));
        let span = self.span_secs.unwrap_or(self.capacity as f64 * a.s.refresh_ms.max(250) as f64 / 1000.0);
        let xl = if show_x && self.future > 0 {
            vec![format!("-{}", ago(span / 2.0)), "now".into(), format!("+{}", ago(span / 2.0))]
        } else if show_x {
            vec![format!("-{}", ago(span)), format!("-{}", ago(span / 2.0)), "now".into()]
        } else {
            vec![]
        };
        let yl = if show_y { vec![ylab(lo), ylab((lo + hi) / 2.0), ylab(hi)] } else { vec![] };
        // the statistics legend drops min, then max, until it fits the bottom border
        let room = area.width.saturating_sub(4) as usize;
        let stats = (0..3).rev().map(|detail| stats_line(a, kind, &self.series, &pal, detail)).find(|l| l.width() <= room).unwrap_or_default();
        let block = self.block.title_bottom(stats);
        // the colored series names in the statistics legend replace a legend box over the plot
        let chart = LineChart::new(out)
            .smooth(true)
            .legend(LegendPosition::None)
            .x_axis(axis(xl).bounds(0.0, (self.capacity - 1) as f64))
            .y_axis(axis(yl).bounds(lo, hi).grid(show_x))
            .block(block);
        f.render_widget(chart, area);
    }
}

/// Statistics of each series over the window, for the chart's bottom border:
/// `detail` 2 = min, max and average, 1 = max and average, 0 = average.
fn stats_line(a: &App, kind: Kind, series: &[Trend], pal: &Palette, detail: u8) -> Line<'static> {
    let dim = Style::new().fg(pal.label);
    let mut spans = vec![Span::raw(" ")];
    for (i, s) in series.iter().enumerate() {
        let v: Vec<f64> = s.values.iter().copied().filter(|v| v.is_finite()).collect();
        if i > 0 {
            spans.push(Span::styled(" │ ", Style::new().fg(pal.axis)));
        }
        if series.len() > 1 && !s.name.is_empty() {
            let c = if kind.level() { pal.level(v.last().copied().unwrap_or(0.0)) } else { rgb(s.hue.unwrap_or(pal.accent)) };
            spans.push(Span::styled(format!("{} ", s.name), Style::new().fg(c)));
        }
        if v.is_empty() {
            spans.push(Span::styled("no samples yet", dim));
            continue;
        }
        let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |m, &x| (m.0.min(x), m.1.max(x)));
        let avg = v.iter().sum::<f64>() / v.len() as f64;
        let mut t = Vec::new();
        if detail >= 2 {
            t.push(format!("min {}", kind.fmt(a, lo)));
        }
        if detail >= 1 {
            t.push(format!("max {}", kind.fmt(a, hi)));
        }
        t.push(format!("avg {}", kind.fmt(a, avg)));
        spans.push(Span::styled(t.join("  "), dim));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Round axis maximum for a throughput chart: 1, 2, 2.5 or 5 × a power of `base`, in the unit
/// the labels use (`per` converts bytes to that unit, 8 for bits).
fn nice_ceiling(max_bytes: f64, base: f64, per: f64) -> f64 {
    let v = (max_bytes * per * 1.1).max(base); // at least 1 K, so an idle link does not look busy
    let mut p = 1.0;
    while v / p >= base {
        p *= base;
    }
    let m = v / p;
    let step = [1.0, 2.0, 2.5, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0, 1024.0].into_iter().find(|&s| s >= m).unwrap_or(base);
    step * p / per
}

/// Shorter axis label: "1.0 MB" → "1MB", "512 KB" → "512KB".
fn compact(s: String) -> String {
    let s = s.replace(".0 ", " ");
    s.replace(' ', "")
}

/// "90s", "4m", "1h30m".
fn ago(secs: f64) -> String {
    let s = secs.round() as u64;
    if s < 120 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s % 3600 / 60 == 0 {
        format!("{}h", s / 3600)
    } else {
        format!("{}h{}m", s / 3600, s % 3600 / 60)
    }
}

const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Sub-cell bar of `w` cells for a percentage: filled part plus a dim track.
pub fn meter_text(p: f64, w: usize) -> (String, String) {
    let cells = p.clamp(0.0, 100.0) / 100.0 * w as f64;
    let mut full = cells.floor() as usize;
    let mut part = ((cells - full as f64) * 8.0).round() as usize;
    if part == 8 {
        full += 1;
        part = 0;
    }
    let full = full.min(w);
    let mut fill = "█".repeat(full);
    let mut used = full;
    if part > 0 && full < w {
        fill.push_str(EIGHTHS[part]);
        used += 1;
    }
    (fill, "░".repeat(w - used))
}

/// Inline bar for text lines: level-colored unless a hue is given.
pub fn meter(a: &App, p: f64, w: usize, hue: Option<Rgb>) -> Vec<Span<'static>> {
    let pal = Palette::of(a);
    let (fill, track) = meter_text(p, w);
    let c = hue.map(rgb).unwrap_or_else(|| pal.level(p));
    vec![Span::styled(fill, Style::new().fg(c)), Span::styled(track, Style::new().fg(pal.track))]
}

/// Progress or level bar widget with the same look as [`meter`]; `hue` None colors it by level.
pub fn gauge(f: &mut Frame, area: Rect, a: &App, ratio: f64, label: String, block: Block<'_>, hue: Option<Rgb>) {
    let pal = Palette::of(a);
    let p = (ratio * 100.0).clamp(0.0, 100.0);
    let c = hue.map(rgb).unwrap_or_else(|| pal.level(p));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let row = Rect { height: inner.height.min(1), ..inner };
    f.render_widget(Gauge::new(p).style(GaugeStyle::Smooth).color(c).label(label).base_style(Style::new().fg(pal.label)), row);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ceilings() {
        assert_eq!(nice_ceiling(0.0, 1024.0, 1.0), 1024.0);
        assert_eq!(nice_ceiling(1500.0, 1024.0, 1.0), 2048.0);
        assert_eq!(nice_ceiling(3.0 * 1024.0 * 1024.0, 1024.0, 1.0), 5.0 * 1024.0 * 1024.0);
        // bits: 1 MB/s = 8 Mb/s → 10 Mb/s
        assert_eq!(nice_ceiling(1_000_000.0, 1000.0, 8.0), 10_000_000.0 / 8.0);
    }

    #[test]
    fn meters() {
        assert_eq!(meter_text(0.0, 4), (String::new(), "░░░░".into()));
        assert_eq!(meter_text(100.0, 4), ("████".into(), String::new()));
        assert_eq!(meter_text(50.0, 4), ("██".into(), "░░".into()));
        let (f, t) = meter_text(30.0, 10);
        assert_eq!(f.chars().count() + t.chars().count(), 10);
    }

    #[test]
    fn labels() {
        assert_eq!(ago(240.0), "4m");
        assert_eq!(ago(30.0), "30s");
        assert_eq!(compact("1.0 MB".into()), "1MB");
    }
}
