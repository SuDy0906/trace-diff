# Security model

## Privileges

| Capability | Required for | Platform notes |
|---|---|---|
| None (user) | L7 HTTP/HTTPS lifecycle probe | Default recommended path |
| Raw ICMP / Admin | L3/L4 traceroute | Windows: Administrator; Linux: `cap_net_raw` or root; macOS: often `sudo` |

`trace-diff` detects ICMP socket availability and records `privileges` in run metadata (`elevated` | `unprivileged` | `unknown`).

The tool **never** requests credentials to third-party cloud services. Optional HTTP features that send headers/cookies would be explicit user input (not enabled by default beyond a fixed User-Agent).

---

## Data stored locally

Default SQLite path (via the `directories` crate), overridable with `--db` / `TRACE_DIFF_DB`:

| Table / field | Contents |
|---|---|
| `targets` | Endpoint strings you probed |
| `runs` | Run ids, timestamps, resolved IPs, JSON blobs |
| `hop_metrics` | Per-hop RTT / loss stats |
| `l7_metrics` | DNS/TCP/TLS/TTFB/transfer timings, status, bytes |
| `baselines` | Name → run id mapping |
| `meta_json` | OS, arch, tool version, probe mode, privilege level, timing disclaimer |

**Not stored by default:** request bodies, Authorization headers, cookies, or full response bodies (body read is capped and not persisted as content).

---

## Network behavior

- Opens outbound TCP/TLS to the target you specify  
- May send ICMP Echo probes when traceroute is enabled  
- Performs DNS lookups via the system / Hickory resolver  

Do not point `trace-diff` at hosts you are not authorized to test.

---

## Supply chain & TLS

- TLS via `rustls` + webpki roots (no OpenSSL by default)  
- SQLite is bundled (`rusqlite` `bundled` feature)  
- Prefer building release binaries in CI and verifying checksums when distributing  

---

## Safe defaults for shared machines

```bash
# L7 only, local DB in the project folder, no color for logs
trace-diff run https://example.com --skip-trace --db ./trace-diff.db --headless --no-color
```

Delete the DB when finished if it contains internal hostnames:

```bash
rm ./trace-diff.db   # or del on Windows
```

---

## Optional LLM (Groq / Ollama)

Workflow discovery can optionally send **OpenAPI spec excerpts** to a third-party LLM:

| Provider | Data sent | Retention |
|---|---|---|
| **Groq** | OpenAPI JSON + heuristic workflow draft (no user passwords) | Subject to [Groq privacy policy](https://groq.com/privacy-policy/) — do not enable if your OpenAPI describes secrets |
| **Ollama (local)** | Same payloads to your local host only | Stays on your machine |

**Never logged:** `GROQ_API_KEY`, bearer tokens, passwords, or auth popup values. Keys are read from env/CLI only at runtime.

Disable cloud LLM entirely with `trace-diff features --no-llm` (heuristic workflows only).

See [LLM_SETUP.md](LLM_SETUP.md) for configuration.
