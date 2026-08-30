//! `trace-diff features` — auto-detect site features and run interactive checks.

use crate::ai::{llm_available, resolve_ollama_model, AiConfig, AiProvider};
use crate::cli::run::UiOpts;
use crate::cli::FeaturesArgs;
use crate::error::Result;
use crate::features::{
    self, AuthProfile, DiscoverOptions, FeatureKind, FeatureResult, ProbeSettings, ProbeVerdict,
};
use std::time::Duration;

pub async fn execute(args: FeaturesArgs, ui: UiOpts) -> Result<()> {
    let timeout = args.timeout.unwrap_or(Duration::from_secs(15));
    let discover = build_discover_options(&args).await?;
    let settings = build_probe_settings(&args, timeout)?;

    if args.yes_all {
        let feats = features::discover_features(&args.target, discover).await?;
        let chosen = select_ci_features(&feats, &args);
        let report = features::run_selected(&args.target, &chosen, &settings).await?;
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

    features::run_features_interactive(&args.target, ui.theme, settings, discover).await
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

async fn build_discover_options(args: &FeaturesArgs) -> Result<DiscoverOptions<'_>> {
    let infer_workflows = !args.no_llm;
    let llm = if infer_workflows {
        let provider = AiProvider::parse(&args.llm_provider)?;
        let model = args
            .llm_model
            .clone()
            .unwrap_or_else(|| provider.default_model().to_string());
        let mut cfg = AiConfig {
            provider,
            model,
            ollama_host: args.ollama_host.clone(),
            groq_api_key: args.groq_api_key.clone(),
            timeout_secs: args.llm_timeout_secs,
        };
        if cfg.provider == AiProvider::Ollama && llm_available(&cfg).await {
            cfg.model = resolve_ollama_model(&cfg).await.unwrap_or(cfg.model);
        }
        Some(cfg)
    } else {
        None
    };

    Ok(DiscoverOptions {
        manifest: args.manifest.as_deref(),
        llm,
        infer_workflows,
        skip_tls_canary: args.no_tls_canary,
    })
}
