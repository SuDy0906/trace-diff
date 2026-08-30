//! Interactive ratatui dashboard with live progress, baseline picker, themes, export.

use crate::diff::{diff_runs, DiffReport, DiffThresholds, Severity};
use crate::l7::L7Metrics;
use crate::meta::RunMetadata;
use crate::progress::ProgressEvent;
use crate::store::{BaselineInfo, Store, StoredRun};
use crate::theme::{Theme, ThemeName};
use crate::traceroute::TraceResult;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Wrap};
use ratatui::Terminal;
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;

type RunProbeResult = crate::error::Result<(StoredRun, Option<DiffReport>, Option<String>)>;

#[derive(Debug, Clone)]
pub struct AppView {
    pub target: String,
    pub resolved: Option<String>,
    pub baseline: Option<String>,
    pub trace: Option<TraceResult>,
    pub l7: Option<L7Metrics>,
    pub diff: Option<DiffReport>,
    pub meta: Option<RunMetadata>,
    pub run_id: Option<String>,
}

impl AppView {
    pub fn from_run(
        run: &StoredRun,
        diff: Option<DiffReport>,
        tagged_baseline: Option<String>,
    ) -> Self {
        Self {
            target: run.target.clone(),
            resolved: run.resolved_ip.clone(),
            baseline: diff
                .as_ref()
                .and_then(|d| d.baseline_name.clone())
                .or(tagged_baseline),
            trace: run.trace.clone(),
            l7: run.l7.clone(),
            diff,
            meta: run.meta.clone(),
            run_id: Some(run.id.clone()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Help,
    BaselinePicker,
    Guide,
}

struct App {
    view: AppView,
    theme: Theme,
    overlay: Overlay,
    baselines: Vec<BaselineInfo>,
    baseline_idx: usize,
    status: String,
    progress_log: Vec<String>,
    probing: bool,
    store: Option<Store>,
    last_export: Option<PathBuf>,
    /// Show OS/privileges/raw meta (press m).
    show_advanced: bool,
    /// Pulse frame for subtle progress animation.
    tick: u64,
    /// Structured live-progress checklist (not string matching).
    progress_steps: ProgressSteps,
}

#[derive(Debug, Clone, Default)]
struct ProgressSteps {
    dns: StepState,
    tcp: StepState,
    tls: StepState,
    ttfb: StepState,
    transfer: StepState,
    hops: StepState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StepState {
    #[default]
    Pending,
    Active,
    Done,
    Skipped,
}

impl ProgressSteps {
    fn apply(&mut self, ev: &ProgressEvent) {
        match ev {
            ProgressEvent::L7Phase { phase } => {
                let p = phase.to_lowercase();
                // Clear previous active L7 marks to Done when moving on.
                self.advance_l7(&p);
            }
            ProgressEvent::L7Finished { .. } => {
                self.dns = promote(self.dns);
                self.tcp = promote(self.tcp);
                self.tls = promote(self.tls);
                self.ttfb = promote(self.ttfb);
                self.transfer = StepState::Done;
            }
            ProgressEvent::TraceStarted { .. } => {
                self.hops = StepState::Active;
            }
            ProgressEvent::TraceHop { .. } => {
                self.hops = StepState::Active;
            }
            ProgressEvent::TraceFinished { .. } => {
                self.hops = StepState::Done;
            }
            ProgressEvent::TraceSkipped { .. } => {
                self.hops = StepState::Skipped;
            }
            _ => {}
        }
    }

    fn advance_l7(&mut self, phase: &str) {
        if phase.contains("dns") {
            self.dns = StepState::Active;
        } else if phase.contains("tcp") {
            self.dns = promote(self.dns);
            self.tcp = StepState::Active;
        } else if phase.contains("tls") {
            self.dns = promote(self.dns);
            self.tcp = promote(self.tcp);
            self.tls = StepState::Active;
        } else if phase.contains("ttfb") || phase.contains("request") {
            self.dns = promote(self.dns);
            self.tcp = promote(self.tcp);
            // Plain HTTP skips TLS — don't leave Secure stuck as ○.
            self.tls = if self.tls == StepState::Pending {
                StepState::Skipped
            } else {
                promote(self.tls)
            };
            self.ttfb = StepState::Active;
        } else if phase.contains("transfer") || phase.contains("content") {
            self.dns = promote(self.dns);
            self.tcp = promote(self.tcp);
            self.tls = if self.tls == StepState::Pending {
                StepState::Skipped
            } else {
                promote(self.tls)
            };
            self.ttfb = promote(self.ttfb);
            self.transfer = StepState::Active;
        }
    }
}

fn promote(s: StepState) -> StepState {
    match s {
        StepState::Pending => StepState::Pending,
        StepState::Active | StepState::Done => StepState::Done,
        StepState::Skipped => StepState::Skipped,
    }
}

fn default_status(advanced: bool) -> String {
    if advanced {
        "q quit · ? help · g guide · b compare · e export · t theme · m simple".into()
    } else {
        "q quit · ? help · g guide · b compare · e export · t theme · m details".into()
    }
}

pub fn render_json(
    run: &StoredRun,
    diff: Option<&DiffReport>,
) -> Result<String, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        run: &'a StoredRun,
        #[serde(skip_serializing_if = "Option::is_none")]
        diff: Option<&'a DiffReport>,
    }
    serde_json::to_string_pretty(&Out { run, diff })
}

pub fn render_text(run: &StoredRun, diff: Option<&DiffReport>) -> String {
    let mut out = String::new();
    let view = AppView::from_run(run, diff.cloned(), None);
    let (kind, title, detail) = verdict_of(&view);
    let icon = match kind {
        VerdictKind::Ok => "OK",
        VerdictKind::Warn => "WARN",
        VerdictKind::Fail => "FAIL",
    };
    out.push_str(&format!("result:     [{icon}] {title} — {detail}\n"));
    out.push_str(&format!("target:     {}\n", run.target));
    out.push_str(&format!("run_id:     {}\n", run.id));
    if let Some(ip) = &run.resolved_ip {
        out.push_str(&format!("resolved:   {ip}\n"));
    }
    if let Some(meta) = &run.meta {
        out.push_str(&format!(
            "meta:       {} {}/{} privileges={:?} mode={:?}\n",
            meta.tool_version, meta.os, meta.arch, meta.privileges, meta.probe_mode
        ));
        out.push_str(&format!("timing:     {}\n", meta.timing_basis));
    }
    if let Some(l7) = &run.l7 {
        out.push_str("\n--- Request journey ---\n");
        out.push_str(&format!("  1 Find (DNS):      {:>8}\n", fmt_ms(l7.dns_ms)));
        out.push_str(&format!("  2 Connect (TCP):   {:>8}\n", fmt_ms(l7.tcp_ms)));
        out.push_str(&format!("  3 Secure (TLS):    {:>8}\n", fmt_ms(l7.tls_ms)));
        out.push_str(&format!("  4 Wait (TTFB):     {:>8}\n", fmt_ms(l7.ttfb_ms)));
        out.push_str(&format!(
            "  5 Download (Body): {:>8}\n",
            fmt_ms(l7.transfer_ms)
        ));
        out.push_str(&format!("  Total:             {:>8.2} ms\n", l7.total_ms));
        if let Some(s) = l7.status {
            out.push_str(&format!("  Status:            {s}\n"));
        }
    }
    if let Some(tr) = &run.trace {
        out.push_str("\n--- Path to server ---\n");
        let s = &tr.summary;
        out.push_str(&format!(
            "  mode={}  hops={}  live={}  silent={}  gaps={}  raw_icmp={}\n",
            tr.probe_kind.label(),
            s.hop_count,
            s.replied,
            s.silent,
            s.gaps.len(),
            s.raw_icmp_ok
        ));
        if let Some(ttl) = s.min_ttl_tcp_reach {
            out.push_str(&format!("  tcp_reach_ttl={ttl}\n"));
        }
        if let Some(asn) = s.dest_asn {
            out.push_str(&format!(
                "  dest_as={} {}\n",
                asn,
                s.dest_as_name.as_deref().unwrap_or("")
            ));
        }
        out.push_str(&format!(
            "{:<4} {:<18} {:<6} {:>8}  {}\n",
            "Hop", "Address", "Via", "RTT", "Name / AS"
        ));
        let last = tr.hops.len().saturating_sub(1);
        for (i, hop) in tr.hops.iter().enumerate() {
            let reached = hop.address.is_some() && hop.metrics.recv > 0;
            let via =
                hop.reply_proto
                    .map(|p| p.label())
                    .unwrap_or(if reached { "?" } else { "—" });
            let mut meta = String::new();
            if let Some(h) = &hop.hostname {
                meta.push_str(h);
            }
            if let Some(asn) = hop.asn {
                if !meta.is_empty() {
                    meta.push_str(" · ");
                }
                meta.push_str(&format!("AS{asn}"));
                if let Some(n) = &hop.as_name {
                    meta.push(' ');
                    meta.push_str(n);
                }
            }
            if meta.is_empty() {
                meta = if i == last && tr.reached {
                    "destination".into()
                } else if !reached {
                    "no reply".into()
                } else {
                    String::new()
                };
            }
            out.push_str(&format!(
                "{:<4} {:<18} {:<6} {:>8}  {}\n",
                hop.ttl,
                hop.address
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "*".into()),
                via,
                if reached {
                    fmt_ms(hop.metrics.p50_ms)
                } else {
                    "-".into()
                },
                meta,
            ));
        }
    }
    if let Some(d) = diff {
        out.push_str("\n--- Compare ---\n");
        if d.regressions.is_empty() {
            out.push_str("  same or better than baseline\n");
        } else {
            for r in &d.regressions {
                out.push_str(&format!(
                    "  [{:?}] {}: {}\n",
                    r.severity, r.metric, r.message
                ));
            }
        }
    }
    out
}

fn fmt_ms(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.2}ms"),
        None => "-".into(),
    }
}

