# How to read a diff (cookbook)

## What a diff answers

After you save a baseline and compare a new run, `trace-diff` answers:

> **Which stage got slower, and did the network path change?**

---

## L7 stage deltas

| Metric | Meaning | Typical action if worse |
|---|---|---|
| `dns` | Hostname → IP time | Check resolver, CDN DNS, split-horizon DNS |
| `tcp_handshake` | TCP connect time | Congestion, distant PoP, firewall path |
| `tls` | TLS negotiation | Cipher/CPU, middleboxes, cert chain issues |
| `ttfb` | Time to first response byte | **App/server** processing, origin latency |
| `transfer` | Body download | Payload size, bandwidth, throttling |
| `total` | End-to-end sum | Start with the largest stage delta |

### Severity bands (defaults)

- **Warn** — ≥ 20% worse than baseline  
- **Critical** — ≥ 50% worse than baseline  

Absolute CI gates (separate from %), e.g. `--fail-if-ttfb-exceeds 250ms`, fail the process even without a baseline.

---

## Topology / hop diffs

| Signal | Meaning |
|---|---|
| Address changed on hop N | Different router/PoP than baseline |
| Added / dropped IPs | Path length or ECMP membership changed |
| Reordered | Same routers, different order |
| Hop RTT delta | That segment is slower |

If **hops changed** but **TTFB is flat**, suspect network/CDN routing.  
If **hops are stable** but **TTFB jumped**, suspect the application or origin.

---

## Worked examples

### 1. Backend regression

```text
[Critical] ttfb: ttfb increased by 65.0% vs baseline
[Warn] total: total increased by 40.0% vs baseline
```

Hops unchanged → investigate deploy, DB, or upstream dependency.

### 2. DNS / edge issue

```text
[Warn] dns: dns increased by 120.0% vs baseline
```

TCP/TLS/TTFB near baseline → resolver or anycast DNS shift.

### 3. Path change

```text
[Warn] topology: route topology changed (added=1, dropped=1, reordered=false)
[Warn] hop_5_rtt: hop_5_rtt increased by 35.0% vs baseline
```

Ask networking/CDN whether a failover or traffic-engineering change occurred.

---

## TUI reading order

1. **Header bars** — where time is spent now  
2. **Hop table** — path health / loss / sparklines  
3. **Diff drawer** — what changed vs baseline (`b` to pick another)  
4. Press `e` to export JSON, `?` for keys, `t` to cycle themes  

---

## CI pattern

```bash
trace-diff run https://api.example.com --skip-trace --save-baseline staging --headless
# later
trace-diff diff staging https://api.example.com --skip-trace --headless
trace-diff run https://api.example.com --skip-trace --headless --fail-if-ttfb-exceeds 250ms
```

Critical regressions from `diff` exit non-zero so pipelines can fail closed.
