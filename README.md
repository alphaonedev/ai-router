# ai-router

Local-first, cost-aware model selection for Claude Code, OpenAI Codex, and Grok Build. The decision engine and CLI are written in Rust. It selects a model before launching a new task; it does not change an existing agent session or execute model calls itself.

**Status:** v0.1 policy baseline. The 50%+ realized AI compute regain goal is an evaluation target, not a measured result. This release includes rules, explicit overrides, local caching, optional Jev escalation, TOON conversion, and a labeled evaluation command. PAW inference is planned, not shipped; the `paw-rs` runtime and model footprint need validation before inclusion.

## Build

```sh
cargo build --release
cargo test
```

## Quick start

Edit `router.json` to match the models actually available on your subscription or API account. Then run:

```sh
./target/release/ai-router route --client claude --task 'Summarize this function' --offline
./target/release/ai-router run claude --task 'Summarize this function' --offline
./target/release/ai-router run codex --task 'Debug a production race condition' --offline
./target/release/ai-router run grok --task 'Implement a new feature' --offline
```

`run` starts a fresh, noninteractive task using each CLI's model flag. Use `route` for interactive workflows and apply the returned model yourself. Explicit `--model` has highest priority. `--high-stakes` raises the floor to deep, except for an explicit model choice. `--min-tier` accepts `fast`, `balanced`, or `deep`. The cache expires after 24 hours and is keyed by normalized task, client, models, constraints, and policy version. Increment `policy_version` after changing routing behavior.

The model IDs in `router.json` are examples. Provider availability, subscriptions, and model names change; validate them before paid runs.

## Optional Jev

Set `jev.enabled` to `true` in `router.json` and set `JEV_API_KEY`. Balanced requests may be sent to Jev's model-route endpoint. The local policy validates the returned model, confidence threshold, and risk floor. Errors fall back to local selection. `--offline` prevents task text from leaving the machine. Jev is not used for explicit selections, fast tasks, or deep tasks.

## TOON

The `toon` command converts JSON to TOON using the official Rust implementation and verifies a lossless roundtrip. It keeps JSON when TOON is longer by bytes unless `--force` is supplied.

```sh
./target/release/ai-router toon --input structured-context.json > context.txt
```

Byte savings are only a screening heuristic. Token savings depend on the target model tokenizer. Use TOON for repeated structured records, and compare actual input-token usage before adopting it in a provider prompt. The router does not rewrite arbitrary code or prose.

## Evaluate the 50% goal

Create a JSONL file with `request` and `minimum_tier` fields, following `examples/evaluation.jsonl`. Run:

```sh
./target/release/ai-router evaluate examples/evaluation.jsonl
```

For a real baseline, log per task: baseline model, routed model, provider input/output/cached tokens, price or subscription quota consumption, retries, success, and human quality label. Compute `regain = 1 - (routed total compute / baseline total compute)` on tasks meeting the same quality bar, counting retries and router overhead. For subscriptions, report a separate proxy based on quota or task throughput; dollar spend may not change. Do not claim 50% until a representative shadow run and live A/B comparison show at least 50% with no under-routing increase.

## Limits and roadmap

- Current classifier is deterministic rules, not PAW. A compact PAW adapter is next after runtime/platform and model-download validation.
- The CLI boundary handles fresh tasks. Claude's in-session model changes can affect prompt caching, so no main-session switching is attempted.
- Jev is a network option and sends the task text to its service when enabled.
- Quality and cost gains depend on workload, model availability, pricing, and retries.

Apache-2.0. See [project site](https://alphaonedev.github.io/ai-router/).
