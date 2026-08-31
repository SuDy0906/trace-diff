//! Interactive feature discover → select → run → graphical scorecard.

use super::auth_spec::{build_realm_auth_specs, realm_spec_satisfied, RealmAuthSpec};
use super::workflow::{detect_auth_realms, workflow_realm};
use super::{
    build_categorized_report, discover_features, has_step_warnings, is_write_flow, AuthRealmHint,
    DetectedFeature, DiscoverOptions, DiscoverStage, FeatureKind,
    FeatureResult, FeatureRunReport, FlowKind, IssueCategory, LlmDiscoveryStatus, ProbeSettings,
    ProbeVerdict, WorkflowScenario,
};
use crate::ai::{AiConfig, AiResolution};
use crate::ai::llm_unavailable_hint;
use crate::error::{Error, Result};
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap};
use ratatui::Terminal;
use std::collections::HashMap;
use std::io::stdout;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

enum Phase {
    Discovering,
    Select,
    Running,
    Results,
}

enum FeaturesOverlay {
    None,
    Help,
    Guide,
    Llm,
}

#[derive(Clone)]
struct StoredDiscoverOptions {
    manifest: Option<PathBuf>,
    llm: Option<AiConfig>,
    ai_resolution: Option<AiResolution>,
    infer_workflows: bool,
    skip_tls_canary: bool,
}

impl StoredDiscoverOptions {
    fn capture(discover: DiscoverOptions<'_>) -> Self {
        Self {
            manifest: discover.manifest.map(|p| p.to_path_buf()),
            llm: discover.llm.clone(),
            ai_resolution: discover.ai_resolution.clone(),
            infer_workflows: discover.infer_workflows,
            skip_tls_canary: discover.skip_tls_canary,
        }
    }
}

#[derive(Debug, Clone)]
enum RowRunState {
    /// Not part of this run.
    Idle,
    /// Selected, waiting its turn.
    Queued,
    /// Currently probing.
    Running,
    Done(Box<FeatureResult>),
}

struct App {
    base: String,
    theme: Theme,
    phase: Phase,
    features: Vec<DetectedFeature>,
    selected: Vec<bool>,
    cursor: usize,
    run_states: Vec<RowRunState>,
    report: Option<FeatureRunReport>,
    status: String,
    error: Option<String>,
    scroll: usize,
    tick: u64,
    /// Visible table rows (updated each frame for page-up/down).
    viewport_rows: usize,
    /// Auto-scroll to the active probe row until the user scrolls manually.
    run_follow: bool,
    settings: ProbeSettings,
    inspect_open: bool,
    inspect_scroll: u16,
    report_open: bool,
    report_scroll: u16,
    cached_report_lines: Option<Vec<Line<'static>>>,
    report_viewport_rows: usize,
    auth_popup: Option<AuthPopupState>,
    overlay: FeaturesOverlay,
    last_export: Option<std::path::PathBuf>,
    stored_discover: StoredDiscoverOptions,
    llm_status: Option<LlmDiscoveryStatus>,
    confirm_quit: bool,
    auth_dismiss_confirm: bool,
}

#[derive(Debug, Clone)]
struct AuthPopupState {
    field_idx: usize,
    rows: Vec<AuthPopupRow>,
    specs: Vec<RealmAuthSpec>,
    values: HashMap<(AuthRealmHint, String), String>,
    include: HashMap<AuthRealmHint, bool>,
}

#[derive(Debug, Clone)]
enum AuthPopupRow {
    ToggleRealm {
        realm: AuthRealmHint,
    },
    Summary {
        text: String,
    },
    Note {
        text: String,
    },
    TextInput {
        realm: AuthRealmHint,
        key: String,
        label: String,
        secret: bool,
        required: bool,
        hint: String,
    },
}

