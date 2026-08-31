# v1.0 definition of done

Use this checklist before tagging `1.0.0`. Items marked **done** reflect the current repo state; re-verify before release.

## Packaging & distribution

- [x] MIT `LICENSE`, `CHANGELOG.md` (Keep a Changelog)
- [x] PyPI metadata: platform classifiers, project URLs, Python 3.9–3.12
- [x] `maturin`/wheel CI on Ubuntu with multi-platform smoke tests
- [ ] Signed release artifacts / checksums published on GitHub Releases

## CI & quality

- [x] `cargo test`, `cargo clippy -- -D warnings` in CI
- [x] Strict diff gate (no `|| true` fallback)
- [x] `cargo audit` job
- [x] Features headless wiremock integration tests
- [x] CI gate tests (`--fail-on-reachable`, TTFB limit)
- [x] LLM provider resolution tests (Ollama mock)

## Features UX

- [x] Non-TTY stdout auto headless (`-y` behavior)
- [x] Discovery sub-progress in TUI
- [x] LLM status panel (`l`), rediscover (`R`)
- [x] Error recovery hints (manifest, `--no-llm`, retry)
- [x] Confirm quit during probe run (double `q`)
- [x] Auth popup: env var names shown (values never displayed)
- [x] Help (`?`), guide (`g`), export (`e`), no-color labels

## Observability & scripting

- [x] `trace-diff features --check-llm` (human + `--json`)
- [x] `info!` discovery phase logging (`RUST_LOG=info`)
- [ ] Structured JSON logs for CI parsers (optional)

## Documentation

- [x] README covers `run`, `diff`, and `features` equally
- [x] `docs/INSTALL.md`, `docs/CI.md`, `docs/TROUBLESHOOTING.md`
- [x] `docs/LLM_SETUP.md`, `docs/SECURITY.md` (Groq disclosure)
- [x] Issue templates (bug, feature)

## Semver policy

- **0.x** — API/CLI flags may change; document in CHANGELOG
- **1.0** — Stable CLI surface for `run`, `diff`, `features`; breaking changes only in major releases
- Deprecations: one minor release warning before removal

## Release sign-off

1. Bump version in `Cargo.toml`, `pyproject.toml`, CHANGELOG `[Unreleased]` → `[x.y.z]`
2. Full CI green on `main`
3. Manual smoke: TUI features on a real OpenAPI host, `--check-llm --json`, pip install wheel
4. Tag and publish PyPI + GitHub release notes
