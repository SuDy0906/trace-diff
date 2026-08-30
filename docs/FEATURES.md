# trace-diff — Feature Reference

Complete catalog of what **trace-diff** can do today: dual-layer probing (L3/L4 path + L7 HTTP lifecycle), baselines, diffs, interactive TUI, CI/headless modes, and enrichment.

Related docs: [INSTALL](INSTALL.md) · [How to read a diff](HOW_TO_READ_A_DIFF.md) · [Security](SECURITY.md) · [Timing](TIMING.md) · [Problem & proposal](PROBLEM_AND_TECHNICAL_PROPOSAL.md)

---

## 1. What it is

**trace-diff** is a terminal-native diagnostic CLI that answers:

1. **Where did time go in the HTTP request?** (DNS → TCP → TLS → TTFB → body)
2. **What path did packets take to the server?** (hop-by-hop L3/L4 traceroute)
3. **Did anything regress vs a known-good baseline?** (SQLite snapshots + diffs)

It is designed for developers and operators debugging “slow API” / “route weirdness” with both a beginner-friendly TUI and machine-readable JSON for CI.

---

## 2. Commands

| Command | Purpose |
|--------|---------|
| `run <target>` | Probe a URL/host (L3/L4 + L7), optional save/compare baseline |
| `features <url>` | Auto-detect pages/APIs, interactive select, graphical scorecard |
| `baseline tag \| delete \| show` | Manage named baselines |
| `diff <baseline> [target]` | Compare latest (or fresh) run against a baseline |
| `list` | List stored runs and baselines |

### 2.1 `run`

```bash
trace-diff run https://api.example.com
trace-diff run https://api.example.com --save-baseline staging
trace-diff run https://api.example.com --compare-baseline staging
trace-diff run https://api.example.com --skip-trace --output text
trace-diff run 1.1.1.1 --skip-http --probe icmp
```

| Flag | Default | Description |
|------|---------|-------------|
| `--save-baseline NAME` | — | Tag this run as a named baseline |
| `--compare-baseline NAME` | — | Diff against an existing baseline after the run |
| `--max-ttl N` | `30` | Maximum hop TTL |
| `--probes N` | `3` | Probes per hop |
| `--timeout DUR` | `2s` | Per-probe / phase timeout (`250ms`, `2s`, …) |
| `--skip-trace` | off | Skip L3/L4 path probing |
| `--skip-http` | off | Skip L7 HTTP lifecycle |
| `--output tui\|json\|text` | `tui` | Result presentation |
| `--headless` | off | Alias for `--output json` |
| `--fail-if-ttfb-exceeds DUR` | — | Non-zero exit if TTFB over limit (CI gate) |
| `--fail-if-handshake-exceeds DUR` | — | Gate on TCP connect time |
| `--fail-if-dns-exceeds DUR` | — | Gate on DNS time |
| `--db PATH` | platform data dir | SQLite database path (`TRACE_DIFF_DB`) |
| `--probe auto\|icmp\|udp\|tcp` | `auto` | L3/L4 probe strategy |
| `--probe-port N` | from URL / `443` | Destination port for TCP path probes |
| `--no-enrich` | off | Skip reverse-DNS + ASN lookups on hops |

### 2.2 `baseline`

```bash
trace-diff baseline tag <run-id> <name>
trace-diff baseline show staging
trace-diff baseline delete staging
```

### 2.3 `diff`

```bash
trace-diff diff staging                         # latest stored run vs baseline
trace-diff diff staging https://api.example.com # re-probe then diff
trace-diff diff staging --headless
```

Supports `--skip-trace`, `--skip-http`, `--output`, `--db`.

### 2.4 `list`

```bash
trace-diff list
trace-diff list --baselines-only
```

---

## 3. Global flags & environment

| Flag / env | Description |
|------------|-------------|
| `-v` / `-vv` | Verbose / trace logs on stderr |
| `--debug` | Maximum verbosity |
| `--no-color` / `TRACE_DIFF_NO_COLOR` / `NO_COLOR` | Disable ANSI colors |
| `--force-color` | Force colors even when `NO_COLOR` is set |
| `--theme default\|ocean\|amber\|mono` / `TRACE_DIFF_THEME` | Color palette |
| `TRACE_DIFF_DB` | Default SQLite path override |

---

## 4. Layer 7 — HTTP request journey

Measures a **GET** to the target URL with monotonic clocks (`std::time::Instant`).

| Stage (UI name) | Technical | Meaning |
|-----------------|-----------|---------|
| 1 Find (DNS) | DNS | Hostname → IP |
| 2 Connect (TCP) | TCP | TCP handshake |
| 3 Secure (TLS) | TLS | TLS handshake (HTTPS) |
| 4 Wait (TTFB) | TTFB | Time to first response byte (often server/API time) |
| 5 Download (Body) | Body | Read response body (capped) |
| — | Total | End-to-end lifecycle |