pub async fn run_features_interactive(
    base: &str,
    theme: Theme,
    settings: ProbeSettings,
    discover: DiscoverOptions<'_>,
) -> Result<()> {
    if discover.infer_workflows {
        if let Some(res) = discover.ai_resolution.as_ref() {
            if let Some(hint) = llm_unavailable_hint(res) {
                eprintln!("{hint}");
            }
        }
    }

    let stored_discover = StoredDiscoverOptions::capture(discover);
    let mut terminal = setup()?;
    let mut app = App {
        base: base.to_string(),
        theme,
        phase: Phase::Discovering,
        features: Vec::new(),
        selected: Vec::new(),
        cursor: 0,
        report: None,
        status: if stored_discover.infer_workflows {
            "Starting discovery…".into()
        } else {
            "Discovering features…".into()
        },
        error: None,
        scroll: 0,
        run_states: Vec::new(),
        tick: 0,
        viewport_rows: 15,
        run_follow: true,
        settings,
        inspect_open: false,
        inspect_scroll: 0,
        report_open: false,
        report_scroll: 0,
        cached_report_lines: None,
        report_viewport_rows: 20,
        auth_popup: None,
        overlay: FeaturesOverlay::None,
        last_export: None,
        stored_discover: stored_discover.clone(),
        llm_status: None,
        confirm_quit: false,
        auth_dismiss_confirm: false,
    };

    run_discovery_pass(&mut terminal, &mut app, stored_discover).await?;

    loop {
        if event::poll(Duration::from_millis(100)).map_err(Error::Io)? {
            if let Event::Key(key) = event::read().map_err(Error::Io)? {
                if !key_event_active(&key, &app) {
                    continue;
                }
                if app.auth_popup.is_some() && handle_auth_popup(&mut app, key.code) {
                    continue;
                }
                if !matches!(app.overlay, FeaturesOverlay::None) {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('l')) {
                        app.overlay = FeaturesOverlay::None;
                        draw(&mut terminal, &mut app)?;
                    }
                    continue;
                }
                if app.inspect_open {
                    match key.code {
                        KeyCode::Esc
                        | KeyCode::Char('q')
                        | KeyCode::Char('d')
                        | KeyCode::Char('i') => {
                            app.inspect_open = false;
                            app.inspect_scroll = 0;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.inspect_scroll = app.inspect_scroll.saturating_sub(1);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.inspect_scroll = app.inspect_scroll.saturating_add(1);
                        }
                        KeyCode::PageUp => {
                            app.inspect_scroll = app.inspect_scroll.saturating_sub(8);
                        }
                        KeyCode::PageDown => {
                            app.inspect_scroll = app.inspect_scroll.saturating_add(8);
                        }
                        KeyCode::Home => app.inspect_scroll = 0,
                        _ => {}
                    }
                    continue;
                }
                if app.report_open {
                    let page = app.report_viewport_rows.max(4) as u16;
                    match key.code {
                        KeyCode::Esc
                        | KeyCode::Char('q')
                        | KeyCode::Char('r')
                        | KeyCode::Char('R') => {
                            app.report_open = false;
                            app.report_scroll = 0;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.report_scroll = app.report_scroll.saturating_sub(4);
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.report_scroll = app.report_scroll.saturating_add(4);
                        }
                        KeyCode::PageUp => {
                            app.report_scroll = app.report_scroll.saturating_sub(page);
                        }
                        KeyCode::PageDown => {
                            app.report_scroll = app.report_scroll.saturating_add(page);
                        }
                        KeyCode::Home => app.report_scroll = 0,
                        KeyCode::End => app.report_scroll = u16::MAX,
                        _ => {}
                    }
                    draw(&mut terminal, &mut app)?;
                    continue;
                }
                match app.phase {
                    Phase::Select => {
                        if matches!(key.code, KeyCode::Char('R')) {
                            let stored = app.stored_discover.clone();
                            run_discovery_pass(&mut terminal, &mut app, stored).await?;
                            continue;
                        }
                        if matches!(key.code, KeyCode::Char('l')) {
                            app.overlay = FeaturesOverlay::Llm;
                            draw(&mut terminal, &mut app)?;
                            continue;
                        }
                        if handle_select(&mut app, key.code)? {
                            break;
                        }
                        if matches!(key.code, KeyCode::Enter | KeyCode::Char('r')) {
                            let chosen: Vec<_> = app
                                .features
                                .iter()
                                .zip(app.selected.iter())
                                .filter_map(|(f, on)| if *on { Some(f.clone()) } else { None })
                                .collect();
                            if chosen.is_empty() {
                                app.status = "Select at least one feature (Space)".into();
                            } else {
                                let total = chosen.len();
                                app.run_states = app
                                    .features
                                    .iter()
                                    .enumerate()
                                    .map(|(i, _)| {
                                        if app.selected.get(i).copied().unwrap_or(false) {
                                            RowRunState::Queued
                                        } else {
                                            RowRunState::Idle
                                        }
                                    })
                                    .collect();
                                app.phase = Phase::Running;
                                app.scroll = 0;
                                app.run_follow = true;
                                app.confirm_quit = false;
                                if let Some(first) = app.selected.iter().position(|on| *on) {
                                    app.cursor = first;
                                }
                                app.status = format!("Running {total} features…");
                                draw(&mut terminal, &mut app)?;

                                let indices: Vec<usize> = app
                                    .features
                                    .iter()
                                    .enumerate()
                                    .filter(|(i, _)| app.selected[*i])
                                    .map(|(i, _)| i)
                                    .collect();

                                let mut results = Vec::with_capacity(indices.len());
                                let mut aborted = false;
                                for (done, idx) in indices.iter().enumerate() {
                                    app.run_states[*idx] = RowRunState::Running;
                                    app.cursor = *idx;
                                    app.status = format!(
                                        "Probing {} ({}/{total})…",
                                        app.features[*idx].label,
                                        done + 1
                                    );
                                    draw(&mut terminal, &mut app)?;

                                    let feature = app.features[*idx].clone();
                                    let settings = app.settings.clone();
                                    let handle = tokio::spawn(async move {
                                        super::probe_feature_with_auth(&feature, &settings).await
                                    });
                                    let Some(result) =
                                        wait_probe_with_ui(&mut terminal, &mut app, handle).await?
                                    else {
                                        aborted = true;
                                        break;
                                    };

                                    app.run_states[*idx] =
                                        RowRunState::Done(Box::new(result.clone()));
                                    results.push(result);
                                    draw(&mut terminal, &mut app)?;
                                }
                                if aborted {
                                    break;
                                }
                                let healthy = results
                                    .iter()
                                    .filter(|r| r.verdict == ProbeVerdict::Healthy)
                                    .count();
                                let reachable = results
                                    .iter()
                                    .filter(|r| r.verdict == ProbeVerdict::Reachable)
                                    .count();
                                let failed = results
                                    .iter()
                                    .filter(|r| r.verdict == ProbeVerdict::Failed)
                                    .count();
                                let passed = healthy + reachable;
                                app.report = Some(FeatureRunReport {
                                    base_url: app.base.clone(),
                                    discovered: app.features.len(),
                                    selected: total,
                                    passed,
                                    failed,
                                    results,
                                });
                                app.run_states.clear();
                                app.phase = Phase::Results;
                                app.cursor = 0;
                                app.scroll = 0;
                                app.status = format!(
                                    "{healthy} ok · {reachable} reachable · {failed} failed · ↑↓ row · Enter/d inspect · R report · q quit · b back"
                                );
                                refresh_report_cache(&mut app);
                            }
                        }
                    }
                    Phase::Results => {
                        let last = app
                            .report
                            .as_ref()
                            .map(|r| r.results.len().saturating_sub(1))
                            .unwrap_or(0);
                        let page = app.viewport_rows.max(1);
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char('?') => app.overlay = FeaturesOverlay::Help,
                            KeyCode::Char('g') => app.overlay = FeaturesOverlay::Guide,
                            KeyCode::Char('t') => app.theme = app.theme.cycle(),
                            KeyCode::Char('e') => {
                                if let Err(e) = export_features_report(&mut app) {
                                    app.error = Some(e.to_string());
                                }
                            }
                            KeyCode::Char('b') => {
                                app.inspect_open = false;
                                app.report_open = false;
                                app.phase = Phase::Select;
                                app.report = None;
                                app.scroll = 0;
                                app.cursor = 0;
                                app.status = "↑↓ move · Space toggle · Enter run · q quit".into();
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                app.cursor = (app.cursor + 1).min(last);
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                app.cursor = app.cursor.saturating_sub(1);
                            }
                            KeyCode::PageDown => {
                                app.cursor = (app.cursor + page).min(last);
                            }
                            KeyCode::PageUp => {
                                app.cursor = app.cursor.saturating_sub(page);
                            }
                            KeyCode::Home => app.cursor = 0,
                            KeyCode::End => app.cursor = last,
                            KeyCode::Enter | KeyCode::Char('d') | KeyCode::Char('i') => {
                                app.report_open = false;
                                app.inspect_open = true;
                                app.inspect_scroll = 0;
                            }
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                app.inspect_open = false;
                                app.report_open = true;
                                app.report_scroll = 0;
                                refresh_report_cache(&mut app);
                            }
                            _ => {}
                        }
                    }
                    Phase::Running => {
                        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                            if app.confirm_quit {
                                break;
                            }
                            app.confirm_quit = true;
                            app.status = format!("{} — press q again to abort", app.status);
                        } else {
                            scroll_list_by(&mut app, key.code);
                        }
                    }
                    Phase::Discovering => {
                        if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                            break;
                        }
                    }
                }
            }
        }
        if matches!(app.phase, Phase::Running) {
            app.tick = app.tick.wrapping_add(1);
        }
        draw(&mut terminal, &mut app)?;
    }

    teardown(terminal)?;
    if let Some(rep) = &app.report {
        // Also print a short text summary after leaving TUI.
        println!(
            "\nFeature run: {}/{} passed (discovered {})",
            rep.passed, rep.selected, rep.discovered
        );
        let categorized = build_categorized_report(rep);
        if categorized.issue_count > 0 {
            println!(
                "  Findings: {} clean · {} issue(s)",
                categorized.clean_features, categorized.issue_count
            );
            for category in IssueCategory::all() {
                let Some(items) = categorized.issues.get(&category) else {
                    continue;
                };
                if items.is_empty() {
                    continue;
                }
                println!(
                    "  [{}] {} — {}",
                    items.len(),
                    category.label(),
                    category.description()
                );
                for item in items.iter().take(8) {
                    let step = item
                        .step_name
                        .as_deref()
                        .map(|s| format!(" · {s}"))
                        .unwrap_or_default();
                    println!("      • {}{}: {}", item.feature_label, step, item.detail);
                }
                if items.len() > 8 {
                    println!("      … and {} more", items.len() - 8);
                }
            }
        }
        for r in &rep.results {
            let mark = if r.ok { "PASS" } else { "FAIL" };
            println!(
                "  [{mark}] {:<20} {:>7.0} ms  {}  {}",
                r.feature.label, r.total_ms, r.message, r.feature.url
            );
        }
    }
    Ok(())
}

async fn run_discovery_pass(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    stored: StoredDiscoverOptions,
) -> Result<()> {
    app.phase = Phase::Discovering;
    app.error = None;
    app.confirm_quit = false;
    app.status = if stored.infer_workflows {
        "Starting discovery…".into()
    } else {
        "Discovering features…".into()
    };
    draw(terminal, app)?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<DiscoverStage>();
    let progress = Arc::new(move |stage: DiscoverStage| {
        let _ = tx.send(stage);
    });
    let base = app.base.clone();
    let manifest = stored.manifest.clone();
    let llm = stored.llm.clone();
    let ai_resolution = stored.ai_resolution.clone();
    let infer_workflows = stored.infer_workflows;
    let skip_tls_canary = stored.skip_tls_canary;
    let handle = tokio::spawn(async move {
        let opts = DiscoverOptions {
            manifest: manifest.as_deref(),
            llm,
            ai_resolution,
            infer_workflows,
            skip_tls_canary,
            on_progress: Some(progress),
        };
        discover_features(&base, opts).await
    });

    loop {
        while let Ok(stage) = rx.try_recv() {
            app.status = stage.label().to_string();
        }
        if handle.is_finished() {
            break;
        }
        if event::poll(Duration::from_millis(50)).map_err(Error::Io)? {
            if let Event::Key(key) = event::read().map_err(Error::Io)? {
                if key_event_active(&key, app)
                    && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    handle.abort();
                    app.phase = Phase::Select;
                    app.status = "Discovery cancelled".into();
                    draw(terminal, app)?;
                    return Ok(());
                }
            }
        }
        app.tick = app.tick.wrapping_add(1);
        draw(terminal, app)?;
    }

    match handle.await {
        Ok(Ok(outcome)) => apply_discovery_outcome(app, outcome),
        Ok(Err(e)) => apply_discovery_error(app, e.to_string()),
        Err(e) => apply_discovery_error(app, format!("discovery task failed: {e}")),
    }
    draw(terminal, app)?;
    Ok(())
}

fn apply_discovery_outcome(app: &mut App, outcome: super::DiscoverOutcome) {
    let feats = outcome.features;
    let n = feats.len();
    let workflows = feats
        .iter()
        .filter(|f| f.kind == FeatureKind::Workflow)
        .count();
    app.llm_status = Some(outcome.llm.clone());
    app.selected = feats.iter().map(default_selected).collect();
    app.features = feats;
    app.cursor = 0;
    app.scroll = 0;
    maybe_auto_open_auth_popup(app);
    app.phase = Phase::Select;
    if n == 0 {
        app.status = "No features found — R retry · try --manifest or --no-llm · q quit".into();
    } else {
        app.status = discovery_status_line(workflows, n, &outcome.llm);
    }
}

