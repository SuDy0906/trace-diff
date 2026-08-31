# PyPI (pip) distribution

Install the prebuilt CLI — **no Rust or cargo required**:

```bash
pip install trace-route-test
```

| | |
|---|---|
| **PyPI package** | `trace-route-test` |
| **CLI command** | `trace-diff` |
| **Current version** | see [pypi.org/project/trace-route-test](https://pypi.org/project/trace-route-test/) |

Wheels ship the native `trace-diff` binary onto your `PATH` inside the active Python environment (venv, conda, or user site-packages).

---

## Everyday use (start here)

Run commands **without** `--headless`. You get the **interactive TUI** — the main reason to install this tool.

### Slow site or API?

```bash
trace-diff run https://example.com
```

Live dashboard: DNS → TCP → TLS → TTFB → download, plus hop-by-hop path.

HTTP timing only (no Administrator on Windows):

```bash
trace-diff run https://example.com --skip-trace
```

### Test API workflows?

Use your **API host** (where OpenAPI is published):

```bash
trace-diff features https://api.example.com
```

Select scenarios with Space, run with Enter. Press `?` for help, `c` for auth, `l` for LLM status.

If discovery only shows `/health` and sitemap pages, you likely pointed at the marketing site — try `https://api.yoursite.com` instead.

---

## For developers & CI

Use `--headless`, `-y`, or `--output json` when you need machine output — not for first-time exploration.

### Network probe (scripts / pipelines)

```bash
trace-diff run https://api.example.com --skip-trace --headless
trace-diff run https://api.example.com --headless --save-baseline staging
trace-diff diff staging https://api.example.com --output json
trace-diff run https://api.example.com --headless --fail-if-ttfb-exceeds 250ms
trace-diff list
```

### API features (CI smoke)

```bash
trace-diff features https://api.example.com -y --json
trace-diff features --check-llm --json
trace-diff features https://api.example.com -y --no-llm --fail-on-reachable
```

Non-TTY stdout auto-runs headless (same as `-y`).

Docs: [CI.md](CI.md) · [FEATURES_AUTODETECT.md](FEATURES_AUTODETECT.md)

---

## What you get

One binary, two workflows:

| Mode | Command | Purpose |
|------|---------|---------|
| **Network probe** | `trace-diff run` | Path + HTTP timing, baselines, diffs |
| **API features** | `trace-diff features` | OpenAPI workflows, auth, scorecard |

---

## Install options

```bash
pip install trace-route-test
pip install --upgrade trace-route-test
pip install --user trace-route-test
pip install trace-route-test==0.1.2
```

Virtual environment (recommended):

```bash
python -m venv .venv
# Windows: .venv\Scripts\activate
# macOS/Linux: source .venv/bin/activate
pip install trace-route-test
```

## Platform wheels

| OS | Architecture |
|----|----------------|
| Windows | x86_64 |
| macOS | Apple Silicon (arm64), Intel (x86_64) |
| Linux | x86_64 (manylinux 2_28) |

`pip` picks the matching wheel automatically. No source fallback — binaries only.

## Privileges

| Platform | Traceroute | HTTP (`--skip-trace`) | `features` |
|----------|------------|------------------------|------------|
| Windows | Administrator | works | works |
| macOS | often `sudo` | works | works |
| Linux | `setcap` or `sudo` | works | works |

See [INSTALL.md](INSTALL.md).

## Optional LLM

Heuristics need no API key. Optional refine with Groq or Ollama: [LLM_SETUP.md](LLM_SETUP.md).

```bash
export GROQ_API_KEY="gsk_..."
trace-diff features --check-llm
trace-diff features https://api.example.com
```

## Build / publish (maintainers)

```bash
pip install maturin
maturin build --release -b bin --out dist
```

1. Bump `version` in [Cargo.toml](../Cargo.toml) and [CHANGELOG.md](../CHANGELOG.md)
2. Tag: `git tag vX.Y.Z && git push origin vX.Y.Z`
3. [.github/workflows/pypi.yml](../.github/workflows/pypi.yml) publishes to PyPI

Configure the `pypi` GitHub environment and [PyPI trusted publisher](https://docs.pypi.org/trusted-publishers/).