Also records:

- HTTP status code  
- Resolved IP  
- Bytes read  
- Measurement timestamp + run metadata  

Presented in the TUI as heat-colored bars and a pipeline strip; in text/JSON as explicit stage timings.

---

## 5. Layer 3/4 — Path to server

Hop-by-hop traceroute toward the resolved destination, with path summary and hop enrichment.

### 5.1 Probe strategies (`--probe`)

| Mode | Behavior |
|------|----------|
| **auto** (default) | ICMP Echo first; for silent hops try UDP then TCP (when raw ICMP receive works) |
| **icmp** | ICMP Echo only |
| **udp** | UDP probes + ICMP Time Exceeded; ICMP fallback |
| **tcp** | TCP SYN-style probes + ICMP Time Exceeded; ICMP fallback |

Paris-style affinity uses a fixed ICMP identifier / source port where applicable to reduce ECMP false jitter.

### 5.2 Windows path discovery

On Windows, ICMP Echo uses the **IP Helper API** (`IcmpSendEcho` + per-probe TTL) — the same family of APIs as `tracert` — so intermediate **Time Exceeded** hops are visible. Raw sockets alone often miss those replies on Windows.

Elevated (Administrator) privileges improve traceroute reliability. L7 still works without elevation.

### 5.3 Per-hop data

For each TTL:

| Field | Description |
|-------|-------------|
| Address | Router / destination IP (or `*` if silent) |
| Via | Protocol that identified the hop (`ICMP` / `UDP` / `TCP`) |
| RTT | Latency samples (min / avg / P50 / P95 / jitter / loss) |
| Hostname | Reverse DNS (PTR), when enrichment enabled |
| ASN / AS name | Team Cymru DNS lookup (IPv4), when enrichment enabled |
| Signal | ◆ reply · ○ silent · ● destination |

### 5.4 Path summary

| Metric | Meaning |
|--------|---------|
| Hop count | TTLs probed until destination (or max) |
| Live / silent | How many hops replied vs timed out |
| Gaps | Contiguous silent TTL ranges |
| Protocols used | Which probe types got replies |
| TCP≥TTLn | Approximate minimum TTL that can TCP-reach dest:port |
| Dest ASN | Autonomous system of the destination hop |
| raw_icmp_ok | Whether ICMP receive / Helper path is available |

Silent hops remain common (routers filter ICMP). The tool surfaces gaps explicitly rather than inventing hop IPs.

### 5.5 Enrichment

Unless `--no-enrich`:

- Reverse DNS (PTR)  
- ASN + organization via Team Cymru (`origin.asn.cymru.com` / `AS####.asn.cymru.com`)

---

## 6. Interactive TUI

Default for interactive terminals (`--output tui`).

### 6.1 Layout (results)

1. **Verdict badge** — HEALTHY / SLOW / ROUTE PROBLEM / REGRESSION, with short reason  
2. **Request journey** — Find(DNS) → Connect(TCP) → Secure(TLS) → Wait(TTFB) → Download(Body) with bars  
3. **Path map** — Summary chips + visual ribbon (`you ○─◆─● dest`)  
4. **Hops table** — TTL, node, via, RTT, AS/name  
5. **Compare** — Baseline status / regressions  
6. **Status bar** — Key hints  

### 6.2 Live progress

While probing: spinner, checklist of stages (DNS/TCP/TLS/TTFB/body/hops), progress log.

### 6.3 Keyboard

| Key | Action |
|-----|--------|
| `?` | Help overlay |
| `g` | Beginner guide |
| `b` | Baseline picker (diff against saved baseline) |
| `e` | Export JSON report to a file |
| `t` | Cycle theme (default → ocean → amber → mono) |
| `m` | Toggle advanced metadata (OS, privileges, probe mode) |
| `q` / Esc | Quit (after probe finishes) |
| ↑/↓ or `j`/`k` | Navigate baseline picker |
| Enter | Confirm baseline selection |

### 6.4 Themes

`default`, `ocean`, `amber`, `mono` — via `--theme`, `TRACE_DIFF_THEME`, or `t` in the TUI.

---

## 7. Output formats

| Format | Use |
|--------|-----|
| **TUI** | Interactive dashboard (default in a terminal) |
| **text** | Human-readable report on stdout |
| **json** / `--headless` | Structured run (+ optional diff) for CI / piping |

JSON includes run id, target, resolved IP, hop metrics, L7 timings, metadata, and optional diff report.

---

## 8. Baselines & storage

Local **SQLite** database (bundled):

| Data | Contents |
|------|----------|
| Targets | Endpoint strings |
| Runs | Timestamps, resolved IP, reached flag, hop JSON, L7 JSON, meta JSON |
| Hop metrics | Per-hop RTT / loss stats |
| L7 metrics | DNS/TCP/TLS/TTFB/transfer/total, status, bytes |
| Baselines | Name → run id |
| Metadata | Tool version, OS/arch, privileges, probe mode, timing disclaimer |

