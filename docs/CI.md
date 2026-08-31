# CI integration

Use `trace-diff` in pipelines for network regression checks and API workflow smoke tests.

Install:

```yaml
- run: pip install trace-route-test
```

## Network probe (baseline + diff)

Save a baseline on main, diff on PRs:

```yaml
- name: Probe and diff
  run: |
    trace-diff run https://api.example.com --skip-trace --headless \
      --save-baseline mainline --db ./trace.db
    trace-diff diff mainline https://api.example.com --skip-trace --headless \
      --output json --db ./trace.db
```

Fail on slow TTFB:

```yaml
- run: |
    trace-diff run https://api.example.com --skip-trace --headless \
      --fail-if-ttfb-exceeds 250ms
```

Exit code is **non-zero** on threshold failure or probe errors.

## Features (OpenAPI workflows)

Headless discovery + run with JSON report:

```yaml
- name: API feature smoke
  env:
    TRACE_DIFF_EMAIL: ${{ secrets.API_EMAIL }}
    TRACE_DIFF_PASSWORD: ${{ secrets.API_PASSWORD }}
  run: |
    trace-diff features https://api.example.com -y --json --no-llm \
      --fail-on-reachable \
      --fail-if-ttfb-exceeds 500ms
```

Skip LLM in CI (`--no-llm`) — heuristics are deterministic and need no API keys.

Verify LLM provider setup (optional job):

```yaml
- run: trace-diff features --check-llm
```

## GitHub Actions example

```yaml
name: api-smoke

on:
  pull_request:

jobs:
  trace-diff:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"
      - run: pip install trace-route-test
      - name: L7 probe
        run: trace-diff run https://api.example.com --skip-trace --headless
      - name: Feature workflows
        run: trace-diff features https://api.example.com -y --no-llm --no-tls-canary
```

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Probe failure, diff threshold exceeded, or features CI gate failed |

Features gates (with `-y`):

- Default: fail if any row is **Failed**
- `--fail-on-reachable`: also fail yellow auth/body rows
- `--fail-if-ttfb-exceeds DURATION`: fail if any probe TTFB exceeds limit

## JSON output

`--headless` / `--output json` on `run` and `diff` emit structured reports for parsing.

`features -y --json` prints a full feature run report to stdout.

JSON snapshot tests in the repo (`tests/json_snapshots.rs`) guard run/diff schema shape — treat field additions as minor version bumps.

## Privileges in CI

Use `--skip-trace` on hosted runners (no raw ICMP). L7 HTTP and `features` work without elevated privileges.

## See also

- [FEATURES_AUTODETECT.md](FEATURES_AUTODETECT.md) — workflow discovery and auth
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) — common failures
- [TIMING.md](TIMING.md) — clock and variance notes
