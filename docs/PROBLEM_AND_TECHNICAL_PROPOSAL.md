# trace-diff — Problem Statement & Technical Proposal

`trace-diff` is an interactive, terminal-native diagnostic CLI built in Rust. It combines Layer 3/4 hop-by-hop route tracing with Layer 7 HTTP connection lifecycle breakdown, storing execution baselines in an embedded SQLite database to detect and visualize network and API performance regressions.

---

## 1. Problem Statement

### 1.1 The pain

When an API or website feels slow, teams usually guess:

- “The internet is slow”
- “The server is slow”
- “DNS is broken”
- “Something changed in the network”

Those failures look similar from a browser or a single `curl` timing. Without structured measurement, on-call engineers waste time chasing the wrong layer.

### 1.2 What is missing today

| Existing tool | Gap |
|---|---|
| `traceroute` / `mtr` | Network path only — no HTTP lifecycle, no baselines |
| `curl -w` / browser DevTools | Application timings only — no hop path, weak historical comparison |
| APM / synthetic monitors | Powerful but heavy, cloud-dependent, not a local terminal workflow |
| Ad-hoc scripts | No shared schema, no TUI, no CI-friendly regression gates |

### 1.3 The product question

Teams need one local tool that can answer:

1. Is the delay **before** we reach the server (DNS / TCP / TLS / path)?
2. Is the delay **on** the server (TTFB / processing)?
3. Did the **route or latency profile change** versus a known-good run?
4. Can CI **fail automatically** when latency crosses a policy threshold?

### 1.4 Who it is for

- Backend / platform engineers validating post-deploy latency
- SRE / DevOps separating network path issues from app regressions
- On-call / support narrowing “it’s slow” to a specific stage
- Learners studying how request time actually splits across the stack

### 1.5 What it is not

- Not a packet sniffer (Wireshark)
- Not a load generator (k6 / Locust)
- Not a full browser performance suite (no rendering / JS)
- Not an auto-remediation system — it **measures and compares** so humans/automation can act

---

## 2. Use Case Theory (product view)

### 2.1 Two stories of “slow”

#### Layer 3/4 — the road trip (network path)

A packet does not go directly to the destination. It hops through routers:

```text
You → ISP router → intermediate routers → destination
```

**Traceroute** asks: who are the stops, and how long to each stop?

Useful signals:

- New slow hop appeared
- Router disappeared or changed IP
- Packet loss on the path
- Round-trip time (RTT) worse than before

#### Layer 7 — the conversation (HTTP/HTTPS)

Even with a good path, an app still must:

1. **DNS** — resolve hostname → IP  
2. **TCP handshake** — open the connection  
3. **TLS handshake** — secure HTTPS  
4. **TTFB** — wait for first response byte (often server work)  
5. **Transfer** — download the body  

Any stage can dominate user-perceived latency.

### 2.2 Core product loop

```text
User complaint: "It's slow"
        │
        ▼
   trace-diff run
        │
        ├── Network path (hops / loss / RTT)
        └── HTTP lifecycle (DNS → TCP → TLS → TTFB → transfer)
        │
        ▼
 compare to named baseline
        │
        ▼
 "Which stage got worse?"  →  fix the right thing
```

### 2.3 Features (user-facing)

1. **Measure a target** — URL, hostname, or IP → one health snapshot  
2. **Save a baseline** — name a known-good run (`staging-baseline`, `prod-before-release`)  
3. **Diff against baseline** — percentage shifts + topology changes + severity highlights  
4. **Interactive TUI** — header stage bars, hop table, regression drawer  
5. **Headless / CI mode** — JSON output + non-zero exit when thresholds are exceeded  

### 2.4 Example story

1. Monday: staging is healthy → save as `staging-baseline`  
2. Wednesday: users report lag → re-probe and `diff staging-baseline`  
3. Outcomes:
   - TTFB up, hops unchanged → likely **backend** regression  
   - DNS up, rest normal → **resolver / CDN naming** issue  
   - Hop set changed, RTT worse → **routing / ISP / CDN path** change  