fn apply_discovery_error(app: &mut App, message: String) {
    app.error = Some(message);
    app.llm_status = None;
    app.features.clear();
    app.selected.clear();
    app.phase = Phase::Select;
    app.status =
        "Discovery failed — R retry · try --manifest or --no-llm · l LLM status · q quit".into();
}

fn draw_discovering(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let spin = ['|', '/', '-', '\\'][(app.tick as usize / 8) % 4];
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!(" {spin} "), Style::default().fg(app.theme.brand)),
            Span::raw(app.status.as_str()),
        ]),
        Line::from(""),
        Line::from("  q cancel discovery"),
    ];
    if let Some(res) = app.stored_discover.ai_resolution.as_ref() {
        if !res.is_ready() {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                "  LLM unavailable — heuristics will be used (l for details)",
                Style::default().fg(app.theme.muted),
            ));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Discovering "),
        ),
        area,
    );
}

fn max_list_scroll(total: usize, viewport_rows: usize) -> usize {
    total.saturating_sub(viewport_rows.max(1))
}

fn scroll_list_by(app: &mut App, code: KeyCode) -> bool {
    if app.features.is_empty() {
        return false;
    }
    let max_scroll = max_list_scroll(app.features.len(), app.viewport_rows);
    let page = app.viewport_rows.max(1);
    let scrolled = match code {
        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll = (app.scroll + 1).min(max_scroll);
            true
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll = app.scroll.saturating_sub(1);
            true
        }
        KeyCode::PageDown => {
            app.scroll = (app.scroll + page).min(max_scroll);
            true
        }
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_sub(page);
            true
        }
        KeyCode::Home => {
            app.scroll = 0;
            true
        }
        KeyCode::End => {
            app.scroll = max_scroll;
            true
        }
        _ => false,
    };
    if scrolled {
        app.run_follow = false;
    }
    scrolled
}

async fn wait_probe_with_ui(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    handle: tokio::task::JoinHandle<FeatureResult>,
) -> Result<Option<FeatureResult>> {
    while !handle.is_finished() {
        if event::poll(Duration::from_millis(50)).map_err(Error::Io)? {
            if let Event::Key(key) = event::read().map_err(Error::Io)? {
                if key.kind == KeyEventKind::Press {
                    if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                        if app.confirm_quit {
                            handle.abort();
                            return Ok(None);
                        }
                        app.confirm_quit = true;
                        app.status = format!("{} — press q again to abort", app.status);
                    } else {
                        app.confirm_quit = false;
                        scroll_list_by(app, key.code);
                    }
                }
            }
        }
        app.tick = app.tick.wrapping_add(1);
        draw(terminal, app)?;
    }
    handle
        .await
        .map_err(|e| Error::Other(format!("probe task failed: {e}")))
        .map(Some)
}

fn handle_select(app: &mut App, code: KeyCode) -> Result<bool> {
    let last = app.features.len().saturating_sub(1);
    let page = app.viewport_rows.max(1);
    match code {
        KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
        KeyCode::Up | KeyCode::Char('k') => {
            app.cursor = app.cursor.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if !app.features.is_empty() {
                app.cursor = (app.cursor + 1).min(last);
            }
        }
        KeyCode::PageUp => {
            app.cursor = app.cursor.saturating_sub(page);
        }
        KeyCode::PageDown => {
            if !app.features.is_empty() {
                app.cursor = (app.cursor + page).min(last);
            }
        }
        KeyCode::Home => {
            app.cursor = 0;
        }
        KeyCode::End => {
            app.cursor = last;
        }
        KeyCode::Char(' ') => {
            if let Some(s) = app.selected.get_mut(app.cursor) {
                *s = !*s;
            }
        }
        KeyCode::Char('a') => {
            app.selected.fill(true);
        }
        KeyCode::Char('c') => {
            open_auth_popup(app);
        }
        KeyCode::Char('?') => app.overlay = FeaturesOverlay::Help,
        KeyCode::Char('g') => app.overlay = FeaturesOverlay::Guide,
        KeyCode::Char('l') => app.overlay = FeaturesOverlay::Llm,
        KeyCode::Char('t') => app.theme = app.theme.cycle(),
        KeyCode::Char('n') => {
            app.selected.fill(false);
        }
        KeyCode::Char('d') | KeyCode::Char('i') => {
            app.inspect_open = true;
            app.inspect_scroll = 0;
        }
        _ => {}
    }
    Ok(false)
}

fn setup() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode().map_err(Error::Io)?;
    stdout().execute(EnterAlternateScreen).map_err(Error::Io)?;
    let backend = CrosstermBackend::new(stdout());
    Terminal::new(backend).map_err(Error::Io)
}

fn teardown(mut terminal: Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    disable_raw_mode().map_err(Error::Io)?;
    terminal
        .backend_mut()
        .execute(LeaveAlternateScreen)
        .map_err(Error::Io)?;
    terminal.show_cursor().map_err(Error::Io)?;
    Ok(())
}

fn draw(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App) -> Result<()> {
    terminal
        .draw(|frame| {
            let area = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(area);

            let title = Paragraph::new(Line::from(vec![
                Span::styled(
                    " trace-diff features ",
                    Style::default()
                        .fg(app.theme.brand)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(app.base.as_str(), Style::default().fg(app.theme.accent)),
            ]))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Auto-detect "),
            );
            frame.render_widget(title, chunks[0]);

            let phase_kind = match &app.phase {
                Phase::Discovering => 0u8,
                Phase::Select => 1,
                Phase::Running => 2,
                Phase::Results => 3,
            };
            match phase_kind {
                0 => draw_discovering(frame, chunks[1], app),
                1 => draw_select_with_detail(frame, chunks[1], app),
                2 => draw_running_with_detail(frame, chunks[1], app),
                _ => draw_results_with_detail(frame, chunks[1], chunks[2], app),
            }

            if !matches!(app.phase, Phase::Results) {
                draw_legend(frame, chunks[2], app);
            }

            let err = app.error.as_deref().unwrap_or("");
            let status = if err.is_empty() {
                app.status.clone()
            } else {
                format!("{} | {}", app.status, err)
            };
            frame.render_widget(
                Paragraph::new(status).style(Style::default().fg(app.theme.muted)),
                chunks[3],
            );

            if app.inspect_open || app.auth_popup.is_some() || app.report_open {
                let popup = active_popup_rect(area, app);
                // Dim only the main list — leave title, score bar, and status legible.
                dim_background(frame, chunks[1], popup, &app.theme);
            }
            if app.report_open {
                draw_report_overlay(frame, area, app);
            } else if app.inspect_open {
                draw_inspect_overlay(frame, area, app);
            } else if app.auth_popup.is_some() {
                draw_auth_popup(frame, area, app);
            }
            match app.overlay {
                FeaturesOverlay::Help => draw_help_overlay(frame, area, app),
                FeaturesOverlay::Guide => draw_guide_overlay(frame, area, app),
                FeaturesOverlay::Llm => draw_llm_overlay(frame, area, app),
                FeaturesOverlay::None => {}
            }
        })
        .map_err(Error::Io)?;
    Ok(())
}

fn list_viewport_rows(area: Rect) -> usize {
    area.height.saturating_sub(3).max(1) as usize
}

fn ensure_list_visible(cursor: usize, scroll: &mut usize, visible: usize, total: usize) {
    if cursor < *scroll {
        *scroll = cursor;
    } else if visible > 0 && cursor >= *scroll + visible {
        *scroll = cursor + 1 - visible;
    }
    let max_scroll = total.saturating_sub(visible);
    *scroll = (*scroll).min(max_scroll);
}

fn result_row_style(theme: &Theme, r: &FeatureResult) -> Style {
    match r.verdict {
        ProbeVerdict::Healthy => Style::default().fg(theme.ok),
        ProbeVerdict::Reachable => Style::default().fg(theme.warn),
        ProbeVerdict::Failed => Style::default().fg(theme.critical),
    }
}

fn verdict_glyph(theme: &Theme, verdict: ProbeVerdict, warn: bool) -> String {
    if theme.use_color {
        return match (verdict, warn) {
            (ProbeVerdict::Healthy, true) => "✓⚠".into(),
            (ProbeVerdict::Healthy, false) => "✓".into(),
            (ProbeVerdict::Reachable, _) => "◐".into(),
            (ProbeVerdict::Failed, _) => "✗".into(),
        };
    }
    match verdict {
        ProbeVerdict::Healthy if warn => "OK!".into(),
        ProbeVerdict::Healthy => "OK".into(),
        ProbeVerdict::Reachable => "REACH".into(),
        ProbeVerdict::Failed => "FAIL".into(),
    }
}

fn result_mark_line(theme: &Theme, r: &FeatureResult, selected: bool) -> Line<'static> {
    let warn = has_step_warnings(r);
    let extra = if selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    let glyph = verdict_glyph(theme, r.verdict, warn);
    let style = match r.verdict {
        ProbeVerdict::Healthy if warn => Style::default().fg(theme.warn).add_modifier(extra),
        ProbeVerdict::Healthy => Style::default().fg(theme.ok).add_modifier(extra),
        ProbeVerdict::Reachable => Style::default().fg(theme.warn).add_modifier(extra),
        ProbeVerdict::Failed => Style::default().fg(theme.critical).add_modifier(extra),
    };
    Line::from(Span::styled(glyph, style))
}

