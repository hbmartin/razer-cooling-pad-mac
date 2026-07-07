//! Live TUI dashboard for the fan curve, with in-place tuning.
//!
//! Runs the same control step as `padctl curve` (shared [`Controller`]) but
//! renders it instead of logging it: raw vs smoothed temperature, target vs
//! actual RPM, the curve shape, and a scrolling panel of recent decisions.
//! Smoothing, down-delay, and the curve points can be adjusted live from
//! the keyboard — the whole point is tuning without restart cycles — and
//! the result saved back to the config file.
//!
//! No logger runs in this mode (a registered stderr logger would draw over
//! the alternate screen), so events that `curve` would log are recorded
//! directly in the decisions panel.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use hidapi::HidApi;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Block, Chart, Dataset, GraphType, List, ListItem, Paragraph};

use crate::curve::{self, Controller, CurveArgs, Decision, OnExit, rpm_text};
use crate::device::{OpenOpts, Pad, Selector};
use crate::fan;
use crate::lighting;
use crate::temp::TempReader;

/// Samples kept for the history charts (1 hour at the default 5s interval).
const HISTORY_CAP: usize = 720;

/// Decision/event lines kept in the scrolling panel.
const EVENTS_CAP: usize = 200;

/// Upper bound on the event-poll timeout, so the UI (uptime, pending-down
/// countdown) stays live between control ticks.
const UI_TICK: Duration = Duration::from_millis(150);

/// Seconds of history shown in the temperature/RPM charts.
const CHART_WINDOW: f64 = 600.0;

pub fn run(
    api: &mut HidApi,
    selector: &Selector,
    opts: OpenOpts,
    args: CurveArgs,
    verbose: bool,
) -> Result<()> {
    let file_config = if args.no_config {
        None
    } else {
        crate::config::load()?
    };
    let s = curve::resolve(&args, file_config.as_ref().map(|c| &c.curve))?;
    // Validate the [lighting] section up front, exactly like `curve`.
    let lighting = match &file_config {
        Some(c) => lighting::plan(&c.lighting)?,
        None => None,
    };
    let reader = TempReader::new()?;

    // Signal handling for SIGTERM/SIGHUP; in raw mode Ctrl-C arrives as a
    // key event instead of SIGINT, so the key handler also quits.
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::SeqCst))
        .context("installing signal handler")?;

    let mut app = App::new(&s, args.dry_run, verbose, selector, opts, reader, lighting);

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, api, &running);
    // Restore the terminal before the on-exit step so its messages (and any
    // error we return) print on a sane screen.
    ratatui::restore();
    result?;

    match s.on_exit {
        OnExit::Off if !args.dry_run => {
            let mut pad = app.pad.take();
            if pad.is_none() {
                let _ = api.refresh_devices();
                pad = Pad::open(api, selector, opts).ok();
            }
            match &pad {
                Some(p) => {
                    p.send(&fan::off())?;
                    println!("fan off");
                }
                None => eprintln!("warning: pad unavailable, could not turn the fan off"),
            }
        }
        _ => println!("leaving fan as-is"),
    }
    if app.dirty {
        println!(
            "tuned values not saved: --points {} --smooth {} --down-delay {}",
            points_text(app.controller.points()),
            app.controller.smooth(),
            app.controller.down_delay().as_secs()
        );
    }
    Ok(())
}

/// The startup tuning, restored by the reset key.
#[derive(Clone)]
struct Tuning {
    points: Vec<(f64, u32)>,
    smooth: f64,
    down_delay: Duration,
}

/// One control tick, as plotted by the history charts.
struct Sample {
    /// Seconds since the dashboard started.
    t: f64,
    raw: f64,
    smoothed: f64,
    target: u32,
    actual: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EventKind {
    /// A speed was pushed to the pad (or would be, in dry-run).
    Send,
    /// A lower target started waiting out the down-delay.
    PendingDown,
    /// Per-poll detail, recorded only with `--verbose`.
    Poll,
    /// Tuning changes, reconnects, saves.
    Info,
    Warn,
}

struct EventLine {
    ts: chrono::DateTime<chrono::Local>,
    kind: EventKind,
    text: String,
}

struct App<'a> {
    controller: Controller,
    interval: Duration,
    dry_run: bool,
    verbose: bool,
    selector: &'a Selector,
    opts: OpenOpts,
    reader: TempReader,
    lighting: Option<lighting::Plan>,
    pad: Option<Pad>,
    outage_reported: bool,
    pending_reported: bool,
    /// When the current pending spin-down started, for the countdown.
    pending_since: Option<Instant>,
    initial: Tuning,
    history: VecDeque<Sample>,
    events: VecDeque<EventLine>,
    /// Curve point selected for editing.
    selected: usize,
    paused: bool,
    quit: bool,
    /// Tuning changed since startup/save; hint to press `w`.
    dirty: bool,
    actual_rpm: Option<u32>,
    started: Instant,
    last_tick: Option<Instant>,
    next_tick: Instant,
}

