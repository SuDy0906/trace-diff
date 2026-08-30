//! `trace-diff features` — auto-detect site features and run interactive checks.

use crate::ai::{print_llm_check, resolve_ai_config, AiResolution};
use crate::cli::run::UiOpts;
use crate::cli::FeaturesArgs;
use crate::error::Result;
use crate::features::{
    self, AuthProfile, DiscoverOptions, FeatureKind, FeatureResult, ProbeSettings, ProbeVerdict,
};
use std::time::Duration;

pub async fn execute(args: FeaturesArgs, ui: UiOpts) -> Result<()> {
    let resolution = resolve_ai_resolution(&args).await?;

    if args.check_llm {
        print_llm_check(&resolution);
        return Ok(());
    }

    let target = args
        .target
        .as_deref()
        .ok_or_else(|| crate::error::Error::Other("target URL required".into()))?;

    let timeout = args.timeout.unwrap_or(Duration::from_secs(15));
    let discover = build_discover_options(&args, resolution).await?;
    let settings = build_probe_settings(&args, timeout)?;

    if args.yes_all {
        let outcome = features::discover_features(target, discover).await?;
        print_llm_hint(&outcome.llm);
        let chosen = select_ci_features(&outcome.features, &args);
        let report = features::run_selected(target, &chosen, &settings).await?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!(
                "Feature run: {}/{} passed (of {} selected)\n",
                report.passed, report.selected, report.selected
            );
            for r in &report.results {
                let mark = match r.verdict {
                    ProbeVerdict::Healthy => "PASS",
                    ProbeVerdict::Reachable => "REACH",
                    ProbeVerdict::Failed => "FAIL",
                };
                println!(
                    "  [{mark}] {:<22} {:>7.0} ms  {:<24} {}",
                    r.feature.label, r.total_ms, r.message, r.feature.url
                );
            }
        }
        ci_gate(&report.results, &args)?;
        return Ok(());
    }

    features::run_features_interactive(target, ui.theme, settings, discover).await
}

async fn resolve_ai_resolution(args: &FeaturesArgs) -> Result<AiResolution> {
    resolve_ai_config(
        &args.llm_provider,
        args.llm_model.clone(),
        args.ollama_host.clone(),
        args.groq_api_key.clone(),
        args.llm_timeout_secs,
    )
    .await
}

fn print_llm_hint(llm: &features::LlmDiscoveryStatus) {
    if let Some(hint) = llm.stderr_hint() {
        eprintln!("{hint}");
    }
}

fn build_probe_settings(args: &FeaturesArgs, timeout: Duration) -> Result<ProbeSettings> {
    let mut auth = AuthProfile::from_env(args.bearer_token.clone());
    if let Some(path) = &args.auth_file {
        auth = auth.merge_file(path)?;
    }
    auth = auth.with_cli(args.email.clone(), args.password.clone());
    let mut settings = ProbeSettings::new(timeout, auth);
    settings.cert_warn_days = args.cert_warn_days.max(0);
    Ok(settings)
}

fn select_ci_features(
    feats: &[features::DetectedFeature],
    args: &FeaturesArgs,
) -> Vec<features::DetectedFeature> {
    let max = args.max_features.unwrap_or(64) as usize;
    let mut tls = Vec::new();
    let mut rest = Vec::new();
    for f in feats {
        if !args.include_writes && features::is_write_flow(f) {
            continue;
        }
        if f.id == "favicon" {
            continue;
        }
        let keep = matches!(
            f.kind,
            FeatureKind::Page | FeatureKind::Api | FeatureKind::Workflow | FeatureKind::Tls
        );
        if !keep {
            continue;
        }
        if f.kind == FeatureKind::Tls {
            tls.push(f.clone());
        } else {
            rest.push(f.clone());
        }
    }
    rest.truncate(max);
    tls.append(&mut rest);
    tls
}

fn ci_gate(results: &[FeatureResult], args: &FeaturesArgs) -> Result<()> {
    let failed = results
        .iter()
        .filter(|r| r.verdict == ProbeVerdict::Failed)
        .count();
    if failed > 0 {
        return Err(crate::error::Error::Other(format!(
            "{failed} feature(s) failed"
        )));
    }

    if args.fail_on_reachable {
        let reachable = results
            .iter()
            .filter(|r| r.verdict == ProbeVerdict::Reachable)
            .count();
        if reachable > 0 {
            return Err(crate::error::Error::Other(format!(
                "{reachable} feature(s) reachable (auth/body required)"
            )));
        }
    }

    if let Some(limit) = args.fail_if_ttfb_exceeds {
        let limit_ms = limit.as_secs_f64() * 1000.0;
        let slow: Vec<_> = results
            .iter()
            .filter_map(|r| {
                let ms = features::result_ttfb_ms(r)?;
                (ms > limit_ms).then_some((r.feature.label.clone(), ms))
            })
            .collect();
        if !slow.is_empty() {
            let detail = slow
                .iter()
                .map(|(label, ms)| format!("{label} {ms:.0}ms"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(crate::error::Error::Other(format!(
                "TTFB exceeded {} ({detail})",
                humantime::format_duration(limit)
            )));
        }
    }

    Ok(())
}

async fn build_discover_options(
    args: &FeaturesArgs,
    resolution: AiResolution,
) -> Result<DiscoverOptions<'_>> {
    let infer_workflows = !args.no_llm;
    let llm = if infer_workflows {
        resolution.config.clone()
    } else {
        None
    };

    Ok(DiscoverOptions {
        manifest: args.manifest.as_deref(),
        llm,
        ai_resolution: if infer_workflows {
            Some(resolution)
        } else {
            None
        },
        infer_workflows,
        skip_tls_canary: args.no_tls_canary,
    })
}