4. CI gate: fail if TTFB exceeds policy (e.g. 250ms)

### 2.5 Naming

- **trace** — measure the journey (hops + request stages)  
- **diff** — compare against a remembered healthy run  

---

## 3. Technical Proposal

### 3.1 Goals

| Goal | Description |
|---|---|
| Dual measurement | L3/L4 hop tracing + L7 HTTP lifecycle in one CLI |
| Baseline memory | Embedded SQLite store for runs and named baselines |
| Regression detection | % deltas, topology add/drop/reorder, warn/critical severities |
| Dual UX | Interactive `ratatui` TUI + headless JSON for CI |
| Policy gates | `--fail-if-ttfb-exceeds` and related absolute thresholds |

### 3.2 Non-goals (initial scope)

- Full packet capture / deep protocol decoding beyond ICMP + HTTP lifecycle  
- Distributed multi-region agent fleet (single-host CLI first)  
- Automatic remediation or traffic engineering  

---

## 4. Technology Stack

### Core system & runtime

| Concern | Choice |
|---|---|
| Language | Rust (edition 2021) |
| Async runtime | `tokio` (multi-threaded, full) |
| Sync / channels | `tokio::sync::mpsc`, `tokio::sync::watch` (as needed) |

### L3/L4 network probing

| Concern | Choice |
|---|---|
| Socket configuration | `socket2` (`IP_TTL`, `IPV6_UNICAST_HOPS`, non-blocking IO) |
| Packet encode/decode | `pnet_packet` (IPv4/IPv6, ICMP Echo / Time Exceeded, UDP) |
| Unprivileged ICMP fallback | `surge-ping` |

### L7 HTTP & DNS profiling

| Concern | Choice |
|---|---|
| HTTP client building blocks | `hyper` / `reqwest` (rustls) |
| DNS resolution timing | `hickory-resolver` |
| TLS engine | `rustls` / `tokio-rustls` |

### Storage & diffing

| Concern | Choice |
|---|---|
| Embedded DB | `rusqlite` (`bundled`) |
| Serialization | `serde`, `serde_json` |

### Terminal UI & CLI

| Concern | Choice |
|---|---|
| TUI | `ratatui` + `crossterm` |
| CLI parsing | `clap` v4 (derive) |
| Errors | `miette`, `thiserror` |

---

## 5. Architecture & Roadmap

### Phase 1 — Dual measurement engines

#### L3/L4 hop-by-hop tracer

- Paris Traceroute-style probing (fixed source port / ICMP identifier) to reduce false jitter under ECMP  
- Increment TTL (`1 → 30`), listen for ICMP Time Exceeded / Echo replies  
- Configurable probe bursts (`N = 3..5`) per hop  
- Metrics: min, median (P50), P95, jitter (RFC 3550), loss %  

#### L7 HTTP phase prober

Instrument connection lifecycle with monotonic `Instant` clocks:

| Interval | Meaning |
|---|---|
| T0 → T1 | DNS resolution |
| T1 → T2 | TCP handshake |
| T2 → T3 | TLS handshake |
| T3 → T4 | Time to First Byte (TTFB) |
| T4 → T5 | Content transfer |

### Phase 2 — Embedded baseline store & diff engine

#### Schema

- `targets` — unique endpoints  
- `runs` — each measurement execution  
- `hop_metrics` — per-hop stats for a run  
- `l7_metrics` — DNS/TCP/TLS/TTFB/transfer for a run  
- `baselines` — name → run_id mapping  

#### Baseline management

```bash
trace-diff run https://api.example.com --save-baseline staging-baseline
trace-diff baseline show staging-baseline
trace-diff list
```

#### Regression algorithm

- Percentage shifts: Δ TTFB, Δ handshake, per-hop Δ RTT  
- Topology changes: added, dropped, or reordered intermediate IPs  
- Severity bands (default): Warn ≥ 20%, Critical ≥ 50%  