impl<'a> App<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        s: &curve::Settings,
        dry_run: bool,
        verbose: bool,
        selector: &'a Selector,
        opts: OpenOpts,
        reader: TempReader,
        lighting: Option<lighting::Plan>,
    ) -> Self {
        let now = Instant::now();
        App {
            controller: Controller::new(
                s.points.clone(),
                s.smooth,
                Duration::from_secs(s.down_delay),
            ),
            interval: Duration::from_secs(s.interval),
            dry_run,
            verbose,
            selector,
            opts,
            reader,
            lighting,
            pad: None,
            outage_reported: false,
            pending_reported: false,
            pending_since: None,
            initial: Tuning {
                points: s.points.clone(),
                smooth: s.smooth,
                down_delay: Duration::from_secs(s.down_delay),
            },
            history: VecDeque::new(),
            events: VecDeque::new(),
            selected: 0,
            paused: false,
            quit: false,
            dirty: false,
            actual_rpm: None,
            started: now,
            last_tick: None,
            next_tick: now,
        }
    }

    fn push_event(&mut self, kind: EventKind, text: String) {
        if self.events.len() == EVENTS_CAP {
            self.events.pop_front();
        }
        self.events.push_back(EventLine {
            ts: chrono::Local::now(),
            kind,
            text,
        });
    }

    /// One control step: the same reconnect/read/decide/send sequence as
    /// `curve::run`, recording event lines instead of logging, plus an
    /// actual-RPM readback the headless loop does not need.
    fn control_tick(&mut self, api: &mut HidApi) {
        if !self.dry_run && self.pad.is_none() {
            let _ = api.refresh_devices();
            match Pad::open(api, self.selector, self.opts) {
                Ok(p) => {
                    if self.outage_reported {
                        self.push_event(EventKind::Info, "cooling pad reconnected".into());
                    }
                    self.outage_reported = false;
                    // The pad may have rebooted: restore lighting and make
                    // sure the next fan target is resent.
                    if let Some(plan) = &self.lighting {
                        let summary = plan.summary.clone();
                        match plan.packets.iter().try_for_each(|pkt| p.send(pkt)) {
                            Ok(()) => {
                                self.push_event(EventKind::Info, format!("lighting: {summary}"));
                            }
                            Err(e) => self.push_event(
                                EventKind::Warn,
                                format!("failed to apply lighting: {e:#}"),
                            ),
                        }
                    }
                    self.pad = Some(p);
                    self.controller.reset();
                }
                Err(e) => {
                    if !self.outage_reported {
                        self.push_event(EventKind::Warn, format!("{e:#}"));
                        self.outage_reported = true;
                    }
                }
            }
        }

        let now = Instant::now();
        let raw = match self.reader.read() {
            Ok(raw) => raw,
            Err(e) => {
                self.push_event(EventKind::Warn, format!("temperature read failed: {e:#}"));
                return;
            }
        };
        // Measured dt keeps the EMA correct across pauses and forced ticks.
        let dt = self.last_tick.map_or(self.interval.as_secs_f64(), |t| {
            now.duration_since(t).as_secs_f64()
        });
        self.last_tick = Some(now);

        let out = self.controller.tick(raw, dt, now);
        let detail = if self.controller.smooth() > 0.0 {
            format!(" (raw {:.1}°C)", out.raw)
        } else {
            String::new()
        };
        match out.decision {
            Decision::Send => {
                self.push_event(
                    EventKind::Send,
                    format!("{:5.1}°C → {}{detail}", out.smoothed, rpm_text(out.target)),
                );
                self.pending_reported = false;
                self.pending_since = None;
            }
            Decision::PendingDown => {
                if !self.pending_reported {
                    self.push_event(
                        EventKind::PendingDown,
                        format!(
                            "{:5.1}°C ‥ {} (down pending)",
                            out.smoothed,
                            rpm_text(out.target)
                        ),
                    );
                    self.pending_reported = true;
                    self.pending_since = Some(now);
                }
            }
            Decision::Hold => {
                self.pending_reported = false;
                self.pending_since = None;
                if self.verbose {
                    self.push_event(
                        EventKind::Poll,
                        format!("{:5.1}°C    {}{detail}", out.smoothed, rpm_text(out.target)),
                    );
                }
            }
        }

        if out.decision == Decision::Send {
            if self.dry_run {
                self.controller.confirm(out.target);
            } else if let Some(p) = &self.pad {
                let result = if out.target == 0 {
                    p.send(&fan::off())
                } else {
                    p.send(&fan::set_rpm(out.target))
                };
                match result {
                    Ok(()) => self.controller.confirm(out.target),
                    Err(e) => {
                        self.push_event(
                            EventKind::Warn,
                            format!("failed to set fan: {e:#} — will reconnect"),
                        );
                        self.pad = None;
                    }
                }
            }
            // No device right now: leave the controller unconfirmed so the
            // send is retried as soon as we reconnect.
        }

        // Actual-RPM readback for the dashboard; `curve` never needs this.
        self.actual_rpm = None;
        if !self.dry_run
            && let Some(p) = &self.pad
        {
            match p.read_report() {
                Ok(report) => self.actual_rpm = Some(fan::rpm_from_report(&report)),
                Err(e) => {
                    self.push_event(
                        EventKind::Warn,
                        format!("fan readback failed: {e:#} — will reconnect"),
                    );
                    self.pad = None;
                }
            }
        }

        if self.history.len() == HISTORY_CAP {
            self.history.pop_front();
        }
        self.history.push_back(Sample {
            t: self.started.elapsed().as_secs_f64(),
            raw: out.raw,
            smoothed: out.smoothed,
            target: out.target,
            actual: self.actual_rpm,
        });
    }

    fn apply(&mut self, action: Action) {
        // Tuning edits recompute immediately instead of waiting out the
        // interval, so the effect is visible on the next frame.
        let mut force_tick = false;
        match action {
            Action::Quit => self.quit = true,
            Action::TogglePause => {
                self.paused = !self.paused;
                let text = if self.paused { "paused" } else { "resumed" };
                self.push_event(EventKind::Info, text.into());
                force_tick = !self.paused;
            }
            Action::Smooth(dir) => {
                let cur = self.controller.smooth();
                let new = (cur + f64::from(dir)).max(0.0);
                if new != cur {
                    self.controller.set_smooth(new);
                    self.push_event(EventKind::Info, format!("smooth {cur}s → {new}s"));
                    self.dirty = true;
                }
                force_tick = true;
            }
            Action::Delay(dir) => {
                let cur = self.controller.down_delay().as_secs();
                let new = cur.saturating_add_signed(i64::from(dir) * 5);
                if new != cur {
                    self.controller.set_down_delay(Duration::from_secs(new));
                    self.push_event(EventKind::Info, format!("down-delay {cur}s → {new}s"));
                    self.dirty = true;
                }
                force_tick = true;
            }
            Action::SelectPoint(dir) => {
                let n = self.controller.points().len();
                self.selected =
                    (self.selected as i64 + i64::from(dir)).rem_euclid(n as i64) as usize;
            }
            Action::PointRpm(dir) => {
                let mut points = self.controller.points().to_vec();
                if adjust_rpm(&mut points, self.selected, dir > 0) {
                    let (t, r) = points[self.selected];
                    self.push_event(EventKind::Info, format!("point {t}°C → {}", rpm_text(r)));
                    self.controller.set_points(points);
                    self.dirty = true;
                }
                force_tick = true;
            }
            Action::PointTemp(dir) => {
                let mut points = self.controller.points().to_vec();
                if adjust_temp(&mut points, self.selected, dir > 0) {
                    let (t, r) = points[self.selected];
                    self.push_event(
                        EventKind::Info,
                        format!("point {} moved to {t}°C", rpm_text(r)),
                    );
                    self.controller.set_points(points);
                    self.dirty = true;
                }
                force_tick = true;
            }
            Action::Reset => {
                self.controller.set_points(self.initial.points.clone());
                self.controller.set_smooth(self.initial.smooth);
                self.controller.set_down_delay(self.initial.down_delay);
                self.selected = self.selected.min(self.initial.points.len() - 1);
                self.dirty = false;
                self.push_event(EventKind::Info, "tuning reset to startup values".into());
                force_tick = true;
            }
            Action::Save => {
                let text = points_text(self.controller.points());
                let smooth = self.controller.smooth();
                let delay = self.controller.down_delay().as_secs();
                match crate::config::save_curve_tuning(&text, smooth, delay) {
                    Ok(path) => {
                        self.push_event(EventKind::Info, format!("saved to {}", path.display()));
                        self.dirty = false;
                    }
                    Err(e) => self.push_event(EventKind::Warn, format!("save failed: {e:#}")),
                }
            }
        }
        if force_tick {
            self.next_tick = Instant::now();
        }
    }
}

