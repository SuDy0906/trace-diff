# trace-diff

Interactive, terminal-native diagnostic CLI: Layer 3/4 hop-by-hop route tracing + Layer 7 HTTP connection lifecycle breakdown, with SQLite baselines for regression detection.

## Quick start

### pip (recommended)

Requires Python 3.9+. No Rust toolchain needed.

```bash
pip install trace-route-test
trace-diff --help
trace-diff run https://example.com --skip-trace --headless
trace-diff features https://api.example.com
```

Package on PyPI: **`trace-route-test`** → CLI: **`trace-diff`**.  
Install guide: [docs/INSTALL.md](docs/INSTALL.md) · [PyPI details](docs/PYPI.md)

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

### TUI keys

| Key | Action |
|---|---|
| `?` | Help |
| `b` | Select baseline to diff |
| `e` | Export JSON report |
| `t` | Cycle theme |
| `q` / Esc | Quit |

`--no-color` / `NO_COLOR` disables colors; `--theme ocean|amber|mono` selects a palette.

## Docs

- [**Feature auto-detect**](docs/FEATURES_AUTODETECT.md) — discover pages/APIs, prompt, scorecard
- [**Feature reference**](docs/FEATURES.md) — full catalog of commands, probes, TUI, baselines, CI
- [Problem & technical proposal](docs/PROBLEM_AND_TECHNICAL_PROPOSAL.md)
- [Install (Windows / macOS / Linux)](docs/INSTALL.md)
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

- `run <target>` — L3/L4 + L7 probe
- `baseline tag|delete|show` — manage named baselines
- `diff <baseline> [target]` — compare against baseline
- `list` — list runs and baselines
