//! `trace-diff features` — auto-detect site features and run interactive checks.

use crate::ai::{print_llm_check, print_llm_check_json, resolve_ai_config, AiResolution};
use crate::cli::run::UiOpts;
use crate::cli::FeaturesArgs;
use crate::error::Result;
use crate::features::{
    self, AuthProfile, DiscoverOptions, FeatureKind, FeatureResult, FeatureRunReport,
    ProbeSettings, ProbeVerdict,
};
use std::time::Duration;

pub async fn execute(args: FeaturesArgs, ui: UiOpts) -> Result<()> {
    let resolution = resolve_ai_resolution(&args).await?;

    if args.check_llm {
        if args.json {
            print_llm_check_json(&resolution)?;
        } else {
            print_llm_check(&resolution);
        }
        return Ok(());
    }

    let target = args
        .target
        .as_deref()
        .ok_or_else(|| crate::error::Error::Other("target URL required".into()))?;

    let timeout = args.timeout.unwrap_or(Duration::from_secs(15));
    let discover = build_discover_options(&args, resolution).await?;
    let settings = build_probe_settings(&args, timeout)?;

    let headless = args.yes_all || !std::io::IsTerminal::is_terminal(&std::io::stdout());
    if headless {
        return run_headless(target, discover, &settings, &args).await;
    }

    features::run_features_interactive(target, ui.theme, settings, discover).await
}

async fn run_headless(
    target: &str,
    discover: DiscoverOptions<'_>,
    settings: &ProbeSettings,
    args: &FeaturesArgs,
) -> Result<()> {
    let outcome = features::discover_features(target, discover).await?;
    print_llm_hint(&outcome.llm);
    if outcome.features.is_empty() {
        return Err(crate::error::Error::Other(
            "no features discovered — check URL or OpenAPI availability".into(),
        ));
    }
    let chosen = select_ci_features(&outcome.features, args);
    if chosen.is_empty() {
        return Err(crate::error::Error::Other(
            "no features selected after filters — try --include-writes or --manifest".into(),
        ));
    }
    let report = features::run_selected(target, &chosen, settings).await?;
    print_headless_report(&report, args.json);
    ci_gate(&report.results, args)
}

fn print_headless_report(report: &FeatureRunReport, json: bool) {
    if json {
        if let Ok(text) = serde_json::to_string_pretty(report) {
            println!("{text}");
        }
        return;
    }
    println!(
        "Feature run: {}/{} passed (discovered {})\n",
        report.passed, report.selected, report.discovered
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
        on_progress: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{DetectedFeature, FeatureKind, FeatureResult, ProbeVerdict};

    fn stub_result(verdict: ProbeVerdict, ttfb_ms: Option<f64>) -> FeatureResult {
        FeatureResult {
            feature: DetectedFeature {
                id: "health".into(),
                label: "health".into(),
                url: "https://example.com/health".into(),
                kind: FeatureKind::Api,
                source: "test".into(),
                method: None,
                workflow: None,
            },
            ok: verdict != ProbeVerdict::Failed,
            verdict,
            status: Some(200),
            total_ms: 50.0,
            ttfb_ms,
            message: "ok".into(),
            l7: None,
            steps: vec![],
        }
    }

    fn gate_args(fail_on_reachable: bool, ttfb_limit: Option<Duration>) -> FeaturesArgs {
        FeaturesArgs {
            target: Some("https://example.com".into()),
            yes_all: true,
            json: false,
            max_features: None,
            timeout: None,
            manifest: None,
            bearer_token: None,
            email: None,
            password: None,
            auth_file: None,
            include_writes: false,
            fail_on_reachable,
            fail_if_ttfb_exceeds: ttfb_limit,
            cert_warn_days: 21,
            no_tls_canary: true,
            no_llm: true,
            check_llm: false,
            llm_provider: "auto".into(),
            llm_model: None,
            ollama_host: "http://localhost:11434".into(),
            groq_api_key: None,
            llm_timeout_secs: 30,
        }
    }

    #[test]
    fn ci_gate_fails_on_failed_probe() {
        let results = vec![stub_result(ProbeVerdict::Failed, None)];
        let err = ci_gate(&results, &gate_args(false, None)).unwrap_err();
        assert!(err.to_string().contains("failed"));
    }

    #[test]
    fn ci_gate_fails_on_reachable_when_flag_set() {
        let results = vec![stub_result(ProbeVerdict::Reachable, None)];
        let err = ci_gate(&results, &gate_args(true, None)).unwrap_err();
        assert!(err.to_string().contains("reachable"));
    }

    #[test]
    fn ci_gate_fails_on_slow_ttfb() {
        let results = vec![stub_result(ProbeVerdict::Healthy, Some(500.0))];
        let err = ci_gate(
            &results,
            &gate_args(false, Some(Duration::from_millis(100))),
        )
        .unwrap_err();
        assert!(err.to_string().contains("TTFB exceeded"));
    }

    #[test]
    fn ci_gate_passes_healthy_probes() {
        let results = vec![stub_result(ProbeVerdict::Healthy, Some(50.0))];
        assert!(ci_gate(&results, &gate_args(true, Some(Duration::from_secs(1)))).is_ok());
    }
}
