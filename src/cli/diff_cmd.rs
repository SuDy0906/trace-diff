//! `trace-diff diff` command.

use crate::cli::run::UiOpts;
use crate::cli::{effective_output, DiffArgs};
use crate::diff::{diff_runs, DiffThresholds};
use crate::error::{Error, Result};
use crate::l7::{self, L7Config};
use crate::meta::{self, PrivilegeLevel, RunMetadata};
use crate::store::Store;
use crate::traceroute::{self, TraceConfig};
use tracing::debug;

pub async fn execute(args: DiffArgs, ui: UiOpts) -> Result<()> {
    let store = Store::open(args.db.as_deref())?;
    let output = effective_output(args.output, args.headless);
    let baseline = store.get_baseline(&args.baseline)?;

    let current = if let Some(target) = &args.target {
        debug!(%target, "re-probing for live diff");
        let privileges = if args.skip_trace {
            PrivilegeLevel::Unknown
        } else {
            meta::detect_icmp_privilege()
        };
        let meta = RunMetadata::capture(args.skip_trace, args.skip_http, privileges);

        let trace = if args.skip_trace {
            None
        } else {
            match traceroute::trace(target, TraceConfig::default()).await {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::debug!(error = %e, "traceroute skipped");
                    None
                }
            }
        };
        let l7 = if args.skip_http {
            None
        } else {
            Some(l7::probe(target, L7Config::default()).await?)
        };
        let id = store.save_run_with_meta(target, trace.as_ref(), l7.as_ref(), Some(&meta))?;
        let mut run = store.get_run(&id)?;
        run.meta = Some(meta);
        run
    } else {
        store
            .latest_run()?
            .ok_or_else(|| Error::Other("no runs stored — provide a target to probe".into()))?
    };

    let report = diff_runs(
        &baseline,
        &current,
        Some(args.baseline.clone()),
        &DiffThresholds::default(),
    );

    crate::cli::run::emit(
        &current,
        Some(&report),
        output,
        &ui.theme,
        args.db.as_deref(),
    )?;

    if report
        .regressions
        .iter()
        .any(|r| matches!(r.severity, crate::diff::Severity::Critical))
    {
        return Err(Error::Other(
            "critical regressions detected vs baseline".into(),
        ));
    }
    Ok(())
}
