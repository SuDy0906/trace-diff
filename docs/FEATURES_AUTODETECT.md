# Features auto-detect

Auto-detect pages, **API workflow scenarios**, and common paths from a site. Same interactive TUI — select scenarios and run.

```powershell
trace-diff features https://api.confuciusai.io
trace-diff features https://api.confuciusai.io --no-llm   # skip workflow pipeline
trace-diff features --check-llm                         # verify LLM provider setup
trace-diff features --check-llm --json                  # machine-readable status
trace-diff features https://api.confuciusai.io --manifest .trace-diff/workflows-api-confuciusai-io.json
```

## Hybrid flow detection (rules + optional LLM)

Discovery uses a **robust pipeline** — not naive string matching:

1. **Fetch OpenAPI** from the target (if published).
2. **Build endpoint index** — paths, methods, tags, `securitySchemes`, request bodies, query params, responses.
3. **Heuristic classifiers** (instant) — groups **all eligible GET endpoints** into chunked flows (~5 endpoints per FLOW row, up to 48 flows):
   - **Health smoke** — only strict health paths (`/health`, `/api/health`, `/ready`, `/live`, `/ping`). Does *not* include `/billing/status` or other `*/status` routes.
   - **Auth smoke** — user realm: `/api/auth/login` → capture token → profile GETs.
   - **Domain flows** — grouped by OpenAPI tag with **per-realm auth** (user / annotator / admin).
   - **Write smokes** — tagged `WRITE` / `kind: write`, mutating POSTs with schema stub bodies. Shown in the TUI but **not selected by default**. CI skips them unless `--include-writes`.
4. **Validation gate** — drops invalid flows (wrong health endpoints, missing auth chains, destructive first steps).
5. **LLM refine** (background, max ~20s when Ollama/Groq available) — improves ordering and grouping; falls back to validated heuristics on timeout.
6. Saves manifest to `.trace-diff/workflows-<host>.json` (`manifest_version: 5`).
7. Inserts a **TLS certificate canary** for HTTPS hosts (handshake + days until expiry).
8. TUI shows **FLOW** / **WRITE** / **TLS** rows with step detail panel on highlight. Discovery shows live stages (Fetching OpenAPI…, Building heuristics…, LLM refine…). Press **`R`** to rediscover, **`l`** for LLM status.

No chat UI. When LLM is unavailable, the TUI status line shows **heuristics only** and stderr prints a setup hint. See [LLM_SETUP.md](LLM_SETUP.md).

## TUI keyboard (pip / terminal)

| Screen | Key | Action |
|--------|-----|--------|
| Select | ↑↓ / j k, Space | Move, toggle feature |
| Select | Enter / r | Run selected |
| Select | a / n | Select all / none |
| Select | c | Auth popup (auto-opens if creds missing) |
| Select | d / i / Enter | Inspect steps |
| Select | l | LLM status panel |
| Select | R | Rediscover |
| Select | ? / g | Help / guide |
| Select | t | Cycle theme |
| Discovery | q | Cancel |
| Running | q ×2 | Confirm abort |
| Results | R | Categorized report |
| Results | e | Export JSON |
| Results | b | Back to select |
| Auth popup | Esc ×2 | Skip without saving |

Piped or non-TTY stdout runs headless automatically (equivalent to `-y`).

## Multi-role auth (user / annotator / admin)

OpenAPI paths and tags infer an **auth realm** per FLOW:

| Realm | Example paths | Credentials |
|-------|---------------|-------------|
| **user** | `/api/auth/*`, billing | email + password |
| **annotator** | `/api/annotators/*` | annotator email + password |
| **admin** | `/api/admin/*` | secret key only (no email) |

Admin FLOWs send the secret as Bearer or `X-Admin-Key` (from OpenAPI `securitySchemes`) — no login step.

### TUI auth popup

When discovery finds multiple realms and any realm is missing credentials, a **popup opens automatically** with every field required for login (from OpenAPI — e.g. `email`, `password`, `captcha_token`, optional `bearer token`, admin `X-Admin-Secret`). Press **`c`** anytime on the Select screen to edit auth.

- **`*`** marks required fields; optional bearer token skips login/captcha entirely
- Each focused field shows **“How to get this value”** at the bottom (DevTools steps, env var names)
- Tab / ↑↓ — move between fields (j/k only on realm toggles, not while typing)
- Space — toggle realm on/off (unchecked realms deselect their FLOWs on save)
- Enter — save and continue
- Esc ×2 — skip without saving (keep env/file creds only)
- Env vars already set are shown by name (e.g. `Env set: TRACE_DIFF_EMAIL`) — values never displayed

Leave a realm blank to skip it — those FLOWs stay **Reachable** (yellow) or are deselected if the realm checkbox is off.

CI (`-y`) never shows the popup — use env or `--auth-file` only.

## Auth profiles (yellow → green)

Yellow **Reachable** means the route exists but login/body is missing. Set credentials so FLOW steps capture tokens and turn **Healthy**:

```powershell
$env:TRACE_DIFF_EMAIL = "user@example.com"
$env:TRACE_DIFF_PASSWORD = "..."
$env:TRACE_DIFF_ANNOTATOR_EMAIL = "annotator@example.com"
$env:TRACE_DIFF_ANNOTATOR_PASSWORD = "..."
$env:TRACE_DIFF_ADMIN_SECRET = "your-admin-secret"
trace-diff features https://api.confuciusai.io
```

