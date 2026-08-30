"""Console entrypoint: exec the platform-native trace-diff binary shipped in the wheel."""

from __future__ import annotations

import ctypes
import os
import subprocess
import sys
from importlib.resources import as_file, files
from pathlib import Path
import shutil


def _exe_name() -> str:
    return "trace-diff.exe" if os.name == "nt" else "trace-diff"


def _bundled_exe():
    return files("trace_diff").joinpath("_bin", _exe_name())


def _is_admin() -> bool:
    if os.name != "nt":
        return True
    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except Exception:
        return False


def _needs_trace_hint(args: list[str]) -> bool:
    if not args or args[0] not in ("run", "features"):
        return False
    return not any(a == "--skip-trace" or a.startswith("--skip-trace=") for a in args)


def _privilege_hint() -> str:
    if sys.platform == "darwin":
        return (
            "Note: L3/L4 traceroute may require sudo on macOS.\n"
            "  L7-only: add --skip-trace"
        )
    if sys.platform.startswith("linux"):
        return (
            "Note: L3/L4 traceroute may require cap_net_raw or sudo on Linux.\n"
            "  L7-only: add --skip-trace\n"
            "  Or: sudo setcap cap_net_raw+epi $(which trace-diff)"
        )
    if os.name == "nt":
        return (
            "Note: L3/L4 traceroute needs Administrator on Windows.\n"
            "  L7-only: add --skip-trace"
        )
    return ""


def _resolve_exe() -> Path:
    try:
        ref = _bundled_exe()
        with as_file(ref) as exe:
            if exe.is_file():
                return exe
    except Exception:
        pass
    found = shutil.which("trace-diff")
    if found:
        return Path(found)
    raise FileNotFoundError("trace-diff binary not found in package or PATH")


def main() -> None:
    try:
        exe = _resolve_exe()
    except FileNotFoundError as exc:
        print(f"trace-diff: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc

    args = sys.argv[1:]
    if _needs_trace_hint(args):
        if os.name == "nt":
            if not _is_admin():
                print(_privilege_hint(), file=sys.stderr)
        else:
            print(_privilege_hint(), file=sys.stderr)

    code = subprocess.call([str(exe), *args])
    raise SystemExit(code)


if __name__ == "__main__":
    main()
