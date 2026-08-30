//! `trace-diff run` command.

use crate::cli::{effective_output, OutputFormat, RunArgs};
use crate::diff::{diff_runs, DiffReport, DiffThresholds};
use crate::error::{Error, Result};
use crate::l7::{self, L7Config};
use crate::meta::{self, PrivilegeLevel, RunMetadata};
use crate::progress::ProgressEvent;
use crate::store::{StoredRun, Store};
use crate::theme::Theme;
use crate::traceroute::{self, TraceConfig};
use crate::tui::{self, AppView};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

pub struct UiOpts {
    pub theme: Theme,
}

pub async fn execute(args: RunArgs, ui: UiOpts) -> Result<()> {
    let store = Store::open(args.db.as_deref())?;
    let output = effective_output(args.output, args.headless);
    let privileges = if args.skip_trace {
        PrivilegeLevel::Unknown
    } else {
        meta::detect_icmp_privilege()
    };
    let meta = RunMetadata::capture(args.skip_trace, args.skip_http, privileges);
    debug!(?meta, "run metadata");

    let use_live_tui = output == OutputFormat::Tui
        && std::io::IsTerminal::is_terminal(&std::io::stdout());

    if use_live_tui {
        return execute_live_tui(args, ui, meta).await;
    }

    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressEvent>();
    // Drain progress into tracing for --verbose/--debug in non-TUI modes.
    let drain = tokio::spawn(async move {
        while let Some(ev) = progress_rx.recv().await {
            debug!(progress = %ev.label(), "probe");
        }
    });

    let _ = progress_tx.send(ProgressEvent::Started {
        target: args.target.clone(),
    });

    let (trace, l7) = probe_pair(&args, Some(progress_tx.clone())).await?;

    let _ = progress_tx.send(ProgressEvent::Saving);
    let run_id =
        store.save_run_with_meta(&args.target, trace.as_ref(), l7.as_ref(), Some(&meta))?;
    debug!(%run_id, "run saved");

    if let Some(name) = &args.save_baseline {
        store.tag_baseline(&run_id, name)?;
        debug!(baseline = %name, "baseline saved");
    }

    let mut run = store.get_run(&run_id)?;
    run.meta = Some(meta);

    let diff = maybe_compare(&store, &run, args.compare_baseline.as_deref())?;
    check_thresholds(&run, &args)?;

    let _ = progress_tx.send(ProgressEvent::Done);
    drop(progress_tx);
    let _ = drain.await;

    emit(&run, diff.as_ref(), output, &ui.theme, args.db.as_deref())?;
    Ok(())
}

