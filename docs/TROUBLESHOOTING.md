# Troubleshooting

Common issues after `pip install trace-route-test`.

## Install and PATH

**`trace-diff: command not found`**

- Activate your venv first: `source .venv/bin/activate` (macOS/Linux) or `.\.venv\Scripts\Activate.ps1` (Windows)
- Or use: `python -m trace_diff` if the launcher package is installed
- Verify: `pip show trace-route-test` and check `Scripts/` or `bin/` on PATH

**No wheel for my platform**

Published wheels: Windows x86_64, macOS arm64 + x86_64, Linux x86_64 (glibc/manylinux 2_28).

Not supported via pip wheels today:

- Linux **aarch64** (ARM servers) — build from source with `cargo build --release`
- **Alpine / musl** — glibc wheels won't install; build from source
- Windows **arm64** — build from source

## Network probe (`run`)

**Traceroute fails / no hops**

| Platform | Fix |
|----------|-----|
| Windows | Run terminal **as Administrator** |
| macOS | Try `sudo trace-diff run ...` |
| Linux | `sudo setcap cap_net_raw+ep $(which trace-diff)` or use `sudo` |

**L7-only without admin:** add `--skip-trace` — HTTP probing still works.

**Diff shows unexpected TTFB changes**

- Timing uses monotonic clocks but network variance is normal — see [TIMING.md](TIMING.md)
- Compare same network path and time of day; use baselines as trends, not absolutes

## Features (`features`)

**Empty or few workflow rows**

- Target must publish **OpenAPI** at `/openapi.json` or similar — check in browser
- Try `--no-llm` if LLM refine times out (heuristics still run)
- Force refresh: delete `.trace-diff/workflows-<host>.json`
- Manual list: `--manifest workflows.json` — see [FEATURES_AUTODETECT.md](FEATURES_AUTODETECT.md)

**Yellow Reachable rows**

Route exists but auth/body missing. Set credentials:

```bash
export TRACE_DIFF_EMAIL="user@example.com"
export TRACE_DIFF_PASSWORD="..."
trace-diff features https://api.example.com
```

Or `--auth-file auth.json` for multi-realm (user / annotator / admin).

**Auth popup keeps appearing**

Press `c` on the Select screen to edit. Uncheck realms you don't need (Space on realm row). Leave optional fields blank to skip.

## LLM setup

**`trace-diff features --check-llm` shows `heuristics only`**

Normal — heuristics work without LLM. To enable refine:

```bash
export GROQ_API_KEY="gsk_..."   # https://console.groq.com
trace-diff features --check-llm
```

Or install [Ollama](https://ollama.com) and `ollama pull qwen2.5:7b-instruct`.

**Groq HTTP 401**

Invalid or expired API key. Regenerate at console.groq.com.

**Ollama unreachable**

Start Ollama app/service. Check `OLLAMA_HOST` (default `http://localhost:11434`).

See [LLM_SETUP.md](LLM_SETUP.md).

## TUI / terminal

**Garbled Unicode / broken layout**

- Widen terminal (80+ columns recommended)
- Try `--no-color` or `NO_COLOR=1`
- Windows: use Windows Terminal instead of legacy conhost

**TUI doesn't start in CI/script**

Use headless modes:

```bash
trace-diff run https://example.com --skip-trace --headless
trace-diff features https://api.example.com -y --json
```

## Getting help

1. Run with verbose logs: `trace-diff -v features https://api.example.com`
2. Include `trace-diff --version`, OS, and install method in [GitHub issues](https://github.com/SuDy0906/trace-diff/issues)