/// Run interactive TUI for a completed probe.
pub fn run_tui(view: AppView, theme: Theme, db: Option<&Path>) -> io::Result<Option<PathBuf>> {
    let store = Store::open(db).ok();
    let baselines = store
        .as_ref()
        .and_then(|s| s.list_baselines().ok())
        .unwrap_or_default();
    let mut app = App {
        view,
        theme,
        overlay: Overlay::Guide,
        baselines,
        baseline_idx: 0,
        status: default_status(false),
        progress_log: Vec::new(),
        probing: false,
        store,
        last_export: None,
        show_advanced: false,
        tick: 0,
        progress_steps: ProgressSteps::default(),
    };
    run_app_loop(&mut app)
}

/// Live progress TUI while probes run; then switches to results.
pub async fn run_tui_with_progress(
    mut progress_rx: UnboundedReceiver<ProgressEvent>,
    result_rx: tokio::sync::oneshot::Receiver<RunProbeResult>,
    theme: Theme,
    target: String,
    db: Option<&Path>,
) -> io::Result<RunProbeResult> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let store = Store::open(db).ok();
    let baselines = store
        .as_ref()
        .and_then(|s| s.list_baselines().ok())
        .unwrap_or_default();

    let mut app = App {
        view: AppView {
            target,
            resolved: None,
            baseline: None,
            trace: None,
            l7: None,
            diff: None,
            meta: None,
            run_id: None,
        },
        theme,
        overlay: Overlay::None,
        baselines,
        baseline_idx: 0,
        status: "Measuring… results appear when the probe finishes".into(),
        progress_log: vec!["Starting…".into()],
        probing: true,
        store,
        last_export: None,
        show_advanced: false,
        tick: 0,
        progress_steps: ProgressSteps::default(),
    };

    let mut result_rx = result_rx;
    let mut final_result: Option<RunProbeResult> = None;

    let loop_result = (|| -> io::Result<()> {
        loop {
            app.tick = app.tick.wrapping_add(1);
            while let Ok(ev) = progress_rx.try_recv() {
                app.progress_steps.apply(&ev);
                app.progress_log.push(ev.label());
                if app.progress_log.len() > 12 {
                    app.progress_log.remove(0);
                }
            }

            if final_result.is_none() {
                match result_rx.try_recv() {
                    Ok(res) => {
                        app.probing = false;
                        match &res {
                            Ok((run, diff, tagged)) => {
                                app.view = AppView::from_run(run, diff.clone(), tagged.clone());
                                if let Some(store) = &app.store {
                                    app.baselines = store.list_baselines().unwrap_or_default();
                                }
                                app.status = if tagged.is_some() {
                                    format!(
                                        "Saved as baseline '{}' · {}",
                                        tagged.as_deref().unwrap_or(""),
                                        default_status(app.show_advanced)
                                    )
                                } else {
                                    default_status(app.show_advanced)
                                };
                                if tagged.is_none() && app.view.diff.is_none() {
                                    app.overlay = Overlay::Guide;
                                }
                            }
                            Err(e) => {
                                app.status = format!("probe failed: {e}");
                            }
                        }
                        final_result = Some(res);
                    }
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                        if final_result.is_none() {
                            final_result = Some(Err(crate::error::Error::Other(
                                "probe task ended unexpectedly".into(),
                            )));
                            app.probing = false;
                        }
                    }
                }
            }

            terminal.draw(|f| draw(f, &app))?;

            if event::poll(Duration::from_millis(80))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if handle_key(&mut app, key.code)? {
                            break;
                        }
                    }
                    Event::Resize(_, _) => {
                        // redraw on next loop
                    }
                    _ => {}
                }
            }

            // Allow quit only after probe finishes (or on Esc during progress keeps UI until done)
            if !app.probing && final_result.is_some() {
                // keep looping until user quits via handle_key
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    loop_result?;
    Ok(final_result.unwrap_or(Err(crate::error::Error::Other("no result".into()))))
}