CLI flags override env for the **user** realm:

```powershell
trace-diff features https://api.confuciusai.io --email user@example.com --password "..."
trace-diff features https://api.confuciusai.io --auth-file auth.json
trace-diff features https://api.confuciusai.io --bearer-token $env:TOKEN
```

Multi-profile `auth.json`:

```json
{
  "profiles": {
    "user": { "email": "user@example.com", "password": "..." },
    "annotator": { "email": "ann@example.com", "password": "..." },
    "admin": { "secret": "admin-key-here" }
  }
}
```

Legacy single-profile JSON still works (`email`, `password`, `bearer_token` at top level → user realm).

`--bearer-token` (or `TRACE_DIFF_BEARER_TOKEN`) skips capture-login steps when no password login is set.

## Per-step contract

Each FLOW step is scored independently:

- HTTP status is classified (2xx green, 401/422 yellow, 5xx/404 red).
- Login steps expect **HTTP 200** (or spec `responses`) and require the token field from OpenAPI (`access_token`, `token`, …).
- Steps without credentials for their realm are **skipped** with message `auth skipped (no {realm} creds)` — **Reachable**, not Failed.
- Manifest steps may set `"expect_status"`, `"auth_realm"`, `"auth_mode"`, `"operation_id"`.

Inspect a row with `d` / `i` / Enter — captured tokens show as `token saved`.

## CI (`--yes-all` + `--manifest`)

Headless run fails the process on **Failed** rows. Optional gates:

| Flag | Effect |
|------|--------|
| `--yes-all` / `-y` | Skip TUI; run selected features (also auto when stdout is not a TTY) |
| `--manifest PATH` | Load workflow JSON (version 4 or 5) or a flat endpoint list |
| `--fail-on-reachable` | Also fail yellow auth/body rows |
| `--fail-if-ttfb-exceeds 250ms` | Fail if any selected probe TTFB exceeds the limit |
| `--include-writes` | Run mutating write-smoke FLOWs |
| `--no-tls-canary` | Skip the TLS certificate row |
| `--cert-warn-days 21` | Yellow if the cert expires sooner |
| `--max-features 64` | Cap (TLS is always kept; writes still need `--include-writes`) |
| `--json` | Print the full report |

```powershell
trace-diff features https://api.confuciusai.io -y --manifest .trace-diff/workflows-api-confuciusai-io.json --auth-file auth.json --fail-if-ttfb-exceeds 250ms
```

Exit non-zero if any row is **Failed**, or if TTFB exceeds the limit.

## TLS + cert canary

HTTPS discovery adds a **TLS certificate** row: handshake, protocol version, and days until expiry. Expired certs are **Failed**. Expiry inside `--cert-warn-days` is **Reachable** (yellow). Healthy otherwise.

## LLM setup (optional)

Heuristic workflows work without any LLM. For optional refine, **Groq is recommended** for pip users (API key only — no local install). Full guide: [LLM_SETUP.md](LLM_SETUP.md).

```powershell
$env:GROQ_API_KEY = "gsk_..."   # free at https://console.groq.com
trace-diff features --check-llm
trace-diff features https://api.confuciusai.io
```

Local alternative:

```powershell
ollama pull qwen2.5:7b-instruct
trace-diff features https://api.confuciusai.io
```

| Flag / env | Default | Purpose |
|------------|---------|---------|
| `--check-llm` | off | Print provider status and exit |
| `--check-llm --json` | off | JSON provider status (CI / setup scripts) |
| `--no-llm` | off | Skip workflow pipeline (flat endpoint list) |
| `TRACE_DIFF_AI_PROVIDER` | `auto` | `auto`, `groq`, or `ollama` (`auto` prefers Groq key, then Ollama) |
| `GROQ_API_KEY` | — | Groq API key (recommended) |
| `OLLAMA_HOST` | `http://localhost:11434` | Local Ollama |
| `TRACE_DIFF_AI_MODEL` | provider default | Override model id |

## Cache invalidation

Workflow manifests include `"manifest_version": 5`. Version 4 manifests still load; older versions are ignored and regenerated on next run.

Delete manually to force refresh:

```powershell
Remove-Item .trace-diff/workflows-api-confuciusai-io.json
```

## Manual manifest format (v5)

```json
{
  "manifest_version": 5,
  "base_url": "https://api.confuciusai.io",
  "workflows": [
    {
      "id": "auth_smoke",
      "label": "Auth smoke",
      "kind": "read",
      "auth_realm": "user",
      "steps": [
        {
          "name": "login",
          "method": "POST",
          "path": "/api/auth/login",
          "operation_id": "loginUser",
          "auth_realm": "user",
          "auth_mode": "bearer_capture",
          "body": { "email": "${CONFUCIUS_EMAIL}", "password": "${CONFUCIUS_PASSWORD}" },
          "capture_bearer": "access_token",
          "expect_status": 200
        },
        {
          "name": "me",
          "method": "GET",
          "path": "/api/auth/me",
          "auth_realm": "user",
          "auth_mode": "bearer_capture",
          "use_bearer": true,
          "expect_status": 200
        }
      ]
    }
  ]
}
```

Write smokes use `"kind": "write"`. Load with `--manifest workflows.json`.

## Reachability vs E2E

Workflow runs execute real multi-step HTTP. **Reachable** (yellow) still means auth/body needed on a single probe; workflows aim for **green** when credentials are set.

This is scenario reachability testing, not browser UI tests.
