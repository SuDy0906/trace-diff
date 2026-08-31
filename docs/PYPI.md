# PyPI (pip) distribution

Install the prebuilt CLI — **no Rust or cargo required**:

```bash
pip install trace-route-test
trace-diff --help
```

| | |
|---|---|
| **PyPI package** | `trace-route-test` |
| **CLI command** | `trace-diff` |
| **Current version** | see [pypi.org/project/trace-route-test](https://pypi.org/project/trace-route-test/) |

Wheels ship the native `trace-diff` binary onto your `PATH` inside the active Python environment (venv, conda, or user site-packages).

## What you get

One binary, two main workflows:

| Mode | Command | Purpose |
|------|---------|---------|
| **Network probe** | `trace-diff run` | L3/L4 traceroute + L7 HTTP timing (DNS→TCP→TLS→TTFB), baselines, diffs |
| **API features** | `trace-diff features` | OpenAPI workflow discovery, multi-step auth flows, interactive or CI scorecard |

Both are included in every wheel. Use `--skip-trace` on `run` for HTTP-only probing without Admin/sudo.

## Install options

```bash
# Default
pip install trace-route-test

# Upgrade
pip install --upgrade trace-route-test

# User install (no venv)
pip install --user trace-route-test

# Specific version
pip install trace-route-test==0.1.0
```

Use a virtual environment when you can:

```bash
python -m venv .venv
# Windows: .venv\Scripts\activate
# macOS/Linux: source .venv/bin/activate
pip install trace-route-test
```

## Platform wheels

CI publishes wheels for:

| OS | Architecture |
|----|----------------|
| Windows | x86_64 |
| macOS | Apple Silicon (arm64) |
| macOS | Intel (x86_64) |
| Linux | x86_64 (manylinux 2_28) |

`pip` selects the matching wheel automatically. If no wheel matches your platform, `pip` will not fall back to building from source (this package ships binaries only).

## Privileges

| Platform | L3/L4 traceroute | L7 HTTP (`--skip-trace`) | `features` (HTTP probes) |
|----------|------------------|---------------------------|---------------------------|
| Windows | Administrator | works | works |
| macOS | often `sudo` | works | works |
| Linux | `setcap` or `sudo` | works | works |

See [INSTALL.md](INSTALL.md) for per-platform examples.

## Network probe (`run` / `diff`)

Trace routes and measure HTTP connection phases. Save baselines and diff later runs.

```bash
# Smoke test (no admin)
trace-diff run https://example.com --skip-trace --headless

# Save baseline + diff in CI
trace-diff run https://api.example.com --headless --save-baseline staging
trace-diff diff staging https://api.example.com --output json

# Fail pipeline if TTFB regresses
trace-diff run https://api.example.com --headless --fail-if-ttfb-exceeds 250ms

trace-diff list
```

## Features (`features`)

Auto-detect **API workflow scenarios** from a site's OpenAPI spec: health smokes, login→token→GET chains, tag-grouped flows, TLS cert canary. Interactive TUI to select and run, or headless for CI.

```bash
# Interactive — discover, select, run, scorecard
trace-diff features https://api.example.com

# Verify optional LLM setup (Groq/Ollama)
trace-diff features --check-llm

# Machine-readable LLM check (setup scripts / CI)
trace-diff features --check-llm --json

# Headless CI — JSON report, non-zero exit on failures
trace-diff features https://api.example.com -y --json

# Non-TTY stdout (pipes) auto-runs headless — same as -y
trace-diff features https://api.example.com --json | jq .passed

# With credentials (multi-realm: user / annotator / admin)
trace-diff features https://api.example.com --auth-file auth.json

# Stricter CI gates
trace-diff features https://api.example.com -y \
  --fail-on-reachable \
  --fail-if-ttfb-exceeds 250ms
```

**How discovery works**

1. Fetch OpenAPI from the target (if published)
2. Build heuristic workflow scenarios (instant — **no LLM or API key required**)
3. Optionally refine with LLM when Groq or Ollama is configured (~20s max)
4. Cache manifest to `.trace-diff/workflows-<host>.json`
5. TUI shows FLOW / WRITE / TLS rows; press `d` to inspect steps

Yellow **Reachable** means the route exists but needs auth or body. Set `TRACE_DIFF_EMAIL` / `TRACE_DIFF_PASSWORD` (or `--auth-file`) to turn flows green.

### Interactive TUI keys

| Screen | Key | Action |
|--------|-----|--------|
| Select | ↑↓ j/k, Space | Move, toggle |
| Select | Enter / r | Run selected |
| Select | a / n | All / none |
| Select | c | Auth popup |
| Select | d / i | Step inspect |
| Select | l | LLM status panel |
| Select | R | Rediscover |
| Select | ? / g | Help / guide |
| Discovery | (live) | Stage updates: Fetching OpenAPI…, Building heuristics…, LLM refine… |
| Running | q ×2 | Confirm abort |
| Results | R | Categorized report |
| Results | e | Export JSON to `.trace-diff/` |
| Any | t | Cycle theme |

`NO_COLOR=1` or `--no-color` uses text labels (`OK` / `REACH` / `FAIL`) instead of color-only glyphs.

Docs: [FEATURES_AUTODETECT.md](FEATURES_AUTODETECT.md) · [FEATURES.md](FEATURES.md) · [CI.md](CI.md)

## Optional LLM (smarter API workflows)

Heuristic workflows work without any setup. For optional LLM refine (better grouping/ordering):

```bash
export GROQ_API_KEY="gsk_..."   # free at https://console.groq.com
trace-diff features --check-llm
trace-diff features --check-llm --json   # for setup scripts
trace-diff features https://api.example.com
```

Or run [Ollama](https://ollama.com) locally. Default provider is `auto` (Groq key → Ollama → heuristics only).

See [LLM_SETUP.md](LLM_SETUP.md).

## Build a wheel locally (maintainers)

```bash
pip install maturin
maturin build --release -b bin --out dist
pip install dist/*.whl   # or dist\*.whl on Windows
```

## Publish (maintainers)

1. Bump `version` in [Cargo.toml](Cargo.toml) and add entry to [CHANGELOG.md](../CHANGELOG.md)
2. Tag: `git tag vX.Y.Z && git push origin vX.Y.Z`
3. GitHub Actions workflow [.github/workflows/pypi.yml](../.github/workflows/pypi.yml) runs tests, builds wheels, smoke-tests all platforms, uploads to PyPI
4. Create a GitHub Release with the changelog excerpt for that version

Configure the `pypi` GitHub environment and [PyPI trusted publisher](https://docs.pypi.org/trusted-publishers/) for the repository.

## Optional Python launcher

The `python/trace_diff/` package is an optional launcher with privilege hints. Maturin cannot combine it with `bindings = "bin"` in the same wheel today; the shipped PyPI package installs the Rust binary directly.
