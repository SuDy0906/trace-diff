# PyPI (pip) distribution

Install the prebuilt CLI — **no Rust or cargo required**:

```bash
pip install trace-route-test
trace-diff --help
trace-diff run https://example.com --skip-trace --headless
```

| | |
|---|---|
| **PyPI package** | `trace-route-test` |
| **CLI command** | `trace-diff` |
| **Current version** | see [pypi.org/project/trace-route-test](https://pypi.org/project/trace-route-test/) |

Wheels ship the native `trace-diff` binary onto your `PATH` inside the active Python environment (venv, conda, or user site-packages).

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

| Platform | L3/L4 traceroute | L7 HTTP (`--skip-trace`) |
|----------|------------------|---------------------------|
| Windows | Administrator | works |
| macOS | often `sudo` | works |
| Linux | `setcap` or `sudo` | works |

See [INSTALL.md](INSTALL.md) for per-platform examples using the pip-installed `trace-diff` command.

## Common commands (after pip install)

```bash
trace-diff run https://example.com --skip-trace --headless
trace-diff features https://api.example.com
trace-diff diff staging https://example.com --output json
trace-diff list
```

## Build a wheel locally (maintainers)

```bash
pip install maturin
maturin build --release -b bin --out dist
pip install dist/*.whl   # or dist\*.whl on Windows
```

## Publish (maintainers)

1. Bump `version` in `Cargo.toml`
2. Tag: `git tag v0.1.0 && git push origin v0.1.0`
3. GitHub Actions workflow `.github/workflows/pypi.yml` builds wheels and uploads to PyPI

Configure the `pypi` GitHub environment and [PyPI trusted publisher](https://docs.pypi.org/trusted-publishers/) for the repository.

## Optional Python launcher

The `python/trace_diff/` package is an optional launcher with privilege hints. Maturin cannot combine it with `bindings = "bin"` in the same wheel today; the shipped PyPI package installs the Rust binary directly.