fn result_data_style(theme: &Theme, r: &FeatureResult, selected: bool) -> Style {
    let mut style = result_row_style(theme, r);
    if selected {
        style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
    }
    style
}

fn result_mark_char(theme: &Theme, r: &FeatureResult) -> String {
    if r.verdict == ProbeVerdict::Healthy && has_step_warnings(r) {
        "✓⚠".into()
    } else {
        result_mark_line(theme, r, false)
            .spans
            .first()
            .map(|s| s.content.to_string())
            .unwrap_or_else(|| "?".into())
    }
}

fn draw_running(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let visible = list_viewport_rows(area);
    app.viewport_rows = visible;

    if app.run_follow {
        if let Some(active) = app
            .run_states
            .iter()
            .position(|s| matches!(s, RowRunState::Running))
        {
            ensure_list_visible(active, &mut app.scroll, visible, app.features.len());
        }
    }

    let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let spinner = spin[(app.tick as usize / 2) % spin.len()];

    let (done, total) = run_progress(&app.run_states);
    let row_total = app.features.len();
    let title = if row_total > visible {
        let from = app.scroll + 1;
        let to = (app.scroll + visible).min(row_total);
        format!(" Running ({done}/{total}) · {from}–{to}/{row_total} · ↑↓ PgUp/Dn ")
    } else {
        format!(" Running ({done}/{total}) ")
    };

    let rows: Vec<Row> = app
        .features
        .iter()
        .enumerate()
        .skip(app.scroll)
        .take(visible)
        .map(|(i, f)| {
            let kind = kind_label(f);

            let (status, style) = match app.run_states.get(i) {
                Some(RowRunState::Idle) | None => {
                    ("–".to_string(), Style::default().fg(app.theme.muted))
                }
                Some(RowRunState::Queued) => {
                    ("○".to_string(), Style::default().fg(app.theme.muted))
                }
                Some(RowRunState::Running) => (
                    format!("{spinner} …"),
                    Style::default()
                        .fg(app.theme.brand)
                        .add_modifier(Modifier::BOLD),
                ),
                Some(RowRunState::Done(r)) => {
                    let mark = result_mark_char(&app.theme, r);
                    let style = result_row_style(&app.theme, r);
                    (format!("{mark} {:.0}ms", r.total_ms), style)
                }
            };

            let style = if Some(i) == active_run_idx(app) {
                style
                    .fg(app.theme.brand)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                style
            };

            Row::new(vec![
                status,
                kind.to_string(),
                f.label.clone(),
                f.source.clone(),
                if let Some(w) = &f.workflow {
                    format!("{} steps", w.steps.len())
                } else {
                    truncate(&f.url, 40)
                },
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(5),
            Constraint::Min(20),
            Constraint::Length(14),
            Constraint::Min(18),
        ],
    )
    .header(
        Row::new(vec!["Status", "Type", "Feature", "Found via", "URL"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(table, area);
}

fn run_progress(states: &[RowRunState]) -> (usize, usize) {
    let total = states
        .iter()
        .filter(|s| !matches!(s, RowRunState::Idle))
        .count();
    let done = states
        .iter()
        .filter(|s| matches!(s, RowRunState::Done(_)))
        .count();
    (done, total)
}

fn draw_select_with_detail(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let workflow = app
        .features
        .get(app.cursor)
        .and_then(|f| f.workflow.clone());
    if let Some(w) = workflow {
        let detail_h = workflow_panel_height_for(&w, &app.theme, area);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(detail_h)])
            .split(area);
        draw_select(frame, parts[0], app);
        draw_workflow_detail(frame, parts[1], &w, &app.theme, "Flow");
    } else {
        draw_select(frame, area, app);
    }
}

fn active_run_idx(app: &App) -> Option<usize> {
    app.run_states
        .iter()
        .position(|s| matches!(s, RowRunState::Running))
        .or_else(|| {
            app.run_states
                .iter()
                .position(|s| matches!(s, RowRunState::Queued))
        })
}

fn draw_running_with_detail(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let idx = active_run_idx(app).unwrap_or(app.cursor);
    let workflow = app.features.get(idx).and_then(|f| f.workflow.clone());
    if let Some(w) = workflow {
        let detail_h = running_detail_height(&w, &app.theme, area);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(10), Constraint::Length(detail_h)])
            .split(area);
        draw_running(frame, parts[0], app);
        draw_workflow_detail(frame, parts[1], &w, &app.theme, "Running");
    } else if let Some(f) = app.features.get(idx).cloned() {
        let detail_h = 8.min(area.height.saturating_sub(8)).max(5);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(detail_h)])
            .split(area);
        draw_running(frame, parts[0], app);
        draw_running_feature_detail(frame, parts[1], app, &f, idx);
    } else {
        draw_running(frame, area, app);
    }
}

fn draw_running_feature_detail(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    f: &DetectedFeature,
    idx: usize,
) {
    let probing = matches!(app.run_states.get(idx), Some(RowRunState::Running));
    let title = if probing {
        format!(" Probing: {} ", f.label)
    } else {
        format!(" Next: {} ", f.label)
    };
    let mut lines = inspect_feature_lines(f, &app.theme);
    match app.run_states.get(idx) {
        Some(RowRunState::Running) => lines.insert(
            0,
            Line::from(Span::styled(
                "in progress",
                Style::default()
                    .fg(app.theme.brand)
                    .add_modifier(Modifier::BOLD),
            )),
        ),
        Some(RowRunState::Queued) => lines.insert(
            0,
            Line::from(Span::styled("queued", Style::default().fg(app.theme.muted))),
        ),
        Some(RowRunState::Done(r)) => lines.insert(
            0,
            Line::from(Span::styled(
                r.message.clone(),
                Style::default().fg(app.theme.muted),
            )),
        ),
        _ => {}
    }
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(
                Style::default()
                    .fg(app.theme.brand)
                    .add_modifier(Modifier::BOLD),
            ),
    );
    frame.render_widget(p, area);
}

fn draw_results_with_detail(frame: &mut ratatui::Frame, main: Rect, bars: Rect, app: &mut App) {
    let workflow = app.report.as_ref().and_then(|rep| {
        rep.results
            .get(app.cursor)
            .and_then(|r| r.feature.workflow.clone())
    });
    if let Some(w) = workflow {
        let detail_h = workflow_panel_height_for(&w, &app.theme, main);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(detail_h)])
            .split(main);
        draw_results(frame, parts[0], bars, app);
        draw_workflow_detail(frame, parts[1], &w, &app.theme, "Flow");
        return;
    }
    draw_results(frame, main, bars, app);
}

fn workflow_panel_height_for(w: &WorkflowScenario, theme: &Theme, area: Rect) -> u16 {
    let lines = workflow_detail_lines(w, theme);
    let inner_w = area.width.saturating_sub(4).max(8);
    let content = wrapped_line_count(&lines, inner_w).max(1);
    let needed = content.saturating_add(2); // borders
    let max_h = area
        .height
        .saturating_sub(8)
        .min(area.height.saturating_mul(6) / 10)
        .max(6);
    needed.min(max_h).max(5)
}

/// Detail panel during runs — cap height so the status table keeps most of the screen.
fn running_detail_height(w: &WorkflowScenario, theme: &Theme, area: Rect) -> u16 {
    let ideal = workflow_panel_height_for(w, theme, area);
    let table_min = 12u16;
    let max_detail = area
        .height
        .saturating_sub(table_min)
        .min(area.height.saturating_mul(38) / 100)
        .max(6);
    ideal.min(max_detail).max(5)
}

fn wrapped_line_count(lines: &[Line], inner_width: u16) -> u16 {
    let w = inner_width.max(1) as usize;
    lines
        .iter()
        .map(|line| {
            let n = line.width().max(1);
            n.div_ceil(w).max(1)
        })
        .sum::<usize>() as u16
}

fn draw_workflow_detail(
    frame: &mut ratatui::Frame,
    area: Rect,
    w: &WorkflowScenario,
    theme: &Theme,
    title_kind: &str,
) {
    let lines = workflow_detail_lines(w, theme);
    let title = format!(" {title_kind}: {} ", w.label);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_style(
                Style::default()
                    .fg(theme.brand)
                    .add_modifier(Modifier::BOLD),
            ),
    );
    frame.render_widget(p, area);
}

