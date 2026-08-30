# PyPI (pip) distribution

Install the prebuilt CLI without Rust:

```bash
pip install trace-test
trace-diff --help
trace-diff run https://example.com --skip-trace --headless
```

Wheels ship the native `trace-diff` binary onto your `PATH` (inside the active Python environment). The PyPI distribution name is **`trace-test`**; the CLI command remains **`trace-diff`**.

## Platform wheels

CI publishes wheels for:

| OS | Architecture |
|----|----------------|
| Windows | x86_64 |
| macOS | Apple Silicon (arm64) |
| macOS | Intel (x86_64) |
| Linux | x86_64 (manylinux 2_28) |

`pip` selects the matching wheel automatically.

## Privileges (same as cargo install)

| Platform | L3/L4 traceroute | L7 HTTP (`--skip-trace`) |
|----------|------------------|---------------------------|
| Windows | Administrator | works |
| macOS | often `sudo` | works |
| Linux | `setcap` or `sudo` | works |

## Build a wheel locally

```bash
pip install maturin
maturin build --release -b bin --out dist
pip install dist/*.whl   # or dist\*.whl on Windows
```

Cross-target example:

```bash
rustup target add x86_64-pc-windows-msvc
maturin build --release -b bin --target x86_64-pc-windows-msvc --out dist
```

## Publish (maintainers)

1. Bump `version` in `Cargo.toml`
2. Tag: `git tag v0.1.0 && git push origin v0.1.0`
3. GitHub Actions workflow `.github/workflows/pypi.yml` builds wheels and uploads to PyPI

Configure the `pypi` GitHub environment and [PyPI trusted publisher](https://docs.pypi.org/trusted-publishers/) for the repository.

## Optional Python launcher

The `python/trace_diff/` package is an optional launcher with privilege hints. Maturin cannot combine it with `bindings = "bin"` in the same wheel today; the shipped PyPI package installs the Rust binary directly.