/// What a keypress does; direction is +1/-1.
enum Action {
    Quit,
    TogglePause,
    Smooth(i8),
    Delay(i8),
    SelectPoint(i8),
    PointRpm(i8),
    PointTemp(i8),
    Reset,
    Save,
}

fn action_for(key: &KeyEvent) -> Option<Action> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Char(' ') => Some(Action::TogglePause),
        KeyCode::Char('S') => Some(Action::Smooth(1)),
        KeyCode::Char('s') => Some(Action::Smooth(-1)),
        KeyCode::Char('D') => Some(Action::Delay(1)),
        KeyCode::Char('d') => Some(Action::Delay(-1)),
        KeyCode::Tab => Some(Action::SelectPoint(1)),
        KeyCode::BackTab => Some(Action::SelectPoint(-1)),
        KeyCode::Up => Some(Action::PointRpm(1)),
        KeyCode::Down => Some(Action::PointRpm(-1)),
        KeyCode::Right => Some(Action::PointTemp(1)),
        KeyCode::Left => Some(Action::PointTemp(-1)),
        KeyCode::Char('r') => Some(Action::Reset),
        KeyCode::Char('w') => Some(Action::Save),
        _ => None,
    }
}

/// One device step up or down; stepping below the device minimum snaps to
/// 0 (off), and up from 0 snaps back to the minimum. Returns false when
/// already at the limit.
fn adjust_rpm(points: &mut [(f64, u32)], i: usize, up: bool) -> bool {
    let rpm = &mut points[i].1;
    let new = if up {
        match *rpm {
            0 => fan::MIN_RPM,
            r => (r + fan::RPM_STEP).min(fan::MAX_RPM),
        }
    } else {
        match *rpm {
            r if r <= fan::MIN_RPM => 0,
            r => r - fan::RPM_STEP,
        }
    };
    let changed = new != *rpm;
    *rpm = new;
    changed
}

