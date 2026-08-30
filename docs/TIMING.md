# Timing guarantees & clock disclaimer

## What we measure with

Phase durations use **`std::time::Instant`** (monotonic clock):

- DNS, TCP, TLS, TTFB, transfer  
- Per-probe RTT samples for traceroute hops  

Monotonic clocks **do not jump backwards** when NTP steps the wall clock. They are the correct basis for “how long did this phase take on this machine.”

## Wall-clock fields

Fields such as `measured_at` / `created_at` use the **system wall clock** (`chrono::Utc::now()`). These can jump if the OS synchronizes time (NTP/SNTP).

## Cross-machine comparisons

When comparing baselines across different hosts or VMs:

1. Prefer **relative deltas** (percent / ms differences of Instant-based phases), not absolute wall timestamps.  
2. Expect some variance from CPU scheduling, turbo clocks, and virtualization.  
3. Use `--samples`-style repeats (when available) or multiple runs before promoting a baseline.  
4. Record `meta` (OS, arch, privileges, probe mode) so diffs are apples-to-apples.

## Reproducible run metadata

Each saved run may include:

```json
{
  "tool_version": "0.1.0",
  "os": "windows",
  "arch": "x86_64",
  "probe_mode": "l7_only",
  "privileges": "unprivileged",
  "timing_basis": "std::time::Instant (monotonic)",
  "timing_disclaimer": "..."
}
```

Enable verbose probe logs with `-v` or `--debug` (stderr).