fn workflow_detail_lines(w: &WorkflowScenario, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let kind = match w.kind {
        FlowKind::Read => "read",
        FlowKind::Write => "write",
    };
    lines.push(Line::from(Span::styled(
        format!("Kind: {kind}"),
        Style::default().fg(theme.muted),
    )));
    if !w.description.is_empty() {
        lines.push(Line::from(Span::styled(
            w.description.clone(),
            Style::default().fg(theme.muted),
        )));
    }
    for (i, step) in w.steps.iter().enumerate() {
        let mut spans = vec![
            Span::styled(format!(" {:>2}. ", i + 1), Style::default().fg(theme.muted)),
            Span::styled(
                format!("{:4} ", step.method.to_ascii_uppercase()),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(step.path.clone()),
        ];
        if let Some(body) = step_body_hint(&step.body) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(body, Style::default().fg(theme.muted)));
        }
        if step.capture_bearer.is_some() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!(
                    "→ save {}",
                    step.capture_bearer.as_deref().unwrap_or("token")
                ),
                Style::default().fg(theme.warn),
            ));
        }
        if step.use_bearer {
            spans.push(Span::raw("  "));
            spans.push(Span::styled("Bearer", Style::default().fg(theme.ok)));
        }
        if let Some(expected) = step.expect_status {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("expect {expected}"),
                Style::default().fg(theme.muted),
            ));
        }
        if !step.name.is_empty() && step.name != path_tail(&step.path) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("({})", step.name),
                Style::default().fg(theme.muted),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn step_body_hint(body: &Option<serde_json::Value>) -> Option<String> {
    let v = body.as_ref()?;
    if let Some(obj) = v.as_object() {
        let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        if keys.is_empty() {
            return Some("{ }".into());
        }
        return Some(format!("{{ {} }}", keys.join(", ")));
    }
    Some("{…}".into())
}

fn path_tail(path: &str) -> String {
    path.trim_matches('/')
        .split('/')
        .next_back()
        .unwrap_or("")
        .to_string()
}

fn draw_select(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    if let Some(err) = &app.error {
        let p = Paragraph::new(err.as_str())
            .block(Block::default().borders(Borders::ALL).title(" Error "));
        frame.render_widget(p, area);
        return;
    }

    let visible = list_viewport_rows(area);
    ensure_list_visible(app.cursor, &mut app.scroll, visible, app.features.len());
    app.viewport_rows = visible;

    let total = app.features.len();
    let title = if total > visible {
        let from = app.scroll + 1;
        let to = (app.scroll + visible).min(total);
        format!(" Select features · {from}–{to}/{total} · ↑↓ PgUp/Dn ")
    } else {
        " Select features to test (Space) ".into()
    };

    let rows: Vec<Row> = app
        .features
        .iter()
        .enumerate()
        .skip(app.scroll)
        .take(visible)
        .map(|(i, f)| {
            let on = app.selected.get(i).copied().unwrap_or(false);
            let mark = if on { "[x]" } else { "[ ]" };
            let kind = kind_label(f);
            let detail = if let Some(w) = &f.workflow {
                format!("{} steps", w.steps.len())
            } else {
                truncate(&f.url, 48)
            };
            let style = if i == app.cursor {
                Style::default()
                    .fg(app.theme.brand)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else if on {
                Style::default().fg(app.theme.ok)
            } else {
                Style::default().fg(app.theme.muted)
            };
            Row::new(vec![
                mark.to_string(),
                kind.to_string(),
                f.label.clone(),
                f.source.clone(),
                detail,
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(5),
            Constraint::Min(20),
            Constraint::Length(16),
            Constraint::Min(24),
        ],
    )
    .header(
        Row::new(vec!["", "Type", "Feature", "Found via", "URL"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(table, area);
}

fn score_gauge_color(
    theme: &Theme,
    passed: usize,
    failed: usize,
    selected: usize,
) -> ratatui::style::Color {
    if failed == 0 || selected == 0 {
        return theme.ok;
    }
    let pass_ratio = passed as f64 / selected as f64;
    if pass_ratio >= 0.85 {
        theme.ok
    } else if pass_ratio >= 0.5 {
        theme.warn
    } else {
        theme.critical
    }
}

fn draw_results(frame: &mut ratatui::Frame, main: Rect, bars: Rect, app: &mut App) {
    let Some(rep) = &app.report else {
        return;
    };

    let pass_ratio = if rep.selected == 0 {
        0.0
    } else {
        rep.passed as f64 / rep.selected as f64
    };
    let g = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(" Score "))
        .gauge_style(Style::default().fg(score_gauge_color(
            &app.theme,
            rep.passed,
            rep.failed,
            rep.selected,
        )))
        .ratio(pass_ratio.clamp(0.0, 1.0))
        .label(format!(
            "{}/{} passed · {} failed",
            rep.passed, rep.selected, rep.failed
        ));
    frame.render_widget(g, bars);

    let visible = list_viewport_rows(main);
    let total = rep.results.len();
    ensure_list_visible(app.cursor, &mut app.scroll, visible, total);
    app.viewport_rows = visible;
    let max_ms = rep
        .results
        .iter()
        .map(|r| r.total_ms)
        .fold(1.0_f64, f64::max);

    let rows: Vec<Row> = rep
        .results
        .iter()
        .enumerate()
        .skip(app.scroll)
        .take(visible)
        .map(|(i, r)| {
            let selected = i == app.cursor;
            let data_style = result_data_style(&app.theme, r, selected);
            let bar_len = ((r.total_ms / max_ms) * 8.0).round() as usize;
            let bar = format!(
                "{}{}",
                "█".repeat(bar_len.min(8)),
                "░".repeat(8usize.saturating_sub(bar_len))
            );
            Row::new(vec![
                Cell::from(result_mark_line(&app.theme, r, selected)),
                Cell::from(r.feature.label.clone()).style(data_style),
                Cell::from(bar).style(data_style),
                Cell::from(format!("{:.0}ms", r.total_ms)).style(data_style),
                Cell::from(compact_result_detail(r)).style(data_style),
            ])
        })
        .collect();

    let title = if total > visible {
        let from = app.scroll + 1;
        let to = (app.scroll + visible).min(total);
        format!(" Results · {from}–{to}/{total} · ↑↓ highlight ")
    } else {
        " Results (↑↓ highlight) ".into()
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Min(22),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Min(28),
        ],
    )
    .header(
        Row::new(vec!["", "Feature", "Time", "Total", "Detail"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(table, main);
}

fn draw_legend(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let line = if matches!(app.phase, Phase::Running) {
        let (done, total) = run_progress(&app.run_states);
        Line::from(vec![
            Span::styled(
                format!(" {done}/{total} complete "),
                Style::default()
                    .fg(app.theme.brand)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  ↑↓ PgUp/Dn scroll · q quit · status updates per row"),
        ])
    } else {
        let n_on = app.selected.iter().filter(|x| **x).count();
        let flow_hint = app
            .features
            .get(app.cursor)
            .and_then(|f| f.workflow.as_ref())
            .map(|w| format!(" · {}-step flow below", w.steps.len()))
            .unwrap_or_default();
        Line::from(vec![
            Span::styled(
                format!(" {n_on} selected "),
                Style::default()
                    .fg(app.theme.ok)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  ↑↓ PgUp/Dn · Space toggle · a all · n none · c auth · d inspect · Enter/r run · ? help · g guide · t theme{flow_hint}"
            )),
        ])
    };
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" Actions "));
    frame.render_widget(p, area);
}

fn active_popup_rect(area: Rect, app: &App) -> Rect {
    if app.report_open {
        centered_rect(94, 88, area)
    } else if app.inspect_open {
        centered_rect(92, 82, area)
    } else {
        centered_rect(92, 85, area)
    }
}

/// Apply DIM to existing cells so the UI stays readable but de-emphasized behind a modal.
fn dim_background(frame: &mut ratatui::Frame, area: Rect, exclude: Rect, theme: &Theme) {
    let buf = frame.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if x >= exclude.x
                && x < exclude.x.saturating_add(exclude.width)
                && y >= exclude.y
                && y < exclude.y.saturating_add(exclude.height)
            {
                continue;
            }
            let cell = &mut buf[(x, y)];
            let style = cell.style().fg(theme.muted).add_modifier(Modifier::DIM);
            cell.set_style(style);
        }
    }
}

fn key_event_active(key: &crossterm::event::KeyEvent, app: &App) -> bool {
    if key.kind == KeyEventKind::Press {
        return true;
    }
    if key.kind != KeyEventKind::Repeat {
        return false;
    }
    if !(app.report_open || app.inspect_open) {
        return false;
    }
    matches!(
        key.code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Char('j')
            | KeyCode::Char('k')
    )
}

fn refresh_report_cache(app: &mut App) {
    app.cached_report_lines = app
        .report
        .as_ref()
        .map(|rep| build_report_lines(rep, &app.theme));
}

fn draw_report_overlay(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let popup = centered_rect(94, 88, area);
    frame.render_widget(Clear, popup);

    let fallback = vec![Line::from("No run data.")];
    let lines = app.cached_report_lines.as_ref().unwrap_or(&fallback);
    let viewport = popup.height.saturating_sub(2).max(1) as usize;
    app.report_viewport_rows = viewport;
    let max_scroll = lines.len().saturating_sub(viewport) as u16;
    let scroll = app.report_scroll.min(max_scroll);
    app.report_scroll = scroll;

    let p = Paragraph::new(lines.clone()).scroll((scroll, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Run report · Esc/R close · ↑↓ PgUp/Dn scroll ")
            .title_style(
                Style::default()
                    .fg(app.theme.brand)
                    .add_modifier(Modifier::BOLD),
            ),
    );
    frame.render_widget(p, popup);
}

fn build_report_lines(rep: &FeatureRunReport, theme: &Theme) -> Vec<Line<'static>> {
    let cat = build_categorized_report(rep);
    let mut lines = vec![
        Line::from(Span::styled(
            "Summary",
            Style::default()
                .fg(theme.brand)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "  {} features · {} clean · {} finding(s)",
            cat.total_features, cat.clean_features, cat.issue_count
        )),
        Line::from(""),
    ];

    if cat.issue_count == 0 {
        lines.push(Line::from(Span::styled(
            "  All probes clean — no categorized issues.",
            Style::default().fg(theme.ok),
        )));
        return lines;
    }

    for category in IssueCategory::all() {
        let Some(items) = cat.issues.get(&category) else {
            continue;
        };
        if items.is_empty() {
            continue;
        }
        let color = report_category_style(theme, category);
        lines.push(Line::from(Span::styled(
            format!("{} ({})", category.label(), items.len()),
            color.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", category.description()),
            Style::default().fg(theme.muted),
        )));
        for item in items {
            lines.push(Line::from(""));
            let step = item
                .step_name
                .as_deref()
                .map(|s| format!(" · {s}"))
                .unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("  • {}{}", item.feature_label, step),
                color,
            )));
            if let (Some(method), Some(path)) = (&item.method, &item.path) {
                let path = truncate(path, 72);
                lines.push(Line::from(Span::styled(
                    format!("    {method} {path}"),
                    Style::default().fg(theme.muted),
                )));
            }
            let detail = truncate(&item.detail, 96);
            lines.push(Line::from(format!("    {detail}")));
            let hint = truncate(&item.hint, 96);
            lines.push(Line::from(Span::styled(
                format!("    → {hint}"),
                Style::default().fg(theme.warn),
            )));
        }
        lines.push(Line::from(""));
    }
    lines
}

