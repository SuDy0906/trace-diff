# Install guides (Windows / macOS / Linux)

## pip (recommended)

No Rust toolchain required. Install into your active Python environment (system Python, venv, or conda):

```bash
pip install trace-route-test
trace-diff --help
```

PyPI package name: **`trace-route-test`**. CLI command: **`trace-diff`**.

Upgrade or reinstall:

```bash
pip install --upgrade trace-route-test
```

### Quick smoke test (no admin)

Works on all platforms — HTTP only, skips L3/L4 traceroute:

```bash
trace-diff run https://example.com --skip-trace --headless
```

### Features TUI (OpenAPI workflows)

```bash
trace-diff features https://api.example.com
```

Set auth via env vars or `--auth-file` (see [FEATURES_AUTODETECT.md](FEATURES_AUTODETECT.md)).

### Optional: smarter workflows (LLM)

Heuristic workflows work without any setup. To enable optional LLM refine (Groq recommended — API key only):

```bash
export GROQ_API_KEY="gsk_..."   # free at https://console.groq.com
trace-diff features --check-llm
trace-diff features https://api.example.com
```

Full guide: [LLM_SETUP.md](LLM_SETUP.md).

---

## Platform notes (pip install)

After `pip install`, `trace-diff` is on your `PATH` inside that Python environment.

| Platform | Where the binary lives | L7 HTTP (`--skip-trace`) | L3/L4 traceroute |
|----------|------------------------|---------------------------|------------------|
| **Windows** | `<venv>\Scripts\trace-diff.exe` or Python `Scripts\` | Works without Admin | Run terminal **as Administrator** |
| **macOS** | `<venv>/bin/trace-diff` | Works | Often needs `sudo trace-diff ...` |
| **Linux** | `<venv>/bin/trace-diff` | Works | `sudo` or `setcap cap_net_raw+ep $(which trace-diff)` |

### Windows

```powershell
python -m venv .venv
.\.venv\Scripts\Activate.ps1
pip install trace-route-test

# L7 only
trace-diff run https://example.com --skip-trace --headless

# Full traceroute — open PowerShell as Administrator first
trace-diff run https://example.com
```

### macOS

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install trace-route-test

trace-diff run https://example.com --skip-trace --headless

# ICMP may require sudo
sudo trace-diff run 1.1.1.1 --skip-http --output text
```

Wheels are published for Apple Silicon (arm64) and Intel (x86_64); `pip` picks the match.

### Linux

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install trace-route-test

trace-diff run https://example.com --skip-trace --headless

# Optional: grant raw ICMP without sudo (path from `which trace-diff`)
sudo setcap cap_net_raw+ep "$(which trace-diff)"
trace-diff run 1.1.1.1 --skip-http --output text
```

Requires a **manylinux**-compatible x86_64 glibc system (wheels target manylinux 2_28).

### Unsupported platforms (pip wheels)

| Platform | pip wheel | Alternative |
|----------|-----------|-------------|
| Linux aarch64 (ARM64) | not published | `cargo build --release` from source |
| Alpine / musl Linux | not published | build from source on glibc/musl target |
| Windows arm64 | not published | build from source |

See [TROUBLESHOOTING.md](TROUBLESHOOTING.md) for details.

---

## Verify

```bash
trace-diff --help
trace-diff run https://example.com --skip-trace --headless -v
```

`-v` / `--debug` writes probe traces to stderr. `NO_COLOR=1` or `--no-color` disables ANSI colors.

More pip details: [PYPI.md](PYPI.md).

---

## Build from source (developers)

Only needed if you are hacking on the Rust codebase or building wheels locally.

### Prerequisites

- Rust 1.75+ (`rustup`)
- Network access to targets under test

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Windows: winget install Rustlang.Rustup
```

### Clone and build

```bash
git clone https://github.com/SuDy0906/trace-diff.git
cd trace-diff
cargo build --release
```

Binary paths:

- Windows: `.\target\release\trace-diff.exe`
- macOS / Linux: `./target/release/trace-diff`

Or build a local wheel:

```bash
pip install maturin
maturin build --release -b bin --out dist
pip install dist/*.whl   # Windows: dist\*.whl
```
