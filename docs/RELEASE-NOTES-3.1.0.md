# Cortex 3.1.0

A focused release centered on **run observability** — seeing what your agents
actually did, how reliable each model is, and reaching any model you host.

## New

**Agent Reliability Dashboard.** A new view under Observability that aggregates
your local run history into per-provider and per-model success rate, p50/p95
latency, token totals, and an estimated cost, with a time-range filter and
CSV/JSON export. A failing row links straight to the run that failed.

**Run Replay / Agent Black Box.** Pick any past run and play it back as the
timeline it was — the prompt, why that model was chosen, each tool call and
approval, file edits, errors, and the result, with per-run cost. Export a run as
a redacted JSONL. Read-only; nothing leaves your machine.

**Homelab Model Fabric.** Add any OpenAI-compatible endpoint — a vLLM/llama.cpp/
LM Studio box on your LAN or tailnet, or a hosted API — from Settings → Providers
→ Model fabric. Health-check it, discover its models, latency-test it, and chat
through it as a `fabric-<name>` agent. Reachability probes never send your API
key.

## Fixed / improved

- Gateway runs now record real token usage (previously reported zero), so cost
  and usage rollups reflect the primary path.
- The routing reason for each turn is captured (and shown in Run Replay).

## Notes

- Reliability metrics are a local view (no visibility into gateway-internal
  retries); cost is an estimate. Both are labelled as such in the UI.
- Model Fabric routing is explicit for now — pick the endpoint's model in the
  composer. Automatic local-first routing will come later.