fn report_category_style(theme: &Theme, category: IssueCategory) -> Style {
    match category {
        IssueCategory::Severe => Style::default().fg(theme.critical),
        IssueCategory::Auth => Style::default().fg(theme.warn),
        IssueCategory::Compatibility => Style::default().fg(theme.brand),
        IssueCategory::Performance => Style::default().fg(theme.muted),
    }
}

fn draw_inspect_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(92, 82, area);
    frame.render_widget(Clear, popup);

    let lines = inspect_lines(app);
    let max_scroll = lines
        .len()
        .saturating_sub(popup.height.saturating_sub(3) as usize) as u16;
    let scroll = app.inspect_scroll.min(max_scroll);
    let title = inspect_title(app);
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_style(
                    Style::default()
                        .fg(app.theme.brand)
                        .add_modifier(Modifier::BOLD),
                ),
        );
    frame.render_widget(p, popup);
}

fn inspect_title(app: &App) -> String {
    match app.phase {
        Phase::Results => " Inspect · Esc close · ↑↓ scroll ".into(),
        _ => " Inspect (d) · Esc close · ↑↓ scroll ".into(),
    }
}

fn inspect_lines(app: &App) -> Vec<Line<'static>> {
    let theme = &app.theme;
    if matches!(app.phase, Phase::Results) {
        if let Some(r) = app
            .report
            .as_ref()
            .and_then(|rep| rep.results.get(app.cursor))
        {
            return inspect_result_lines(r, theme);
        }
    }
    if let Some(f) = app.features.get(app.cursor) {
        return inspect_feature_lines(f, theme);
    }
    vec![Line::from("No details for this row.")]
}

fn inspect_feature_lines(f: &DetectedFeature, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![
        styled_kv("Feature", &f.label, theme),
        styled_kv("Type", &format!("{:?}", f.kind), theme),
        styled_kv("Source", &f.source, theme),
        styled_kv("URL", &f.url, theme),
    ];
    if let Some(m) = &f.method {
        lines.push(styled_kv("Method", m, theme));
    }
    if let Some(w) = &f.workflow {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("Workflow · {} steps", w.steps.len()),
            Style::default()
                .fg(theme.brand)
                .add_modifier(Modifier::BOLD),
        )));
        if !w.description.is_empty() {
            lines.push(Line::from(Span::styled(
                w.description.clone(),
                Style::default().fg(theme.muted),
            )));
        }
        lines.extend(workflow_detail_lines(w, theme));
    }
    lines
}

fn inspect_result_lines(r: &FeatureResult, theme: &Theme) -> Vec<Line<'static>> {
    let style = result_row_style(theme, r);
    let mut lines = vec![
        Line::from({
            let mut spans = result_mark_line(theme, r, false).spans;
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                r.feature.label.clone(),
                style.add_modifier(Modifier::BOLD),
            ));
            spans
        }),
        styled_kv(
            "Verdict",
            &if r.verdict == ProbeVerdict::Healthy && has_step_warnings(r) {
                let n = r
                    .steps
                    .iter()
                    .filter(|s| s.verdict != ProbeVerdict::Healthy)
                    .count();
                format!("Healthy ({n} alert(s))")
            } else {
                format!("{:?}", r.verdict)
            },
            theme,
        ),
        styled_kv(
            "HTTP",
            &r.status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".into()),
            theme,
        ),
        styled_kv("Total", &format!("{:.0} ms", r.total_ms), theme),
        styled_kv("URL", &r.feature.url, theme),
        Line::from(Span::styled(
            r.message.clone(),
            Style::default().fg(theme.muted),
        )),
    ];

    if let Some(l7) = &r.l7 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "HTTP probe",
            Style::default()
                .fg(theme.brand)
                .add_modifier(Modifier::BOLD),
        )));
        if let Some(ip) = &l7.resolved_ip {
            lines.push(styled_kv("IP", ip, theme));
        }
        if let Some(v) = l7.dns_ms {
            lines.push(styled_kv("DNS", &format!("{v:.0} ms"), theme));
        }
        if let Some(v) = l7.tcp_ms {
            lines.push(styled_kv("TCP", &format!("{v:.0} ms"), theme));
        }
        if let Some(v) = l7.tls_ms {
            lines.push(styled_kv("TLS", &format!("{v:.0} ms"), theme));
        }
        if let Some(v) = l7.ttfb_ms {
            lines.push(styled_kv("TTFB", &format!("{v:.0} ms"), theme));
        }
        if let Some(v) = l7.transfer_ms {
            lines.push(styled_kv("Transfer", &format!("{v:.0} ms"), theme));
        }
        lines.push(styled_kv("Bytes", &l7.bytes_read.to_string(), theme));
    }

    if !r.steps.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("Steps ({})", r.steps.len()),
            Style::default()
                .fg(theme.brand)
                .add_modifier(Modifier::BOLD),
        )));
        for (i, step) in r.steps.iter().enumerate() {
            let (mark, st) = match step.verdict {
                ProbeVerdict::Healthy => ('✓', Style::default().fg(theme.ok)),
                ProbeVerdict::Reachable => ('◐', Style::default().fg(theme.warn)),
                ProbeVerdict::Failed => ('✗', Style::default().fg(theme.critical)),
            };
            let status = step
                .status
                .map(|s| format!("HTTP {s}"))
                .unwrap_or_else(|| "no response".into());
            let mut spans = vec![
                Span::styled(
                    format!(" {:>2}. {mark} ", i + 1),
                    st.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:4} ", step.method),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(step.path.clone()),
                Span::raw("  "),
                Span::styled(status, st),
            ];
            if step.captured_token {
                spans.push(Span::raw("  "));
                spans.push(Span::styled("token saved", Style::default().fg(theme.ok)));
            }
            lines.push(Line::from(spans));
            if !step.message.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("      {}", step.message),
                    Style::default().fg(theme.muted),
                )));
            }
        }
    } else if let Some(w) = &r.feature.workflow {
        lines.push(Line::from(""));
        lines.extend(workflow_detail_lines(w, theme));
    }

    lines
}

fn styled_kv(key: &str, value: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {key:<8} "), Style::default().fg(theme.muted)),
        Span::raw(value.to_string()),
    ])
}