fn run_app_loop(app: &mut App) -> io::Result<Option<PathBuf>> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let result = (|| -> io::Result<()> {
        loop {
            app.tick = app.tick.wrapping_add(1);
            terminal.draw(|f| draw(f, app))?;
            if event::poll(Duration::from_millis(200))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if handle_key(app, key.code)? {
                            break;
                        }
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    result?;
    Ok(app.last_export.clone())
}

fn handle_key(app: &mut App, code: KeyCode) -> io::Result<bool> {
    match app.overlay {
        Overlay::Help | Overlay::Guide => {
            if matches!(
                code,
                KeyCode::Esc
                    | KeyCode::Char('?')
                    | KeyCode::Char('g')
                    | KeyCode::Enter
                    | KeyCode::Char(' ')
            ) {
                app.overlay = Overlay::None;
            }
            if matches!(code, KeyCode::Char('q')) && matches!(app.overlay, Overlay::Help) {
                app.overlay = Overlay::None;
            }
            return Ok(false);
        }
        Overlay::BaselinePicker => {
            match code {
                KeyCode::Esc | KeyCode::Char('b') => app.overlay = Overlay::None,
                KeyCode::Up | KeyCode::Char('k') => {
                    if app.baseline_idx > 0 {
                        app.baseline_idx -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if !app.baselines.is_empty() {
                        app.baseline_idx = (app.baseline_idx + 1).min(app.baselines.len() - 1);
                    }
                }
                KeyCode::Enter => {
                    apply_baseline(app);
                    app.overlay = Overlay::None;
                }
                _ => {}
            }
            return Ok(false);
        }
        Overlay::None => {}
    }

    match code {
        KeyCode::Char('q') | KeyCode::Esc => {
            if app.probing {
                app.status = "Finish measuring first, then press q".into();
                Ok(false)
            } else {
                Ok(true)
            }
        }
        KeyCode::Char('?') => {
            app.overlay = Overlay::Help;
            Ok(false)
        }
        KeyCode::Char('g') => {
            app.overlay = Overlay::Guide;
            Ok(false)
        }
        KeyCode::Char('m') => {
            app.show_advanced = !app.show_advanced;
            app.status = default_status(app.show_advanced);
            Ok(false)
        }
        KeyCode::Char('b') => {
            if let Some(store) = &app.store {
                app.baselines = store.list_baselines().unwrap_or_default();
            }
            if app.baselines.is_empty() {
                app.status = "No baselines yet — re-run with --save-baseline NAME".into();
            } else {
                if let Some(name) = &app.view.baseline {
                    if let Some(idx) = app.baselines.iter().position(|b| &b.name == name) {
                        app.baseline_idx = idx;
                    }
                }
                app.overlay = Overlay::BaselinePicker;
            }
            Ok(false)
        }
        KeyCode::Char('t') => {
            app.theme = app.theme.cycle();
            app.status = format!(
                "Theme: {:?} · {}",
                app.theme.name,
                default_status(app.show_advanced)
            );
            Ok(false)
        }
        KeyCode::Char('e') => {
            match export_report(app) {
                Ok(path) => {
                    app.last_export = Some(path.clone());
                    app.status = format!("Exported {}", path.display());
                }
                Err(e) => app.status = format!("Export failed: {e}"),
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn apply_baseline(app: &mut App) {
    let Some(info) = app.baselines.get(app.baseline_idx).cloned() else {
        return;
    };
    let Some(store) = &app.store else {
        app.status = "no database available".into();
        return;
    };
    let Ok(baseline) = store.get_baseline(&info.name) else {
        app.status = format!("failed to load baseline {}", info.name);
        return;
    };
    let Some(run_id) = app.view.run_id.clone() else {
        // Build a synthetic current run from view
        let current = StoredRun {
            id: "current".into(),
            target: app.view.target.clone(),
            created_at: chrono::Utc::now(),
            resolved_ip: app.view.resolved.clone(),
            reached: app.view.trace.as_ref().map(|t| t.reached).unwrap_or(false),
            trace: app.view.trace.clone(),
            l7: app.view.l7.clone(),
            meta: app.view.meta.clone(),
        };
        let report = diff_runs(
            &baseline,
            &current,
            Some(info.name.clone()),
            &DiffThresholds::default(),
        );
        app.view.baseline = Some(info.name.clone());
        app.view.diff = Some(report);
        app.status = format!("diff vs {}", info.name);
        return;
    };
    match store.get_run(&run_id) {
        Ok(current) => {
            let report = diff_runs(
                &baseline,
                &current,
                Some(info.name.clone()),
                &DiffThresholds::default(),
            );
            app.view.baseline = Some(info.name.clone());
            app.view.diff = Some(report);
            app.status = format!("diff vs {}", info.name);
        }
        Err(e) => app.status = format!("load run failed: {e}"),
    }
}

fn export_report(app: &App) -> io::Result<PathBuf> {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = std::env::temp_dir().join(format!("trace-diff-report-{ts}.json"));
    let run = StoredRun {
        id: app.view.run_id.clone().unwrap_or_else(|| "export".into()),
        target: app.view.target.clone(),
        created_at: chrono::Utc::now(),
        resolved_ip: app.view.resolved.clone(),
        reached: app.view.trace.as_ref().map(|t| t.reached).unwrap_or(false),
        trace: app.view.trace.clone(),
        l7: app.view.l7.clone(),
        meta: app.view.meta.clone(),
    };
    let json = render_json(&run, app.view.diff.as_ref()).map_err(io::Error::other)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

fn draw(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    if app.probing {
        draw_progress(frame, area, app);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // verdict
                Constraint::Length(11), // journey
                Constraint::Min(8),     // path map + hops
                Constraint::Length(4),  // compare
                Constraint::Length(1),  // status
            ])
            .split(area);
        draw_verdict(frame, chunks[0], app);
        draw_journey(frame, chunks[1], app);
        draw_hops(frame, chunks[2], app);
        draw_diff(frame, chunks[3], app);
        draw_status(frame, chunks[4], app);
    }

    match app.overlay {
        Overlay::Help => draw_help(frame, area, app),
        Overlay::Guide => draw_guide(frame, area, app),
        Overlay::BaselinePicker => draw_baseline_picker(frame, area, app),
        Overlay::None => {}
    }
}

#[derive(Clone, Copy)]
enum VerdictKind {
    Ok,
    Warn,
    Fail,
}

fn verdict_of(view: &AppView) -> (VerdictKind, String, String) {
    let status = view.l7.as_ref().and_then(|l| l.status);
    let ttfb = view.l7.as_ref().and_then(|l| l.ttfb_ms).unwrap_or(0.0);
    let total = view.l7.as_ref().map(|l| l.total_ms).unwrap_or(0.0);

    if let Some(code) = status {
        if code >= 500 {
            return (
                VerdictKind::Fail,
                "SERVER ERROR".into(),
                format!("HTTP {code} — the API/server failed"),
            );
        }
        if code >= 400 {
            return (
                VerdictKind::Fail,
                "ROUTE PROBLEM".into(),
                format!("HTTP {code} — endpoint missing or unauthorized (timing still measured)"),
            );
        }
    }

    if let Some(d) = &view.diff {
        if d.regressions
            .iter()
            .any(|r| matches!(r.severity, Severity::Critical))
        {
            return (
                VerdictKind::Fail,
                "REGRESSION".into(),
                "Much slower than your baseline — see Compare below".into(),
            );
        }
        if d.regressions
            .iter()
            .any(|r| matches!(r.severity, Severity::Warn))
        {
            return (
                VerdictKind::Warn,
                "SLOWER".into(),
                "Somewhat slower than baseline — see Compare below".into(),
            );
        }
        if view.l7.is_some() {
            return (
                VerdictKind::Ok,
                "LOOKS GOOD".into(),
                format!("Healthy vs baseline · total {total:.0} ms"),
            );
        }
    }

    if ttfb > 500.0 {
        return (
            VerdictKind::Warn,
            "SLOW RESPONSE".into(),
            format!("API waited ~{ttfb:.0} ms before first byte (TTFB)"),
        );
    }
    if status == Some(200) || status.map(|s| (200..400).contains(&s)).unwrap_or(false) {
        return (
            VerdictKind::Ok,
            "HEALTHY".into(),
            format!("Reached OK in {total:.0} ms"),
        );
    }
    (
        VerdictKind::Ok,
        "DONE".into(),
        format!("Probe finished · total {total:.0} ms"),
    )
}

fn draw_verdict(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let (kind, title, detail) = verdict_of(&app.view);
    let color = match kind {
        VerdictKind::Ok => app.theme.ok,
        VerdictKind::Warn => app.theme.warn,
        VerdictKind::Fail => app.theme.critical,
    };
    let badge = match kind {
        VerdictKind::Ok => "●",
        VerdictKind::Warn => "▲",
        VerdictKind::Fail => "✖",
    };
    let baseline = app
        .view
        .baseline
        .as_deref()
        .map(|b| format!(" · baseline {b}"))
        .unwrap_or_default();
    let line = Line::from(vec![
        Span::styled(
            format!(" {badge} {title} "),
            Style::default()
                .fg(color)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
        Span::raw("  "),
        Span::styled(detail, Style::default().fg(app.theme.accent)),
        Span::styled(baseline, Style::default().fg(app.theme.muted)),
    ]);
    let p = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .title(" Result "),
    );
    frame.render_widget(p, area);
}

fn stage_heat(ms: f64) -> ratatui::style::Color {
    if ms < 50.0 {
        ratatui::style::Color::Green
    } else if ms < 150.0 {
        ratatui::style::Color::Cyan
    } else if ms < 400.0 {
        ratatui::style::Color::Yellow
    } else {
        ratatui::style::Color::Red
    }
}

fn draw_journey(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let target = &app.view.target;
    let ip = app.view.resolved.as_deref().unwrap_or("…");

    let Some(l7) = &app.view.l7 else {
        let p = Paragraph::new(vec![
            Line::from(format!("trace-diff  {target}  →  {ip}")),
            Line::from("No HTTP timing yet"),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Request journey "),
        );
        frame.render_widget(p, area);
        return;
    };

    // Keep plain + technical names together on every row.
    let stages = [
        (
            "1 Find (DNS)",
            "DNS",
            "Look up address",
            l7.dns_ms.unwrap_or(0.0),
        ),
        (
            "2 Connect (TCP)",
            "TCP",
            "Open socket",
            l7.tcp_ms.unwrap_or(0.0),
        ),
        (
            "3 Secure (TLS)",
            "TLS",
            "Encrypt / HTTPS",
            l7.tls_ms.unwrap_or(0.0),
        ),
        (
            "4 Wait (TTFB)",
            "TTFB",
            "Server first byte",
            l7.ttfb_ms.unwrap_or(0.0),
        ),
        (
            "5 Download (Body)",
            "Body",
            "Read response",
            l7.transfer_ms.unwrap_or(0.0),
        ),
    ];
    let total = l7.total_ms.max(1.0);
    let max_bar = 12usize;

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "trace-diff",
            Style::default()
                .fg(app.theme.brand)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(target.as_str(), Style::default().fg(app.theme.accent)),
        Span::raw("  →  "),
        Span::styled(ip, Style::default().fg(app.theme.ok)),
    ]));

    // Pipeline strip with technical names visible.
    let mut strip: Vec<Span> = Vec::new();
    for (i, (label, _tech, _, ms)) in stages.iter().enumerate() {
        if i > 0 {
            strip.push(Span::styled(" → ", Style::default().fg(app.theme.muted)));
        }
        strip.push(Span::styled(
            (*label).to_string(),
            Style::default()
                .fg(stage_heat(*ms))
                .add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(strip));

    let mut summary: Vec<Span> = vec![Span::styled(
        format!("total {total:.0} ms"),
        Style::default()
            .fg(app.theme.brand)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(code) = l7.status {
        let c = if code < 400 {
            app.theme.ok
        } else {
            app.theme.critical
        };
        summary.push(Span::raw("  "));
        summary.push(Span::styled(
            format!(" HTTP {code} "),
            Style::default()
                .fg(c)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ));
    }
    lines.push(Line::from(summary));

    for (label, _tech, hint, ms) in &stages {
        let pct = (*ms / total).clamp(0.0, 1.0);
        let fill = ((pct * max_bar as f64).round() as usize).max(if *ms > 0.0 { 1 } else { 0 });
        let bar = format!(
            "{}{}",
            "█".repeat(fill),
            "░".repeat(max_bar.saturating_sub(fill))
        );
        let heat = stage_heat(*ms);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label:<18}"),
                Style::default().fg(heat).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{bar} "), Style::default().fg(heat)),
            Span::styled(
                format!("{ms:>6.1} ms  "),
                Style::default().fg(app.theme.accent),
            ),
            Span::styled((*hint).to_string(), Style::default().fg(app.theme.muted)),
        ]));
    }

    if app.show_advanced {
        if let Some(meta) = &app.view.meta {
            lines.push(Line::from(Span::styled(
                format!(
                    "details: v{} {}/{} {:?} {:?}",
                    meta.tool_version, meta.os, meta.arch, meta.privileges, meta.probe_mode
                ),
                Style::default().fg(app.theme.muted),
            )));
        }
    }

    let p =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(
            " Request journey — Find(DNS) Connect(TCP) Secure(TLS) Wait(TTFB) Download(Body) ",
        ));
    frame.render_widget(p, area);
}

fn draw_progress(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);

    let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let s = spin[(app.tick as usize / 2) % spin.len()];

    let checklist = [
        ("1 Find (DNS)", app.progress_steps.dns),
        ("2 Connect (TCP)", app.progress_steps.tcp),
        ("3 Secure (TLS)", app.progress_steps.tls),
        ("4 Wait (TTFB)", app.progress_steps.ttfb),
        ("5 Download (Body)", app.progress_steps.transfer),
        ("Trace path (hops)", app.progress_steps.hops),
    ];
    let mut check_lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {s} Measuring "),
                Style::default()
                    .fg(app.theme.brand)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&app.view.target, Style::default().fg(app.theme.accent)),
        ]),
        Line::from(""),
    ];
    for (label, state) in checklist {
        let (mark, style) = match state {
            StepState::Done => ("✔", Style::default().fg(app.theme.ok)),
            StepState::Active => (
                s,
                Style::default()
                    .fg(app.theme.brand)
                    .add_modifier(Modifier::BOLD),
            ),
            StepState::Skipped => ("–", Style::default().fg(app.theme.muted)),
            StepState::Pending => ("○", Style::default().fg(app.theme.muted)),
        };
        check_lines.push(Line::from(Span::styled(
            format!("  {mark}  {label}"),
            style,
        )));
    }

    let head = Paragraph::new(check_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Live progress "),
    );
    frame.render_widget(head, chunks[0]);

    let log_lines: Vec<Line> = app
        .progress_log
        .iter()
        .rev()
        .take(8)
        .rev()
        .map(|l| {
            Line::from(Span::styled(
                format!("  · {l}"),
                Style::default().fg(app.theme.muted),
            ))
        })
        .collect();
    let log = Paragraph::new(log_lines)
        .block(Block::default().borders(Borders::ALL).title(" Activity "))
        .wrap(Wrap { trim: true });
    frame.render_widget(log, chunks[1]);
    draw_status(frame, chunks[2], app);
}