/// Move a point ±1°C, clamped so temperatures stay strictly increasing
/// (at least 1°C between neighbors) and within a sane 0-110°C range.
fn adjust_temp(points: &mut [(f64, u32)], i: usize, right: bool) -> bool {
    let t = points[i].0;
    let new = if right { t + 1.0 } else { t - 1.0 };
    let lo = if i == 0 { 0.0 } else { points[i - 1].0 + 1.0 };
    let hi = if i == points.len() - 1 {
        110.0
    } else {
        points[i + 1].0 - 1.0
    };
    if new < lo || new > hi {
        return false;
    }
    points[i].0 = new;
    true
}

/// Render curve points back into the `--points`/config text form.
fn points_text(points: &[(f64, u32)]) -> String {
    points
        .iter()
        .map(|(t, r)| {
            if t.fract() == 0.0 {
                format!("{t:.0}:{r}")
            } else {
                format!("{t}:{r}")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    api: &mut HidApi,
    running: &AtomicBool,
) -> Result<()> {
    while !app.quit && running.load(Ordering::SeqCst) {
        terminal.draw(|f| ui(f, app)).context("drawing the UI")?;
        let timeout = app
            .next_tick
            .saturating_duration_since(Instant::now())
            .min(UI_TICK);
        if event::poll(timeout).context("polling terminal events")?
            && let Event::Key(key) = event::read().context("reading terminal event")?
            && key.kind == KeyEventKind::Press
            && let Some(action) = action_for(&key)
        {
            app.apply(action);
        }
        if !app.paused && Instant::now() >= app.next_tick {
            app.control_tick(api);
            app.next_tick = Instant::now() + app.interval;
        }
    }
    Ok(())
}

const RAW_COLOR: Color = Color::DarkGray;
const SMOOTH_COLOR: Color = Color::Cyan;
const TARGET_COLOR: Color = Color::Yellow;
const ACTUAL_COLOR: Color = Color::Green;

fn ui(f: &mut Frame, app: &App) {
    let [header, tiles, main, decisions, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(8),
        Constraint::Length(2),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    draw_tiles(f, app, tiles);

    let [charts, curve_panel] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(main);
    let [temp_chart, rpm_chart] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(charts);
    draw_temp_chart(f, app, temp_chart);
    draw_rpm_chart(f, app, rpm_chart);
    draw_curve_panel(f, app, curve_panel);

    draw_decisions(f, app, decisions);
    draw_footer(f, app, footer);
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let uptime = app.started.elapsed().as_secs();
    let mut spans = vec![
        Span::styled(" padctl watch ", Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(format!("─ temp: {} ", app.reader.source_name())),
        Span::raw("─ pad: "),
        if app.dry_run {
            Span::styled("dry-run", Style::new().fg(Color::Magenta))
        } else if app.pad.is_some() {
            Span::styled("connected", Style::new().fg(Color::Green))
        } else {
            Span::styled("not connected", Style::new().fg(Color::Red))
        },
        Span::raw(format!(
            " ─ {:02}:{:02}:{:02} ",
            uptime / 3600,
            uptime / 60 % 60,
            uptime % 60
        )),
    ];
    if app.paused {
        spans.push(Span::styled(
            " PAUSED ",
            Style::new().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    f.render_widget(Line::from(spans), area);
}

fn tile<'t>(title: &'t str, value: String, color: Color) -> Paragraph<'t> {
    Paragraph::new(Line::from(Span::styled(
        value,
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )))
    .centered()
    .block(Block::bordered().title(title))
}

fn draw_tiles(f: &mut Frame, app: &App, area: Rect) {
    let [raw, smoothed, target, actual] = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
    ])
    .areas(area);
    let last = app.history.back();
    let fmt_temp = |v: Option<f64>| v.map_or("—".into(), |v| format!("{v:.1}°C"));
    f.render_widget(tile("raw", fmt_temp(last.map(|s| s.raw)), RAW_COLOR), raw);
    f.render_widget(
        tile(
            "smoothed",
            format!(
                "{} (τ {}s)",
                fmt_temp(last.map(|s| s.smoothed)),
                app.controller.smooth()
            ),
            SMOOTH_COLOR,
        ),
        smoothed,
    );
    f.render_widget(
        tile(
            "target",
            last.map_or("—".into(), |s| rpm_text(s.target)),
            TARGET_COLOR,
        ),
        target,
    );
    f.render_widget(
        tile(
            "actual",
            app.actual_rpm.map_or("n/a".into(), rpm_text),
            ACTUAL_COLOR,
        ),
        actual,
    );
}

/// The samples inside the chart window, as (x, y) pairs per series.
fn window<'s>(app: &'s App) -> impl Iterator<Item = &'s Sample> {
    let cutoff = app.started.elapsed().as_secs_f64() - CHART_WINDOW;
    app.history.iter().filter(move |s| s.t >= cutoff)
}

fn time_bounds(app: &App) -> [f64; 2] {
    let now = app.started.elapsed().as_secs_f64();
    [(now - CHART_WINDOW).max(0.0), now.max(60.0)]
}

fn draw_temp_chart(f: &mut Frame, app: &App, area: Rect) {
    let raw: Vec<(f64, f64)> = window(app).map(|s| (s.t, s.raw)).collect();
    let smoothed: Vec<(f64, f64)> = window(app).map(|s| (s.t, s.smoothed)).collect();
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for &(_, v) in raw.iter().chain(&smoothed) {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if lo > hi {
        (lo, hi) = (30.0, 90.0);
    }
    let (lo, hi) = ((lo - 2.0).floor(), (hi + 2.0).ceil());
    let datasets = vec![
        Dataset::default()
            .name("raw")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(RAW_COLOR))
            .data(&raw),
        Dataset::default()
            .name("smoothed")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(SMOOTH_COLOR))
            .data(&smoothed),
    ];
    let chart = Chart::new(datasets)
        .block(Block::bordered().title("temperature °C"))
        .x_axis(Axis::default().bounds(time_bounds(app)))
        .y_axis(
            Axis::default()
                .bounds([lo, hi])
                .labels([format!("{lo:.0}"), format!("{hi:.0}")]),
        );
    f.render_widget(chart, area);
}

fn draw_rpm_chart(f: &mut Frame, app: &App, area: Rect) {
    let target: Vec<(f64, f64)> = window(app).map(|s| (s.t, f64::from(s.target))).collect();
    let actual: Vec<(f64, f64)> = window(app)
        .filter_map(|s| s.actual.map(|a| (s.t, f64::from(a))))
        .collect();
    let max_y = f64::from(fan::MAX_RPM) + 100.0;
    let datasets = vec![
        Dataset::default()
            .name("target")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(TARGET_COLOR))
            .data(&target),
        Dataset::default()
            .name("actual")
            .marker(symbols::Marker::Dot)
            .style(Style::new().fg(ACTUAL_COLOR))
            .data(&actual),
    ];
    let chart = Chart::new(datasets)
        .block(Block::bordered().title("fan RPM"))
        .x_axis(Axis::default().bounds(time_bounds(app)))
        .y_axis(
            Axis::default()
                .bounds([0.0, max_y])
                .labels(["0".to_string(), format!("{}", fan::MAX_RPM)]),
        );
    f.render_widget(chart, area);
}