fn env_var_is_set(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn env_var_names_for_field(key: &str, realm: AuthRealmHint) -> Vec<&'static str> {
    match key {
        "email" => match realm {
            AuthRealmHint::Annotator => {
                vec!["TRACE_DIFF_ANNOTATOR_EMAIL", "CONFUCIUS_ANNOTATOR_EMAIL"]
            }
            _ => vec!["TRACE_DIFF_EMAIL", "CONFUCIUS_EMAIL"],
        },
        "password" => match realm {
            AuthRealmHint::Annotator => {
                vec!["TRACE_DIFF_ANNOTATOR_PASSWORD", "CONFUCIUS_ANNOTATOR_PASSWORD"]
            }
            _ => vec!["TRACE_DIFF_PASSWORD", "CONFUCIUS_PASSWORD"],
        },
        "secret" => vec!["TRACE_DIFF_ADMIN_SECRET", "CONFUCIUS_ADMIN_KEY"],
        "bearer_token" => vec!["TRACE_DIFF_BEARER_TOKEN"],
        _ if key.contains("captcha") => vec!["TRACE_DIFF_CAPTCHA_TOKEN", "CAPTCHA_TOKEN"],
        _ => vec![],
    }
}

fn auth_env_summary_for_spec(spec: &RealmAuthSpec) -> String {
    let mut vars = Vec::new();
    for field in &spec.fields {
        for name in env_var_names_for_field(&field.key, spec.realm) {
            if env_var_is_set(name) && !vars.contains(&name) {
                vars.push(name);
            }
        }
    }
    vars.join(", ")
}

fn maybe_auto_open_auth_popup(app: &mut App) {
    let workflows: Vec<WorkflowScenario> = app
        .features
        .iter()
        .filter_map(|f| f.workflow.clone())
        .collect();
    let realms = detect_auth_realms(&workflows);
    if realms.is_empty() {
        return;
    }
    let missing = app.settings.auth.detected_realms_needing_creds(&realms);
    if !missing.is_empty() {
        open_auth_popup(app);
    }
}

fn open_auth_popup(app: &mut App) {
    let workflows: Vec<WorkflowScenario> = app
        .features
        .iter()
        .filter_map(|f| f.workflow.clone())
        .collect();
    let specs = build_realm_auth_specs(&workflows);
    if specs.is_empty() {
        return;
    }

    let mut values = HashMap::new();
    let mut include = HashMap::new();
    let mut rows = Vec::new();

    for spec in &specs {
        include.insert(spec.realm, true);
        rows.push(AuthPopupRow::ToggleRealm { realm: spec.realm });
        let env_line = auth_env_summary_for_spec(spec);
        if !env_line.is_empty() {
            rows.push(AuthPopupRow::Summary {
                text: format!("Env set: {env_line}"),
            });
        }
        rows.push(AuthPopupRow::Summary {
            text: spec.login_summary.clone(),
        });
        for note in &spec.notes {
            rows.push(AuthPopupRow::Note { text: note.clone() });
        }
        for field in &spec.fields {
            let existing = app.settings.auth.field_value(spec.realm, &field.key);
            if let Some(v) = existing {
                values.insert((spec.realm, field.key.clone()), v);
            }
            rows.push(AuthPopupRow::TextInput {
                realm: spec.realm,
                key: field.key.clone(),
                label: field.label.clone(),
                secret: field.secret,
                required: field.required,
                hint: field.hint.clone(),
            });
        }
    }

    let field_idx = rows
        .iter()
        .position(|r| matches!(r, AuthPopupRow::TextInput { .. }))
        .unwrap_or(0);

    app.auth_popup = Some(AuthPopupState {
        field_idx,
        rows,
        specs,
        values,
        include,
    });
    app.auth_dismiss_confirm = false;
}

fn auth_popup_row_is_text(row: &AuthPopupRow) -> bool {
    matches!(row, AuthPopupRow::TextInput { .. })
}

fn auth_popup_is_included(popup: &AuthPopupState, realm: AuthRealmHint) -> bool {
    popup.include.get(&realm).copied().unwrap_or(false)
}

fn handle_auth_popup(app: &mut App, code: KeyCode) -> bool {
    let Some(popup) = app.auth_popup.as_mut() else {
        return false;
    };
    let on_text = popup
        .rows
        .get(popup.field_idx)
        .map(auth_popup_row_is_text)
        .unwrap_or(false);

    match code {
        KeyCode::Esc => {
            if app.auth_dismiss_confirm {
                app.auth_popup = None;
                app.auth_dismiss_confirm = false;
            } else {
                app.auth_dismiss_confirm = true;
            }
        }
        KeyCode::Enter => {
            apply_auth_popup(app);
            app.auth_popup = None;
            app.auth_dismiss_confirm = false;
        }
        KeyCode::Tab | KeyCode::Down => {
            popup.field_idx = (popup.field_idx + 1) % popup.rows.len().max(1);
        }
        KeyCode::BackTab | KeyCode::Up => {
            if popup.rows.is_empty() {
                return true;
            }
            popup.field_idx = if popup.field_idx == 0 {
                popup.rows.len() - 1
            } else {
                popup.field_idx - 1
            };
        }
        KeyCode::Char('j') if !on_text => {
            popup.field_idx = (popup.field_idx + 1) % popup.rows.len().max(1);
        }
        KeyCode::Char('k') if !on_text => {
            if popup.rows.is_empty() {
                return true;
            }
            popup.field_idx = if popup.field_idx == 0 {
                popup.rows.len() - 1
            } else {
                popup.field_idx - 1
            };
        }
        KeyCode::Char(' ') => {
            if let Some(AuthPopupRow::ToggleRealm { realm }) = popup.rows.get(popup.field_idx) {
                let realm = *realm;
                let on = popup.include.get(&realm).copied().unwrap_or(false);
                popup.include.insert(realm, !on);
            }
        }
        KeyCode::Backspace => {
            auth_popup_backspace(popup);
        }
        KeyCode::Char(c) => {
            auth_popup_push_char(popup, c);
        }
        _ => {}
    }
    true
}

fn auth_popup_backspace(popup: &mut AuthPopupState) {
    let Some(AuthPopupRow::TextInput { realm, key, .. }) = popup.rows.get(popup.field_idx).cloned()
    else {
        return;
    };
    if let Some(val) = popup.values.get_mut(&(realm, key)) {
        val.pop();
    }
}

fn auth_popup_push_char(popup: &mut AuthPopupState, c: char) {
    let Some(AuthPopupRow::TextInput { realm, key, .. }) = popup.rows.get(popup.field_idx).cloned()
    else {
        return;
    };
    popup.values.entry((realm, key)).or_default().push(c);
}

fn apply_auth_popup(app: &mut App) {
    let Some(popup) = app.auth_popup.clone() else {
        return;
    };

    let auth = &mut app.settings.auth;
    for spec in &popup.specs {
        if !auth_popup_is_included(&popup, spec.realm) {
            continue;
        }
        for field in &spec.fields {
            let val = popup
                .values
                .get(&(spec.realm, field.key.clone()))
                .cloned()
                .filter(|s| !s.is_empty());
            auth.set_realm_field(spec.realm, &field.key, val);
        }
    }

    let mut skipped = 0usize;
    for (i, f) in app.features.iter().enumerate() {
        let Some(w) = f.workflow.as_ref() else {
            continue;
        };
        let realm = workflow_realm(w);
        let Some(realm) = realm else {
            continue;
        };
        let Some(spec) = popup.specs.iter().find(|s| s.realm == realm) else {
            continue;
        };
        if !auth_popup_is_included(&popup, realm) {
            if app.selected.get(i) == Some(&true) {
                skipped += 1;
            }
            if let Some(s) = app.selected.get_mut(i) {
                *s = false;
            }
            continue;
        }
        if !realm_spec_satisfied(spec, &popup.values) {
            if app.selected.get(i) == Some(&true) {
                skipped += 1;
            }
            if let Some(s) = app.selected.get_mut(i) {
                *s = false;
            }
        }
    }
    if skipped > 0 {
        app.status =
            format!("Skipped {skipped} flow(s) — missing required auth · c edit · Enter run");
    } else {
        app.status = "Auth saved · ↑↓ Space · Enter run · c edit auth · q quit".into();
    }
}

