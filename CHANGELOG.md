# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Production packaging: LICENSE, CHANGELOG, PyPI metadata URLs, GitHub issue templates
- CI: strict diff gate, `cargo audit`, PR wheel build validation, multi-platform wheel smoke tests
- Docs: [TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md), [CI.md](docs/CI.md)
- Features TUI: `?` help, `g` guide, `e` JSON export, non-TTY text fallback
- Integration test: `features -y` against wiremock OpenAPI fixture
- README and PyPI docs expanded with `features` command details

### Changed

- PyPI classifiers: platform-specific wheels (removed misleading OS Independent)

## [0.1.0] - 2026-08-30

### Added

- Initial PyPI release as **`trace-route-test`** (CLI: **`trace-diff`**)
- Network probe: L3/L4 traceroute + L7 HTTP timing, SQLite baselines, diff
- Features: OpenAPI workflow discovery, multi-realm auth, interactive TUI, CI mode (`-y`)
- Optional LLM refine (Groq/Ollama) with `--check-llm` and `auto` provider resolution
- Wheels: Windows x86_64, macOS arm64/x86_64, Linux x86_64 (manylinux 2_28)

[Unreleased]: https://github.com/SuDy0906/trace-diff/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/SuDy0906/trace-diff/releases/tag/v0.1.0