### Phase 3 — Interactive TUI & CI modes

#### Interactive mode (`ratatui`)

- **Top header:** target, resolved IP, environment/baseline, stacked L7 stage bar  
- **Center grid:** hop index, host/IP, loss %, sent/recv, min/avg/P95, sparklines  
- **Bottom drawer:** live diff vs selected baseline (amber/red regressions)  

#### Headless / CI mode

```bash
trace-diff run https://api.example.com --headless --fail-if-ttfb-exceeds 250ms
trace-diff diff staging-baseline https://api.example.com --output json
```

- `--output json` / `--headless` emit structured JSON  
- Exit non-zero when absolute thresholds are exceeded or critical regressions are detected  

---

## 6. Testing Strategy

### Testing matrix

| Layer | Approach |
|---|---|
| Integration | Linux `netns` / Docker + `tc netem` multi-hop path emulation |
| L7 mocking | `wiremock` / embedded `axum`, delayed TLS/headers/body |
| Unit & property | `proptest` parsers, in-memory SQLite, percentile/diff math |
| CLI & TUI | `ratatui::TestBackend`, `assert_cmd`, `insta` JSON snapshots |
| Multi-OS CI | Linux (`cap_net_raw`), macOS, Windows Admin runner |

### Local network simulation (integration)

- Linux network namespaces + `veth` pairs for synthetic hops  
- `tc netem` for delay, jitter, and loss injection  
- Optional 3-tier `docker-compose` gateway path for TTL / ICMP validation  

### L7 phase mocking

- Deterministic delays before handshake, headers, and chunked body  
- Local DNS mock for NXDOMAIN, SERVFAIL, high-latency lookups  

### Statistical & storage unit tests

- Percentiles (P50 / P90 / P99), loss rate, RFC 3550 jitter  
- Diff algorithms against `rusqlite::Connection::open_in_memory()`  

### CI permission notes

| Platform | Permission strategy |
|---|---|
| Linux | `setcap cap_net_raw+ep` on the binary |
| macOS | Passwordless `sudo` for raw ICMP where required |
| Windows | Administrator context for ICMP traceroute |

> L7 HTTP probing works without elevated privileges on all platforms.

---

## 7. CLI Surface (proposed / implemented)

```text
trace-diff run <target> [options]
trace-diff baseline tag|delete|show ...
trace-diff diff <baseline> [target]
trace-diff list [--baselines-only]
```

Important `run` options:

- `--save-baseline <name>`
- `--skip-trace` / `--skip-http`
- `--output tui|json|text` / `--headless`
- `--fail-if-ttfb-exceeds <duration>`
- `--fail-if-handshake-exceeds <duration>`
- `--fail-if-dns-exceeds <duration>`
- `--db <path>` / `TRACE_DIFF_DB`

---

## 8. Success criteria

1. A developer can measure HTTPS lifecycle timings in one command without cloud setup  
2. A named baseline can be saved and later compared with human-readable + JSON output  
3. Regressions in TTFB / handshake / hop RTT / topology are explicitly surfaced  
4. CI can fail a build on absolute latency policy violations  
5. Interactive TUI makes stage contribution and hop health visible at a glance  

---

## 9. Quick demo script (validation)

```bash
# Capture baseline
cargo run -- run https://example.com --skip-trace --headless --save-baseline demo

# Inspect store
cargo run -- list

# Visual / text diff
cargo run -- diff demo https://example.com --skip-trace --output text

# CI-style gate
cargo run -- run https://example.com --skip-trace --headless --fail-if-ttfb-exceeds 250ms
```

---

## 10. Summary

**Problem:** “It’s slow” is ambiguous across DNS, transport, TLS, server time, and route changes.  

**Proposal:** A Rust CLI that **traces** both network path and HTTP lifecycle, **remembers** healthy baselines in SQLite, and **diffs** new runs for regressions — interactively in a TUI and automatically in CI.