fn draw_auth_popup(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let Some(popup) = &app.auth_popup else {
        return;
    };
    let rect = centered_rect(92, 85, area);
    frame.render_widget(Clear, rect);

    let inner = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(4)])
        .split(rect);

    let mut lines: Vec<Line> = vec![
        Line::from(if app.auth_dismiss_confirm {
            "Press Esc again to skip auth · Enter save · Tab/↑↓ move"
        } else {
            "Required fields marked * · Tab/↑↓ move · Space toggle realm · Enter save · Esc skip"
        }),
        Line::from(""),
    ];

    for (i, row) in popup.rows.iter().enumerate() {
        let active = i == popup.field_idx;
        let style = if active {
            Style::default()
                .fg(app.theme.brand)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let muted = Style::default().fg(app.theme.muted);

        match row {
            AuthPopupRow::ToggleRealm { realm } => {
                let title = popup
                    .specs
                    .iter()
                    .find(|s| s.realm == *realm)
                    .map(|s| s.title.as_str())
                    .unwrap_or(realm.as_str());
                let on = auth_popup_is_included(popup, *realm);
                lines.push(Line::styled(
                    format!(" [{}] {title} realm", if on { 'x' } else { ' ' }),
                    style,
                ));
            }
            AuthPopupRow::Summary { text } => {
                lines.push(Line::styled(format!("     → {text}"), muted));
            }
            AuthPopupRow::Note { text } => {
                lines.push(Line::styled(format!("     ! {text}"), muted));
            }
            AuthPopupRow::TextInput {
                realm,
                key,
                label,
                secret,
                required,
                ..
            } => {
                let val = popup
                    .values
                    .get(&(*realm, key.clone()))
                    .cloned()
                    .unwrap_or_default();
                let display = if *secret { "*".repeat(val.len()) } else { val };
                let req = if *required { "*" } else { " " };
                lines.push(Line::styled(
                    format!("     {req} {label:<22} {display}"),
                    style,
                ));
            }
        }
    }

    let title = format!(" Auth profiles ({} realm(s)) ", popup.specs.len());
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_style(
                    Style::default()
                        .fg(app.theme.brand)
                        .add_modifier(Modifier::BOLD),
                ),
        ),
        inner[0],
    );

    let hint_text = popup
        .rows
        .get(popup.field_idx)
        .and_then(|r| match r {
            AuthPopupRow::TextInput { hint, .. } => Some(hint.as_str()),
            AuthPopupRow::ToggleRealm { realm } => popup
                .specs
                .iter()
                .find(|s| s.realm == *realm)
                .map(|s| s.login_summary.as_str()),
            AuthPopupRow::Note { text } | AuthPopupRow::Summary { text } => Some(text.as_str()),
        })
        .unwrap_or("Select a field for help.");

    frame.render_widget(
        Paragraph::new(hint_text)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" How to get this value "),
            )
            .style(Style::default().fg(app.theme.muted)),
        inner[1],
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

fn compact_result_detail(r: &FeatureResult) -> String {
    if let Some(w) = &r.feature.workflow {
        let n = w.steps.len();
        let warn = r
            .steps
            .iter()
            .filter(|s| s.verdict != ProbeVerdict::Healthy)
            .count();
        match r.verdict {
            ProbeVerdict::Healthy if warn > 0 => format!(
                "{n} steps OK · {warn} alert(s) · HTTP {}",
                r.status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".into())
            ),
            ProbeVerdict::Healthy => format!(
                "{n} steps OK · HTTP {}",
                r.status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".into())
            ),
            ProbeVerdict::Reachable => format!(
                "{n} steps reachable · HTTP {}",
                r.status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".into())
            ),
            ProbeVerdict::Failed => r
                .message
                .split(" — ")
                .nth(1)
                .unwrap_or(&r.message)
                .to_string(),
        }
    } else if r.verdict == ProbeVerdict::Healthy && has_step_warnings(r) {
        format!("{} · alert", r.message)
    } else {
        r.message.clone()
    }
}

fn discovery_status_line(workflows: usize, total: usize, llm: &LlmDiscoveryStatus) -> String {
    let base = if workflows > 0 {
        format!("Found {workflows} workflows + {total} total · ↑↓ · Space · Enter run · q quit")
    } else {
        format!(
            "Found {total} candidates · ↑↓ move · Space toggle · a all · n none · Enter run · q quit"
        )
    };
    match llm.status_suffix() {
        Some(suffix) => format!("{base} · {suffix}"),
        None => base,
    }
}

fn default_selected(f: &DetectedFeature) -> bool {
    if f.id == "favicon" || is_write_flow(f) {
        return false;
    }
    matches!(
        f.kind,
        FeatureKind::Api | FeatureKind::Page | FeatureKind::Workflow | FeatureKind::Tls
    )
}

fn kind_label(f: &DetectedFeature) -> &'static str {
    match f.kind {
        FeatureKind::Api => "API",
        FeatureKind::Page => "PAGE",
        FeatureKind::Meta => "META",
        FeatureKind::Tls => "TLS",
        FeatureKind::Workflow => {
            if is_write_flow(f) {
                "WRITE"
            } else {
                "FLOW"
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

fn export_features_report(app: &mut App) -> Result<()> {
    let report = app
        .report
        .as_ref()
        .ok_or_else(|| Error::Other("no report to export — run features first".into()))?;
    std::fs::create_dir_all(".trace-diff").map_err(Error::Io)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let path = std::path::PathBuf::from(format!(".trace-diff/features-report-{stamp}.json"));
    let json = serde_json::to_string_pretty(report).map_err(|e| Error::Other(e.to_string()))?;
    std::fs::write(&path, json).map_err(Error::Io)?;
    app.last_export = Some(path.clone());
    app.status = format!("Exported {}", path.display());
    app.error = None;
    Ok(())
}

fn llm_mode_label(llm: &LlmDiscoveryStatus) -> &'static str {
    match llm {
        LlmDiscoveryStatus::Disabled => "disabled (--no-llm)",
        LlmDiscoveryStatus::Unavailable { .. } => "unavailable",
        LlmDiscoveryStatus::Cached => "cached workflows",
        LlmDiscoveryStatus::HeuristicsOnly => "heuristics only",
        LlmDiscoveryStatus::Refined { .. } => "LLM refined",
        LlmDiscoveryStatus::Generated { .. } => "LLM generated",
    }
}

fn draw_llm_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(76, 62, area);
    frame.render_widget(Clear, popup);
    let mut lines = vec![
        Line::from(" LLM discovery status "),
        Line::from(""),
    ];
    if let Some(llm) = &app.llm_status {
        lines.push(Line::from(format!("  Mode: {}", llm_mode_label(llm))));
        if let Some(suffix) = llm.status_suffix() {
            lines.push(Line::from(format!("  {suffix}")));
        }
        if let Some(hint) = llm.stderr_hint() {
            lines.push(Line::from(""));
            lines.push(Line::from(format!("  {hint}")));
        }
    } else if let Some(res) = app.stored_discover.ai_resolution.as_ref() {
        lines.push(Line::from(format!(
            "  Provider: {} → {}",
            res.requested.label(),
            res.resolved_label()
        )));
        if res.is_ready() {
            if let Some(cfg) = &res.config {
                lines.push(Line::from(format!("  Model: {}", cfg.resolve_model())));
            }
        } else {
            lines.push(Line::from("  LLM not configured — heuristics only"));
            if !res.groq_key_set {
                lines.push(Line::from("  GROQ_API_KEY: not set"));
            }
            if !res.ollama_reachable {
                lines.push(Line::from(format!(
                    "  Ollama: unreachable at {}",
                    res.ollama_host
                )));
            }
        }
    } else {
        lines.push(Line::from("  LLM disabled (--no-llm)"));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("  R rediscover · docs/LLM_SETUP.md"));
    lines.push(Line::from("  Esc or l to close"));
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" LLM ")
                    .border_style(Style::default().fg(app.theme.brand)),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn draw_help_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(78, 70, area);
    frame.render_widget(Clear, popup);
    let lines = vec![
        Line::from(" Keyboard — features TUI "),
        Line::from(""),
        Line::from("  Select:  ↑↓ j/k · Space toggle · a all · n none · Enter/r run"),
        Line::from("  Auth:    c credentials popup"),
        Line::from("  LLM:     l status panel · R rediscover"),
        Line::from("  Inspect: d / i step detail"),
        Line::from("  Results: R categorized report · e export JSON"),
        Line::from("  Help:    ? help · g guide · t theme · q quit"),
        Line::from(""),
        Line::from("  OK=green  REACH=yellow (auth/body)  FAIL=red"),
        Line::from(""),
        Line::from("  Esc or ? to close"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Help ")
                    .border_style(Style::default().fg(app.theme.brand)),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn draw_guide_overlay(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let popup = centered_rect(82, 72, area);
    frame.render_widget(Clear, popup);
    let lines = vec![
        Line::from(" Quick guide — trace-diff features "),
        Line::from(""),
        Line::from(" 1. Discovery fetches OpenAPI and builds workflow rows (FLOW)."),
        Line::from(" 2. FLOW = multi-step API scenario (login → GET chain)."),
        Line::from(" 3. WRITE = mutating smoke (off by default). TLS = cert canary."),
        Line::from(" 4. Yellow REACH = route exists, needs auth — press c to set creds."),
        Line::from(" 5. Optional LLM refine: set GROQ_API_KEY or run Ollama."),
        Line::from("    Check: trace-diff features --check-llm"),
        Line::from(""),
        Line::from("  Docs: docs/FEATURES_AUTODETECT.md · docs/LLM_SETUP.md"),
        Line::from(""),
        Line::from("  Esc or ? to close"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Guide ")
                    .border_style(Style::default().fg(app.theme.brand)),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}