fn draw_curve_panel(f: &mut Frame, app: &App, area: Rect) {
    let points = app.controller.points();
    let [chart_area, list_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(points.len() as u16)])
            .areas(area);

    let curve: Vec<(f64, f64)> = points.iter().map(|&(t, r)| (t, f64::from(r))).collect();
    let selected = [curve[app.selected.min(curve.len() - 1)]];
    let operating: Vec<(f64, f64)> = app
        .history
        .back()
        .map(|s| (s.smoothed, f64::from(s.target)))
        .into_iter()
        .collect();
    let x_lo = (points.first().map_or(40.0, |p| p.0) - 5.0).floor();
    let x_hi = (points.last().map_or(100.0, |p| p.0) + 5.0).ceil();
    let datasets = vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::new().fg(Color::Blue))
            .data(&curve),
        Dataset::default()
            .marker(symbols::Marker::Block)
            .style(Style::new().fg(Color::White))
            .data(&selected),
        Dataset::default()
            .marker(symbols::Marker::Block)
            .style(Style::new().fg(SMOOTH_COLOR))
            .data(&operating),
    ];
    let chart = Chart::new(datasets)
        .block(Block::bordered().title("curve (Tab select, ←→↑↓ edit)"))
        .x_axis(
            Axis::default()
                .bounds([x_lo, x_hi])
                .labels([format!("{x_lo:.0}°C"), format!("{x_hi:.0}°C")]),
        )
        .y_axis(Axis::default().bounds([0.0, f64::from(fan::MAX_RPM) + 100.0]));
    f.render_widget(chart, chart_area);

    let rows: Vec<ListItem> = points
        .iter()
        .enumerate()
        .map(|(i, &(t, r))| {
            let marker = if i == app.selected { "▶" } else { " " };
            let style = if i == app.selected {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            ListItem::new(Line::from(Span::styled(
                format!("{marker} {t:>5.1}°C : {}", rpm_text(r)),
                style,
            )))
        })
        .collect();
    f.render_widget(List::new(rows), list_area);
}

