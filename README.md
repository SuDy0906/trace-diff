# trace-diff

**See why a site or API is slow — in your terminal.**

Install once with pip, run two commands, get a live dashboard: hop-by-hop network path, HTTP timing (DNS → TLS → first byte), and optional API workflow smoke tests from OpenAPI.

```bash
pip install trace-route-test
```

Package on PyPI: **`trace-route-test`** · CLI: **`trace-diff`** · Python 3.9+

---

## Everyday use (interactive)

Open a terminal and run a command **without extra flags**. You get the **TUI** — live progress, colored bars, keyboard shortcuts. No JSON, no scripts.

### “Why is this site slow?”

```bash
trace-diff run https://example.com
```

Shows where time goes: DNS, TCP connect, TLS, time-to-first-byte (TTFB), download — plus the network path hop-by-hop.

**Tip:** On Windows, run PowerShell **as Administrator** for full traceroute. For HTTP timing only (no admin):

```bash
trace-diff run https://example.com --skip-trace
```

### “Does my API work?”

Point at your **API host** (the one that serves OpenAPI), not the marketing website:

```bash
trace-diff features https://api.example.com
```

1. Discovers workflow scenarios (health checks, login flows, tagged endpoints)
2. Lets you pick what to run (Space to toggle, Enter to run)
3. Scores each step: green pass, yellow needs auth, red failed

**Tip:** If you only see basic pages (`/health`, sitemap links), switch to `https://api.yoursite.com` — that’s where OpenAPI lives.

### Optional: smarter API grouping (LLM)

Heuristics work with **no API key**. For nicer workflow grouping, set `GROQ_API_KEY` or run Ollama locally — see [docs/LLM_SETUP.md](docs/LLM_SETUP.md).

Press **`l`** in the features TUI to check LLM status.

### Keyboard shortcuts

**`run` results**

| Key | Action |
|-----|--------|
| `?` | Help |
| `b` | Compare to a saved baseline |
| `e` | Export JSON report |
| `t` | Change theme |
| `q` / Esc | Quit |

**`features` select screen**

| Key | Action |
|-----|--------|
| ↑↓ / j k | Move |
| Space | Toggle scenario |
| Enter / r | Run selected |
| c | Auth credentials |
| d / i | Inspect steps |
| l | LLM status |
| R | Rediscover |
| `?` / g | Help / quick guide |
| q / Esc | Quit |

During a probe run, press **`q` twice** to confirm abort.

`--no-color` or `NO_COLOR` disables colors; `--theme ocean|amber|mono` picks a palette.

---

## For developers & CI (scripts and automation)

Use these when you want **JSON output**, **pipelines**, or **no TUI** — not for your first try.

### Headless network probe

```bash
trace-diff run https://api.example.com --skip-trace --headless
trace-diff run https://api.example.com --headless --save-baseline staging
trace-diff diff staging https://api.example.com --output json
trace-diff run https://api.example.com --headless --fail-if-ttfb-exceeds 250ms
```

### Headless API workflows

```bash
trace-diff features https://api.example.com -y --json
trace-diff features --check-llm --json
trace-diff features https://api.example.com -y --no-llm --fail-on-reachable
```

`-y` skips the TUI. Piped/non-TTY stdout auto-runs headless (same as `-y`).

Auth in CI: env vars (`TRACE_DIFF_EMAIL`, `TRACE_DIFF_PASSWORD`, …) or `--auth-file auth.json`. See [docs/FEATURES_AUTODETECT.md](docs/FEATURES_AUTODETECT.md).

### Build from source (Rust)

```bash
cargo build --release
cargo run -- run https://example.com
cargo run -- features https://api.example.com
```

---

## What each mode does

| Mode | Command | Best for |
|------|---------|----------|
| **Network probe** | `trace-diff run <url>` | Slow site/API — split network vs server time |
| **API features** | `trace-diff features <api-url>` | OpenAPI smoke tests, auth flows, TLS check |
| **Baseline diff** | `trace-diff diff <name> <url>` | “Did latency or route change since last week?” |

Both modes ship in the same pip package. No Rust toolchain required for pip users.

---

## Docs

- [Install (Windows / macOS / Linux)](docs/INSTALL.md)
- [Feature auto-detect](docs/FEATURES_AUTODETECT.md) — discovery, auth, scorecard
- [PyPI / pip details](docs/PYPI.md)
- [CI integration](docs/CI.md) — GitHub Actions, exit codes
- [LLM setup (optional)](docs/LLM_SETUP.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)
- [How to read a diff](docs/HOW_TO_READ_A_DIFF.md)
- [Changelog](CHANGELOG.md)

---

## Permissions

| Platform | Full traceroute | HTTP only (`--skip-trace`) |
|----------|-----------------|----------------------------|
| Windows | Run terminal as **Administrator** | Normal user |
| macOS | Often needs `sudo` | Normal user |
| Linux | `sudo setcap cap_net_raw+ep $(which trace-diff)` | Normal user |

`features` uses HTTP only — no admin needed.

---

## All commands

**Network:** `run`, `diff`, `baseline`, `list`  
**API:** `features`, `features --check-llm`, `features --auth-file …`, `features --no-llm`

Full reference: [docs/FEATURES.md](docs/FEATURES.md)
