# trace-diff

Interactive, terminal-native diagnostic CLI with two main modes:

1. **Network probe** — Layer 3/4 hop-by-hop traceroute + Layer 7 HTTP connection lifecycle breakdown, with SQLite baselines for regression detection.
2. **Features** — Auto-discover API workflow scenarios from OpenAPI, run multi-step auth flows interactively or in CI, and score each endpoint (health, latency, TLS).

Both ship in the same `pip install trace-route-test` binary. No Rust toolchain required for pip users.

## Quick start

### pip (recommended)

Requires Python 3.9+.

```bash
pip install trace-route-test
trace-diff --help

# Network probe (HTTP only, no admin)
trace-diff run https://example.com --skip-trace --headless

# API workflow discovery + interactive TUI
trace-diff features https://api.example.com
```

Package on PyPI: **`trace-route-test`** → CLI: **`trace-diff`**.  
Install guide: [docs/INSTALL.md](docs/INSTALL.md) · [PyPI details](docs/PYPI.md)

### Network probe (`run` / `diff`)

Trace routes and measure HTTP timing (DNS → TCP → TLS → TTFB). Save baselines and diff later runs to catch regressions.

```bash
trace-diff run https://example.com --skip-trace --headless
trace-diff run https://api.example.com --headless --save-baseline staging
trace-diff diff staging https://api.example.com --output json
trace-diff run https://api.example.com --headless --fail-if-ttfb-exceeds 250ms
```

Works without elevated privileges when using `--skip-trace` (L7 HTTP only). ICMP traceroute may need Admin/sudo — see [Permissions](#permissions).

### Features (`features`)

Point at an API host with a published OpenAPI spec. `trace-diff` discovers **workflow scenarios** (health smoke, auth chains, tag-grouped GET flows, optional write smokes), lets you select them in a TUI, and runs each step with pass/reach/fail scoring.

```bash
trace-diff features https://api.example.com
trace-diff features --check-llm                    # verify optional LLM provider
trace-diff features https://api.example.com -y --json   # headless CI run
```

**Discovery pipeline:** fetch OpenAPI → heuristic workflow grouping (instant, no API key) → optional LLM refine (Groq or Ollama) → TLS cert canary → interactive scorecard.

**Auth:** multi-realm support (user / annotator / admin) via env vars, `--auth-file`, or the in-TUI auth popup. Yellow **Reachable** rows mean the route exists but needs credentials.

**Optional LLM:** heuristics work out of the box. For smarter grouping, set `GROQ_API_KEY` ([console.groq.com](https://console.groq.com)) or run Ollama locally. See [docs/LLM_SETUP.md](docs/LLM_SETUP.md).

Full guide: [docs/FEATURES_AUTODETECT.md](docs/FEATURES_AUTODETECT.md)

### From source (developers)

```bash
cargo build --release

# Full probe (TUI) — live progress, then results
cargo run -- run https://example.com

# Headless / CI JSON
cargo run -- run https://example.com --headless --save-baseline staging

# Diff against baseline
cargo run -- diff staging https://example.com --output json

# Fail CI if TTFB exceeds a hard limit
cargo run -- run https://api.example.com --headless --fail-if-ttfb-exceeds 250ms

# Verbose / debug traces on stderr
cargo run -- -v run https://example.com --skip-trace --output text
```

### TUI keys (`run`)

| Key | Action |
|---|---|
| `?` | Help |
| `b` | Select baseline to diff |
| `e` | Export JSON report |
| `t` | Cycle theme |
| `q` / Esc | Quit |

### TUI keys (`features`)

| Key | Action |
|---|---|
| ↑↓ / Space | Move and toggle features |
| Enter | Run selected features |
| `d` / `i` | Inspect workflow steps |
| `c` | Edit auth credentials |
| `a` / `n` | Select all / none |
| `R` | Categorized run report (after run) |
| `q` / Esc | Quit |

`--no-color` / `NO_COLOR` disables colors; `--theme ocean|amber|mono` selects a palette.

## Docs

- [**Feature auto-detect**](docs/FEATURES_AUTODETECT.md) — discover pages/APIs, prompt, scorecard
- [**LLM setup (optional)**](docs/LLM_SETUP.md) — Groq or Ollama for smarter workflows
- [**Feature reference**](docs/FEATURES.md) — full catalog of commands, probes, TUI, baselines, CI
- [Problem & technical proposal](docs/PROBLEM_AND_TECHNICAL_PROPOSAL.md)
- [Install (Windows / macOS / Linux)](docs/INSTALL.md)
- [CI integration](docs/CI.md) — GitHub Actions, exit codes, JSON reports
- [Troubleshooting](docs/TROUBLESHOOTING.md)
- [Changelog](CHANGELOG.md)
- [How to read a diff](docs/HOW_TO_READ_A_DIFF.md)
- [Security model](docs/SECURITY.md)
- [Timing / clock disclaimer](docs/TIMING.md)
- Linux netns testbed: `scripts/netns_netem_testbed.sh`

## Permissions

After `pip install trace-route-test`, use the `trace-diff` command (see [INSTALL.md](docs/INSTALL.md)).

| Platform | Notes |
|---|---|
| Linux | `sudo setcap cap_net_raw+ep $(which trace-diff)` for traceroute without root |
| macOS | May require `sudo trace-diff ...` for raw ICMP |
| Windows | Run terminal **as Administrator** for ICMP traceroute |

L7 HTTP probing works without elevated privileges (`--skip-trace`).

## Commands

**Network probe**

- `run <target>` — L3/L4 traceroute + L7 HTTP probe
- `baseline tag|delete|show` — manage named baselines
- `diff <baseline> [target]` — compare against baseline
- `list` — list runs and baselines

**API features**

- `features <url>` — discover OpenAPI workflows, interactive TUI select + run
- `features <url> -y` — headless auto-run (CI)
- `features <url> -y --json` — JSON report to stdout
- `features --check-llm` — print LLM provider status (Groq/Ollama)
- `features <url> --auth-file auth.json` — multi-realm credentials
- `features <url> --no-llm` — skip workflow pipeline (flat endpoint list)
