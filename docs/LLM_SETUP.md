# LLM setup (optional)

`trace-diff features` discovers API workflows from OpenAPI using **heuristic rules** built into the CLI. That works out of the box after `pip install trace-route-test` — no LLM required.

An **optional LLM refine step** can improve workflow grouping and ordering when a provider is configured. The pip wheel includes LLM client code, but **does not bundle a model or runtime**. You bring one of:

| Path | Best for | Install |
|------|----------|---------|
| **Groq (recommended)** | pip users, no local install | Free API key only |
| **Ollama** | offline / local dev | Install Ollama + pull a model |

Default provider is **`auto`**: use Groq when `GROQ_API_KEY` is set, else Ollama when reachable, else heuristics only.

## Quick check

```bash
trace-diff features --check-llm
```

Example when ready (Groq):

```text
  auto resolved: groq
  model: llama-3.1-8b-instant
  status: ready (groq)
```

Example when no provider is configured:

```text
  auto resolved: none
  ollama: unreachable at http://localhost:11434
  groq: GROQ_API_KEY not set
  status: heuristics only — see docs/LLM_SETUP.md
```

## Groq (recommended for pip users)

1. Create a free key at [console.groq.com](https://console.groq.com).
2. Set the environment variable:

```powershell
# Windows PowerShell
$env:GROQ_API_KEY = "gsk_..."
```

```bash
# macOS / Linux
export GROQ_API_KEY="gsk_..."
```

3. Verify and run:

```bash
trace-diff features --check-llm
trace-diff features https://api.example.com
```

No extra flags needed — `auto` picks Groq when the key is set.

Optional overrides:

| Flag / env | Default | Purpose |
|------------|---------|---------|
| `TRACE_DIFF_AI_PROVIDER` | `auto` | `auto`, `groq`, or `ollama` |
| `TRACE_DIFF_AI_MODEL` | `llama-3.1-8b-instant` | Groq model id |
| `--llm-provider groq` | — | Force Groq |
| `--groq-api-key` | — | Alternative to env var |

## Ollama (local)

1. Install [Ollama](https://ollama.com) and start it.
2. Pull a model:

```bash
ollama pull qwen2.5:7b-instruct
```

3. Run features (Ollama is used when Groq key is absent and Ollama responds):

```bash
trace-diff features --check-llm
trace-diff features https://api.example.com
```

| Flag / env | Default | Purpose |
|------------|---------|---------|
| `OLLAMA_HOST` | `http://localhost:11434` | Ollama API base URL |
| `TRACE_DIFF_AI_MODEL` | `qwen2.5:7b-instruct` | Preferred Ollama model |
| `--llm-provider ollama` | — | Force Ollama even if Groq key is set |

## Disable LLM

```bash
trace-diff features https://api.example.com --no-llm
```

Skips the workflow pipeline entirely and uses flat endpoint discovery instead.

## How it fits discovery

1. Fetch OpenAPI from the target.
2. Build **heuristic workflows** (instant).
3. If a provider is available, **LLM refine** runs in the background (max ~20s).
4. On timeout or failure, validated heuristics are kept.
5. Manifest cached under `.trace-diff/workflows-<host>.json`.

During discovery the TUI status line shows whether workflows came from heuristics, cache, or LLM refine.

## See also

- [FEATURES_AUTODETECT.md](FEATURES_AUTODETECT.md) — full discovery pipeline, auth, CI
- [INSTALL.md](INSTALL.md) — pip install and platform notes