async fn execute_live_tui(args: RunArgs, ui: UiOpts, meta: RunMetadata) -> Result<()> {
    let (progress_tx, progress_rx) = mpsc::unbounded_channel::<ProgressEvent>();
    let (result_tx, result_rx) = oneshot::channel();

    let target = args.target.clone();
    let save_baseline = args.save_baseline.clone();
    let compare = args.compare_baseline.clone();
    let db_path = args.db.clone();
    let args_clone = args.clone();
    let db_for_task = db_path.clone();

    tokio::spawn(async move {
        let outcome = (async {
            let _ = progress_tx.send(ProgressEvent::Started {
                target: args_clone.target.clone(),
            });
            // Finish all async probing before touching SQLite (!Sync Connection).
            let (trace, l7) = probe_pair(&args_clone, Some(progress_tx.clone())).await?;
            let _ = progress_tx.send(ProgressEvent::Saving);
            Ok::<_, Error>((trace, l7))
        })
        .await
        .and_then(|(trace, l7)| {
            let store = Store::open(db_for_task.as_deref())?;
            let run_id = store.save_run_with_meta(
                &args_clone.target,
                trace.as_ref(),
                l7.as_ref(),
                Some(&meta),
            )?;
            if let Some(name) = &save_baseline {
                store.tag_baseline(&run_id, name)?;
            }
            let mut run = store.get_run(&run_id)?;
            run.meta = Some(meta);
            let diff = maybe_compare(&store, &run, compare.as_deref())?;
            check_thresholds(&run, &args_clone)?;
            let _ = progress_tx.send(ProgressEvent::Done);
            Ok((run, diff, save_baseline))
        });
        let _ = result_tx.send(outcome);
    });

    let res = tui::run_tui_with_progress(
        progress_rx,
        result_rx,
        ui.theme,
        target,
        db_path.as_deref(),
    )
    .await
    .map_err(Error::Io)?;

    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

async fn probe_pair(
    args: &RunArgs,
    progress: Option<traceroute::ProgressTx>,
) -> Result<(
    Option<crate::traceroute::TraceResult>,
    Option<crate::l7::L7Metrics>,
)> {
    // L7 first so the Request journey checklist ticks immediately in the TUI;
    // traceroute (often slower) runs afterward.
    let l7 = if args.skip_http {
        None
    } else {
        debug!("running L7 HTTP probe");
        let cfg = L7Config {
            timeout: args.timeout.max(Duration::from_secs(10)),
            ..L7Config::default()
        };
        Some(l7::probe_with_progress(&args.target, cfg, progress.clone()).await?)
    };

    let trace = if args.skip_trace {
        if let Some(tx) = &progress {
            let _ = tx.send(ProgressEvent::TraceSkipped {
                reason: "--skip-trace".into(),
            });
        }
        None
    } else {
        debug!("running L3/L4 traceroute");
        let dest_port = args
            .probe_port
            .unwrap_or_else(|| traceroute::infer_dest_port(&args.target));
        let cfg = TraceConfig {
            max_ttl: args.max_ttl,
            probes_per_hop: args.probes,
            timeout: args.timeout,
            probe_kind: args.probe.to_probe_kind(),
            dest_port,
            enrich: !args.no_enrich,
            flows: args.probe_flows.max(1),
            ..TraceConfig::default()
        };
        match traceroute::trace_with_progress(&args.target, cfg, progress.clone()).await {
            Ok(t) => Some(t),
            Err(e) => {
                if let Some(tx) = &progress {
                    let _ = tx.send(ProgressEvent::TraceSkipped {
                        reason: e.to_string(),
                    });
                }
                debug!(error = %e, "traceroute skipped");
                None
            }
        }
    };

    Ok((trace, l7))
}

fn maybe_compare(
    store: &Store,
    run: &StoredRun,
    name: Option<&str>,
) -> Result<Option<DiffReport>> {
    let Some(name) = name else {
        return Ok(None);
    };
    let baseline = store.get_baseline(name)?;
    Ok(Some(diff_runs(
        &baseline,
        run,
        Some(name.to_string()),
        &DiffThresholds::default(),
    )))
}

fn check_thresholds(run: &StoredRun, args: &RunArgs) -> Result<()> {
    let Some(l7) = &run.l7 else {
        return Ok(());
    };
    if let (Some(limit), Some(actual)) = (args.fail_if_ttfb_exceeds, l7.ttfb_ms) {
        let limit_ms = limit.as_secs_f64() * 1000.0;
        if actual > limit_ms {
            return Err(Error::ThresholdExceeded {
                metric: "ttfb".into(),
                actual: format!("{actual:.2}ms"),
                limit: format!("{limit_ms:.2}ms"),
            });
        }
    }
    if let (Some(limit), Some(actual)) = (args.fail_if_handshake_exceeds, l7.tcp_ms) {
        let limit_ms = limit.as_secs_f64() * 1000.0;
        if actual > limit_ms {
            return Err(Error::ThresholdExceeded {
                metric: "tcp_handshake".into(),
                actual: format!("{actual:.2}ms"),
                limit: format!("{limit_ms:.2}ms"),
            });
        }
    }
    if let (Some(limit), Some(actual)) = (args.fail_if_dns_exceeds, l7.dns_ms) {
        let limit_ms = limit.as_secs_f64() * 1000.0;
        if actual > limit_ms {
            return Err(Error::ThresholdExceeded {
                metric: "dns".into(),
                actual: format!("{actual:.2}ms"),
                limit: format!("{limit_ms:.2}ms"),
            });
        }
    }
    Ok(())
}

pub fn emit(
    run: &StoredRun,
    diff: Option<&DiffReport>,
    output: OutputFormat,
    theme: &Theme,
    db: Option<&Path>,
) -> Result<()> {
    match output {
        OutputFormat::Json => {
            println!("{}", tui::render_json(run, diff)?);
        }
        OutputFormat::Text => {
            print!("{}", tui::render_text(run, diff));
        }
        OutputFormat::Tui => {
            let view = AppView::from_run(run, diff.cloned(), None);
            if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                print!("{}", tui::render_text(run, diff));
            } else {
                tui::run_tui(view, theme.clone(), db).map_err(Error::Io)?;
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub fn exit_for(err: &Error) -> ExitCode {
    match err {
        Error::ThresholdExceeded { .. } => ExitCode::from(1),
        _ => ExitCode::FAILURE,
    }
}

#[allow(dead_code)]
pub fn diff_against_baseline(
    store: &Store,
    baseline_name: &str,
    current: &StoredRun,
) -> Result<DiffReport> {
    let baseline = store.get_baseline(baseline_name)?;
    Ok(diff_runs(
        &baseline,
        current,
        Some(baseline_name.to_string()),
        &DiffThresholds::default(),
    ))
}
