# Install guides (Windows / macOS / Linux)

## pip (all platforms)

```bash
pip install trace-route-test
trace-diff --help
trace-diff run https://example.com --skip-trace --headless
```

Details: [PYPI.md](PYPI.md)

---

## Prerequisites (build from source)

- Rust 1.75+ (`rustup`) for building from source  
- Network access to the target under test  
- **Optional elevated privileges** for L3/L4 traceroute (ICMP). L7 HTTP probing works without elevation.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# Windows: winget install Rustlang.Rustup
```

Ensure Cargo is on `PATH`:

```powershell
# Windows PowerShell (current session)
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
```

```bash
# macOS / Linux
source "$HOME/.cargo/env"
```

---

## Build from source

```bash
git clone <your-repo-url> trace-diff
cd trace-diff
cargo build --release
```

Binary path:

- Windows: `.\target\release\trace-diff.exe`
- macOS / Linux: `./target/release/trace-diff`

---

## Windows

1. Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) (C++ workload) if `rustc` asks for a linker.
2. Build as above.
3. **L7 only (no Admin):**
   ```powershell
   .\target\release\trace-diff.exe run https://example.com --skip-trace --output text
   ```
4. **Full traceroute:** open PowerShell **as Administrator**, then omit `--skip-trace`.
5. Optional install location: copy the exe to a folder on `PATH`, or publish via Scoop/winget later.

---

## macOS

```bash
cargo build --release
./target/release/trace-diff run https://example.com --skip-trace --output text
```

ICMP traceroute may require `sudo`:

```bash
sudo ./target/release/trace-diff run 1.1.1.1 --skip-http --output text
```

Apple Silicon and Intel both work via `rustup` targets `aarch64-apple-darwin` / `x86_64-apple-darwin`.

---

## Linux

```bash
cargo build --release
./target/release/trace-diff run https://example.com --skip-trace --output text
```

Prefer capability instead of full root for traceroute:

```bash
sudo setcap cap_net_raw+ep ./target/release/trace-diff
./target/release/trace-diff run 1.1.1.1 --skip-http --output text
```

---

## Verify

```bash
trace-diff --help
trace-diff run https://example.com --skip-trace --headless -v
```

`-v` / `--debug` write probe traces to stderr. `NO_COLOR=1` or `--no-color` disables ANSI colors.