fn draw_decisions(f: &mut Frame, app: &App, area: Rect) {
    let rows: Vec<ListItem> = app
        .events
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .map(|e| {
            let style = match e.kind {
                EventKind::Send => Style::new(),
                EventKind::PendingDown => Style::new().fg(TARGET_COLOR),
                EventKind::Poll => Style::new().fg(Color::DarkGray),
                EventKind::Info => Style::new().fg(SMOOTH_COLOR),
                EventKind::Warn => Style::new().fg(Color::Red),
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", e.ts.format("%H:%M:%S")),
                    Style::new().fg(Color::DarkGray),
                ),
                Span::styled(e.text.clone(), style),
            ]))
        })
        .collect();
    f.render_widget(
        List::new(rows).block(Block::bordered().title("decisions")),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let mut status = format!(
        "smooth {}s  down-delay {}s  poll {}s  point {}/{}",
        app.controller.smooth(),
        app.controller.down_delay().as_secs(),
        app.interval.as_secs(),
        app.selected + 1,
        app.controller.points().len(),
    );
    if let Some(since) = app.pending_since {
        status.push_str(&format!(
            "  ─ down pending {}s/{}s",
            since.elapsed().as_secs(),
            app.controller.down_delay().as_secs()
        ));
    }
    let mut spans = vec![Span::raw(status)];
    if app.dirty {
        spans.push(Span::styled(
            "  * unsaved — press w",
            Style::new().fg(TARGET_COLOR),
        ));
    }
    let help = "[S/s] smooth ± [D/d] delay ± [Tab] point [←→↑↓] edit \
                [w] save [r] reset [space] pause [q] quit";
    let lines = vec![
        Line::from(spans),
        Line::from(Span::styled(help, Style::new().fg(Color::DarkGray))),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymap_covers_the_documented_keys() {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(matches!(
            action_for(&key(KeyCode::Char('q'))),
            Some(Action::Quit)
        ));
        assert!(matches!(action_for(&key(KeyCode::Esc)), Some(Action::Quit)));
        assert!(matches!(
            action_for(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        ));
        assert!(matches!(
            action_for(&key(KeyCode::Char('S'))),
            Some(Action::Smooth(1))
        ));
        assert!(matches!(
            action_for(&key(KeyCode::Char('s'))),
            Some(Action::Smooth(-1))
        ));
        assert!(matches!(
            action_for(&key(KeyCode::Char('D'))),
            Some(Action::Delay(1))
        ));
        assert!(matches!(
            action_for(&key(KeyCode::Char('d'))),
            Some(Action::Delay(-1))
        ));
        assert!(matches!(
            action_for(&key(KeyCode::Tab)),
            Some(Action::SelectPoint(1))
        ));
        assert!(matches!(
            action_for(&key(KeyCode::Up)),
            Some(Action::PointRpm(1))
        ));
        assert!(matches!(
            action_for(&key(KeyCode::Left)),
            Some(Action::PointTemp(-1))
        ));
        assert!(action_for(&key(KeyCode::Char('x'))).is_none());
    }

    #[test]
    fn adjust_rpm_steps_and_snaps_at_the_edges() {
        let mut points = vec![(55.0, 800), (85.0, 3200)];
        assert!(adjust_rpm(&mut points, 0, true));
        assert_eq!(points[0].1, 850);
        assert!(!adjust_rpm(&mut points, 1, true)); // already at MAX_RPM
        // Stepping below the minimum snaps to off, and back up to min.
        points[0].1 = fan::MIN_RPM;
        assert!(adjust_rpm(&mut points, 0, false));
        assert_eq!(points[0].1, 0);
        assert!(!adjust_rpm(&mut points, 0, false)); // stays off
        assert!(adjust_rpm(&mut points, 0, true));
        assert_eq!(points[0].1, fan::MIN_RPM);
    }

    #[test]
    fn adjust_temp_keeps_points_strictly_increasing() {
        let mut points = vec![(55.0, 800), (57.0, 1500), (85.0, 3200)];
        assert!(adjust_temp(&mut points, 0, true)); // 55 -> 56, still 1° below 57
        assert!(!adjust_temp(&mut points, 0, true)); // would collide with 57
        assert!(adjust_temp(&mut points, 1, true)); // 57 -> 58
        assert_eq!(points[1].0, 58.0);
        // Clamped to the sane range at the extremes.
        let mut points = vec![(0.0, 800), (110.0, 3200)];
        assert!(!adjust_temp(&mut points, 0, false));
        assert!(!adjust_temp(&mut points, 1, true));
    }

    #[test]
    fn points_text_round_trips_through_the_parser() {
        let points = vec![(55.0, 800), (65.5, 1500), (85.0, 0)];
        assert_eq!(points_text(&points), "55:800,65.5:1500,85:0");
    }
}