Default DB location is the platform data directory (override with `--db` / `TRACE_DIFF_DB`).

---

## 9. Diff & regression engine

Compares a **current** run to a **baseline** run:

### 9.1 L7 deltas

Percent change for DNS, TCP, TLS, TTFB, transfer, total.

### 9.2 Hop / topology

- Per-TTL address + RTT delta  
- Added / dropped hop IPs  
- Reordered shared path detection  

### 9.3 Severity

Default thresholds:

| Level | Δ vs baseline |
|-------|----------------|
| Warn | ≥ ~20% |
| Critical | ≥ ~50% |

Regressions appear in the TUI Compare panel and in JSON. `diff` exits non-zero on critical regressions.

### 9.4 CI hard gates (`run`)

Independent of baseline diffs:

- `--fail-if-ttfb-exceeds`  
- `--fail-if-handshake-exceeds`  
- `--fail-if-dns-exceeds`  

---

## 10. Observability & metadata

Each run can record:

- Tool version  
- OS / arch  
- Privilege level (`elevated` / `unprivileged` / `unknown`)  
- Probe mode (`full` / `l7_only` / `trace_only` / `none`)  
- Timing basis + disclaimer (monotonic Instant vs wall clock)  

Verbose logs: `-v`, `-vv`, or `--debug` (stderr; does not pollute TUI stdout).

---

## 11. Statistics

Per hop and phase where applicable:

- min, average, P50, P95  
- RFC 3550–style jitter estimate  
- Loss %  

---

## 12. Testing & CI

| Area | Coverage |
|------|----------|
| Unit tests | Stats, DNS helpers, path summary, store round-trip, TUI backend |
| Integration | CLI help, headless run + baseline, wiremock L7, DNS faults, JSON snapshots |
| GitHub Actions | Build/test on Ubuntu, Windows, macOS; headless L7 smoke; fmt + clippy |
| Optional script | `scripts/netns_netem_testbed.sh` (Linux netem path lab) |

---

## 13. Security & privileges (summary)

| Capability | Needs |
|------------|--------|
| L7 HTTP/HTTPS | None (normal user) |
| L3/L4 traceroute | Raw ICMP / Admin (Windows elevated; Linux `cap_net_raw` or root; macOS often `sudo`) |

- No third-party cloud credentials requested  
- Response bodies not persisted as content (read is capped)  
- TLS via `rustls` + webpki roots  

See [SECURITY.md](SECURITY.md) for the full model.

---

## 14. Platform notes

| Platform | Notes |
|----------|--------|
| **Windows** | ICMP Helper for hop discovery; run as Administrator for best path detail |
| **Linux** | Prefer `setcap cap_net_raw+ep` over full root |
| **macOS** | May need `sudo` for ICMP |

L7-only workflows (`--skip-trace`) work everywhere without elevation.

---

## 15. Example workflows

### Fast HTTP-only check

```bash
trace-diff run https://api.example.com --skip-trace
```

### Save a staging baseline, then catch regressions

```bash
trace-diff run https://api.example.com --save-baseline staging
# … later …
trace-diff run https://api.example.com --compare-baseline staging
# or
trace-diff diff staging https://api.example.com --output json
```

### CI gate on TTFB

```bash
trace-diff run https://api.example.com --headless --skip-trace --fail-if-ttfb-exceeds 300ms
```

### Path-focused probe with enrichment

```bash
# Windows: elevated PowerShell recommended
trace-diff run https://confuciusai.io --probe auto --output text
```

### Text report without saving a baseline

```bash
trace-diff run https://example.com --output text
```

---

## 16. Feature checklist (at a glance)

- [x] L7 phase timing: DNS, TCP, TLS, TTFB, body, total, HTTP status  
- [x] L3/L4 traceroute: ICMP / UDP / TCP / auto  
- [x] Windows ICMP Helper for intermediate hops  
- [x] Path map + hop table visualization  
- [x] Reverse DNS + ASN enrichment  
- [x] Path summary (live/silent, gaps, protocols, TCP reach TTL)  
- [x] Interactive TUI with live progress  
- [x] Themes, help, guide, export, baseline picker  
- [x] SQLite baselines and run history  
- [x] Diff engine (L7 %, hops, topology, severity)  
- [x] CI JSON / headless + fail-if-* gates  
- [x] Verbose logging and run metadata  
- [x] Cross-platform CI tests  

### Not in scope (today)

- Custom HTTP methods / headers / bodies (GET-only L7)  
- Full packet capture / Wireshark-class analysis  
- Multipath ECMP enumeration UI (Paris IDs used internally; multi-flow UI not exposed)  
- Guaranteed intermediate hop IPs when routers filter all ICMP Time Exceeded  

---

*Generated for trace-diff v0.1.x — keep this file updated when adding commands or probe modes.*