fn draw_hops(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let Some(tr) = app.view.trace.as_ref() else {
        let p = Paragraph::new("Path tracing skipped (omit --skip-trace to enable)").block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Path to server "),
        );
        frame.render_widget(p, area);
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(3)])
        .split(area);

    // --- Summary + visual path ribbon ---
    let s = &tr.summary;
    let protos = if s.protocols_used.is_empty() {
        "—".into()
    } else {
        s.protocols_used
            .iter()
            .map(|p| p.label())
            .collect::<Vec<_>>()
            .join("+")
    };
    let gaps = if s.gaps.is_empty() {
        "none".into()
    } else {
        s.gaps
            .iter()
            .map(|g| {
                if g.from_ttl == g.to_ttl {
                    format!("TTL{}", g.from_ttl)
                } else {
                    format!("TTL{}–{}", g.from_ttl, g.to_ttl)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut summary_spans = vec![
        Span::styled("▸ ", Style::default().fg(app.theme.brand)),
        Span::styled(
            format!("{} hops", s.hop_count),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  "),
        Span::styled(
            format!("{} live", s.replied),
            Style::default().fg(app.theme.ok),
        ),
        Span::raw("  ·  "),
        Span::styled(
            format!("{} silent", s.silent),
            Style::default().fg(app.theme.muted),
        ),
        Span::raw("  ·  "),
        Span::styled(
            format!("via {protos}"),
            Style::default().fg(app.theme.accent),
        ),
        Span::raw("  ·  "),
        Span::styled(
            format!("mode {}", tr.probe_kind.label()),
            Style::default().fg(app.theme.muted),
        ),
    ];
    if let Some(fc) = s.flow_count.filter(|n| *n > 1) {
        summary_spans.push(Span::raw("  ·  "));
        summary_spans.push(Span::styled(
            format!("{fc} flows"),
            Style::default().fg(app.theme.brand),
        ));
        if !s.divergent_ttls.is_empty() {
            summary_spans.push(Span::raw("  ·  "));
            summary_spans.push(Span::styled(
                format!("ECMP≠{:?}", s.divergent_ttls),
                Style::default().fg(app.theme.warn),
            ));
        }
    }
    if let Some(ttl) = s.min_ttl_tcp_reach {
        summary_spans.push(Span::raw("  ·  "));
        summary_spans.push(Span::styled(
            format!("TCP≥TTL{ttl}"),
            Style::default().fg(app.theme.brand),
        ));
    }
    if !s.raw_icmp_ok {
        summary_spans.push(Span::raw("  ·  "));
        summary_spans.push(Span::styled(
            "run as Admin for TCP/UDP hop fill",
            Style::default().fg(app.theme.warn),
        ));
    }

    let ribbon = build_path_ribbon(tr, app);
    let as_line = if let Some(asn) = s.dest_asn {
        Line::from(vec![
            Span::styled("dest ", Style::default().fg(app.theme.muted)),
            Span::styled(
                format!("AS{asn}"),
                Style::default()
                    .fg(app.theme.ok)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                s.dest_as_name.as_deref().unwrap_or(""),
                Style::default().fg(app.theme.accent),
            ),
            Span::raw("   gaps: "),
            Span::styled(gaps, Style::default().fg(app.theme.warn)),
        ])
    } else {
        Line::from(vec![
            Span::raw("gaps: "),
            Span::styled(gaps, Style::default().fg(app.theme.warn)),
        ])
    };

    let head = Paragraph::new(vec![Line::from(summary_spans), ribbon, as_line])
        .block(Block::default().borders(Borders::ALL).title(" Path map "));
    frame.render_widget(head, chunks[0]);

    // --- Hop detail table ---
    let header = Row::new(["TTL", "Node", "Via", "RTT", "AS / name"])
        .style(Style::default().add_modifier(Modifier::BOLD));

    let last = tr.hops.len().saturating_sub(1);
    let rows: Vec<Row> = tr
        .hops
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let live = h.address.is_some() && h.metrics.recv > 0;
            let is_dest = i == last && tr.reached;
            let heat = h.metrics.p50_ms.map(stage_heat).unwrap_or(app.theme.muted);

            let marker = if is_dest {
                "●"
            } else if live {
                "◆"
            } else {
                "○"
            };
            let marker_style = if is_dest {
                Style::default()
                    .fg(app.theme.ok)
                    .add_modifier(Modifier::BOLD)
            } else if live {
                Style::default().fg(heat)
            } else {
                Style::default().fg(app.theme.muted)
            };

            let node = match h.address {
                Some(a) => format!("{marker} {a}"),
                None => format!("{marker} *"),
            };
            let via = h
                .reply_proto
                .map(|p| p.label().to_string())
                .unwrap_or_else(|| if live { "?".into() } else { "—".into() });
            let rtt = if live {
                fmt_ms(h.metrics.p50_ms)
            } else {
                "—".into()
            };

            let mut meta = String::new();
            if let Some(asn) = h.asn {
                meta.push_str(&format!("AS{asn}"));
            }
            if let Some(name) = &h.as_name {
                let short = name.split(',').next().unwrap_or(name);
                let short = if short.len() > 28 {
                    format!("{}…", &short[..27])
                } else {
                    short.to_string()
                };
                if !meta.is_empty() {
                    meta.push(' ');
                }
                meta.push_str(&short);
            } else if let Some(host) = &h.hostname {
                let short = if host.len() > 32 {
                    format!("{}…", &host[..31])
                } else {
                    host.clone()
                };
                meta.push_str(&short);
            } else if !live {
                meta.push_str("no Time Exceeded (ISP filter)");
            }

            Row::new(vec![
                Cell::from(h.ttl.to_string()),
                Cell::from(Span::styled(node, marker_style)),
                Cell::from(Span::styled(
                    via,
                    if live {
                        Style::default().fg(app.theme.accent)
                    } else {
                        Style::default().fg(app.theme.muted)
                    },
                )),
                Cell::from(Span::styled(rtt, Style::default().fg(heat))),
                Cell::from(Span::styled(meta, Style::default().fg(app.theme.muted))),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(20),
            Constraint::Length(5),
            Constraint::Length(9),
            Constraint::Min(16),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Hops (◆ reply  ○ silent  ● destination) "),
    );
    frame.render_widget(table, chunks[1]);
}

fn build_path_ribbon(tr: &TraceResult, app: &App) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        "you ",
        Style::default()
            .fg(app.theme.brand)
            .add_modifier(Modifier::BOLD),
    )];

    let last = tr.hops.len().saturating_sub(1);
    // Compress long paths: show all if ≤12, else first 4 + … + last 4
    let indices: Vec<usize> = if tr.hops.len() <= 12 {
        (0..tr.hops.len()).collect()
    } else {
        let mut v: Vec<_> = (0..4).chain(tr.hops.len() - 4..tr.hops.len()).collect();
        v.sort_unstable();
        v.dedup();
        v
    };

    let mut prev = None;
    for &i in &indices {
        if let Some(p) = prev {
            if i > p + 1 {
                spans.push(Span::styled("╌…╌", Style::default().fg(app.theme.muted)));
            } else {
                spans.push(Span::styled("─", Style::default().fg(app.theme.muted)));
            }
        }
        let h = &tr.hops[i];
        let live = h.address.is_some() && h.metrics.recv > 0;
        let is_dest = i == last && tr.reached;
        let (glyph, style) = if is_dest {
            (
                "●",
                Style::default()
                    .fg(app.theme.ok)
                    .add_modifier(Modifier::BOLD),
            )
        } else if live {
            let heat = h.metrics.p50_ms.map(stage_heat).unwrap_or(app.theme.brand);
            ("◆", Style::default().fg(heat))
        } else {
            ("○", Style::default().fg(app.theme.muted))
        };
        spans.push(Span::styled(glyph.to_string(), style));
        prev = Some(i);
    }

    spans.push(Span::styled(
        " dest",
        Style::default()
            .fg(app.theme.ok)
            .add_modifier(Modifier::BOLD),
    ));
    Line::from(spans)
}

fn draw_diff(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let lines: Vec<Line> = match (&app.view.diff, &app.view.baseline) {
        (None, Some(name)) => vec![
            Line::from(Span::styled(
                format!("Snapshot saved as “{name}”"),
                Style::default()
                    .fg(app.theme.ok)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Next: press b to compare, or re-run:  trace-diff diff NAME <url>",
                Style::default().fg(app.theme.muted),
            )),
        ],
        (None, None) => vec![
            Line::from(Span::styled(
                "No comparison yet",
                Style::default()
                    .fg(app.theme.warn)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("Press b to pick a saved baseline, or run with --save-baseline NAME"),
        ],
        (Some(d), _) if d.regressions.is_empty() => {
            let mut lines = vec![Line::from(Span::styled(
                "✓ Same or better than baseline",
                Style::default()
                    .fg(app.theme.ok)
                    .add_modifier(Modifier::BOLD),
            ))];
            if let Some(l7) = &d.l7 {
                if let Some(delta) = l7.ttfb_delta_pct {
                    let arrow = if delta < -1.0 {
                        "↓ faster"
                    } else if delta > 1.0 {
                        "↑ slower"
                    } else {
                        "→ same"
                    };
                    lines.push(Line::from(format!(
                        "  TTFB {arrow} ({delta:+.0}%)   total {:+.0}%",
                        l7.total_delta_pct.unwrap_or(0.0)
                    )));
                }
            }
            lines
        }
        (Some(d), _) => {
            let mut lines = vec![Line::from(Span::styled(
                "Changes vs baseline",
                Style::default()
                    .fg(app.theme.warn)
                    .add_modifier(Modifier::BOLD),
            ))];
            for r in &d.regressions {
                let (arrow, color) = match r.severity {
                    Severity::Critical => ("⬆⬆", app.theme.critical),
                    Severity::Warn => ("⬆", app.theme.warn),
                    Severity::Info => ("·", app.theme.brand),
                };
                let plain = r.metric.replace('_', " ");
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {arrow} {plain}: {}",
                        r.delta_pct
                            .map(|p| format!("{p:+.0}%"))
                            .unwrap_or_else(|| r.message.clone())
                    ),
                    Style::default().fg(color),
                )));
            }
            lines
        }
    };

    let p = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Compare "))
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn draw_status(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let p = Paragraph::new(app.status.as_str())
        .style(Style::default().fg(app.theme.muted))
        .alignment(Alignment::Left);
    frame.render_widget(p, area);
}

fn draw_guide(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(72, 70, area);
    frame.render_widget(Clear, popup);
    let text = vec![
        Line::from(Span::styled(
            " Quick guide — how to read this screen ",
            Style::default()
                .fg(app.theme.brand)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "1. Result",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("   Big badge: HEALTHY / SLOW / ROUTE PROBLEM / REGRESSION"),
        Line::from(""),
        Line::from(Span::styled(
            "2. Request journey",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("   Find(DNS) → Connect(TCP) → Secure(TLS) → Wait(TTFB) → Download(Body)"),
        Line::from("   Longer / warmer bar = that step used more time."),
        Line::from("   Wait (TTFB) is usually your API/server time."),
        Line::from(""),
        Line::from(Span::styled(
            "3. Path to server",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("   ◆ reply  ○ silent  ● destination · AS + name when known"),
        Line::from("   Auto probe: ICMP, then TCP/UDP fill (Admin helps on Windows)."),
        Line::from(""),
        Line::from(Span::styled(
            "4. Compare",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("   Press b to diff against a saved baseline."),
        Line::from(""),
        Line::from(Span::styled(
            " Enter / Space / g  dismiss   ·   ? full keys   ·   m toggle details ",
            Style::default().fg(app.theme.muted),
        )),
    ];
    let p = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.brand))
                .title(" Welcome "),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(p, popup);
}

fn draw_help(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(70, 65, area);
    frame.render_widget(Clear, popup);
    let text = vec![
        Line::from(Span::styled(
            "All features",
            Style::default()
                .fg(app.theme.brand)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("  q / Esc     quit"),
        Line::from("  g           welcome guide (visual tour)"),
        Line::from("  ?           this help"),
        Line::from("  b           pick baseline to compare"),
        Line::from("  ↑↓ / j k    move in baseline list"),
        Line::from("  Enter       apply baseline"),
        Line::from("  e           export JSON report"),
        Line::from("  t           cycle theme (default/ocean/amber/mono)"),
        Line::from("  m           show/hide advanced meta"),
        Line::from(""),
        Line::from("Colors: green=fast  yellow=moderate  red=slow/error"),
        Line::from("--no-color / NO_COLOR disables color."),
        Line::from(""),
        Line::from(Span::styled(
            "Tip: save a good run with --save-baseline NAME",
            Style::default().fg(app.theme.muted),
        )),
    ];
    let p = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" Help "))
        .wrap(Wrap { trim: false });
    frame.render_widget(p, popup);
}

fn draw_baseline_picker(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(64, 55, area);
    frame.render_widget(Clear, popup);
    let mut lines = vec![
        Line::from(Span::styled(
            "Compare against which snapshot?",
            Style::default()
                .fg(app.theme.brand)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "↑↓ move · Enter apply · Esc cancel",
            Style::default().fg(app.theme.muted),
        )),
        Line::from(""),
    ];
    for (i, b) in app.baselines.iter().enumerate() {
        let marker = if i == app.baseline_idx { "▶ " } else { "  " };
        let style = if i == app.baseline_idx {
            Style::default()
                .fg(app.theme.warn)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{:<20} {}", b.name, b.target),
            style,
        )));
    }
    let p =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Baselines "));
    frame.render_widget(p, popup);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

#[allow(dead_code)]
fn spark_str(samples: &[f64]) -> String {
    const BLOCKS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if samples.is_empty() {
        return String::new();
    }
    let min = samples.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = samples.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = (max - min).max(1e-9);
    samples
        .iter()
        .map(|v| {
            let idx = (((v - min) / span) * (BLOCKS.len() as f64 - 1.0)).round() as usize;
            BLOCKS[idx.min(BLOCKS.len() - 1)]
        })
        .collect()
}

/// Resolve theme from CLI/env for non-TUI callers.
pub fn theme_from_flags(name: ThemeName, no_color: bool, force_color: bool) -> Theme {
    Theme::resolve(name, no_color, force_color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn spark_nonempty() {
        let s = spark_str(&[1.0, 2.0, 3.0, 2.0]);
        assert_eq!(s.chars().count(), 4);
    }

    #[test]
    fn test_backend_renders() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App {
            view: AppView {
                target: "https://example.com".into(),
                resolved: Some("1.2.3.4".into()),
                baseline: None,
                trace: None,
                l7: None,
                diff: None,
                meta: None,
                run_id: None,
            },
            theme: Theme::named(ThemeName::Default),
            overlay: Overlay::None,
            baselines: vec![],
            baseline_idx: 0,
            status: "test".into(),
            progress_log: vec![],
            probing: false,
            store: None,
            last_export: None,
            show_advanced: false,
            tick: 0,
            progress_steps: ProgressSteps::default(),
        };
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let flat: String = buf
            .content()
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(flat.contains("trace-diff"));
    }

    #[test]
    fn resize_safe_progress() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App {
            view: AppView {
                target: "https://example.com".into(),
                resolved: None,
                baseline: None,
                trace: None,
                l7: None,
                diff: None,
                meta: None,
                run_id: None,
            },
            theme: Theme::named(ThemeName::Mono),
            overlay: Overlay::Help,
            baselines: vec![],
            baseline_idx: 0,
            status: "probing".into(),
            progress_log: vec!["DNS".into(), "TCP".into()],
            probing: true,
            store: None,
            last_export: None,
            show_advanced: false,
            tick: 0,
            progress_steps: ProgressSteps::default(),
        };
        terminal.draw(|f| draw(f, &app)).unwrap();
    }
}
