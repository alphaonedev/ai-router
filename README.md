# ai-router

Local-first, cost-aware model selection and opt-in lead–sidekick coding workflow for Claude Code CLI, OpenAI Codex CLI, Grok Build CLI, and one-shot OpenRouter API calls. The router, coordinator, adapters, evaluation tools, and optional PAW integration are written in Rust.

**Status:** integration-ready routing baseline. On 25 September 2026, routed fast, balanced, and deep invocations were tested through all three installed CLIs on macOS. A [matched ten-task benchmark](benchmarks/README.md) measured **76.9% lower model-reported cost**, with all ten tasks passing on both paths. This establishes the 50% target for that documented small and medium coding suite, not for every workload or a subscription invoice. See the [security review](SECURITY.md) for operating boundaries and audit findings.

## Build

```sh
cargo build --release
cargo test
cargo build --release --features paw   # optional local classifier runtime
```

The default binary does not bundle model weights. The `paw` feature uses the Rust Candle backend and adds a large runtime dependency. PAW model and program assets are supplied separately. The compact base model is not bundled in this repository.

## Configure models

`router.toml` lists model IDs and effort levels per harness. The checked-in defaults were validated on the maintainer's laptop, but subscription and API model availability varies. Edit them for your account. Increment `policy_version` after any policy change.

The decision order is: explicit `--model`, cached decision, local rules, optional PAW for ambiguous balanced requests, optional local System One service, optional Jev for remaining balanced requests, and a policy fallback. `--high-stakes` sets a deep floor unless the caller explicitly selects a model. The router never grants tool permissions or authorizes actions.

## Run a task

```sh
./target/release/ai-router run claude --task 'Summarize this function' --offline
./target/release/ai-router run codex --task 'Debug a production race condition' --offline
./target/release/ai-router run grok --task 'Implement a new feature' --offline
```

These start noninteractive sessions using `claude --print`, `codex exec`, and `grok --single`. Add `--interactive` to start a new interactive session with the selected model and task as its opening prompt. Use `--dry-run` to inspect the exact program, arguments, and decision without invoking a provider.

```sh
./target/release/ai-router run codex --task 'Plan a migration' --interactive --dry-run
```

Any arguments after `--` are passed to the underlying CLI. Model and effort flags must be set through ai-router options or `router.toml` to keep the displayed decision consistent with the launched command. The router does not take over an existing session or change its model mid-conversation.

## Fusion workflow

### Adaptive entry point

`adaptive` classifies a new task using the configured router. Below `[fusion].min_tier` (default `deep`), it can first ask a low-cost OpenRouter model for bounded exact edits to explicitly named files. The Rust coordinator accepts only exact, unique replacements, validates the combined change, and rolls back every edit if validation fails. `[patch].max_attempts` bounds retries with validation feedback; optional `[patch].retry_model` selects a different allowlisted model after the first attempt. A rejected edit or unavailable API goes to `[fusion.routine]` (Claude Haiku in the checked-in TOML). Failed validation there escalates to Fusion when `[fusion].enabled = true`; otherwise adaptive reports a failure. The reported cost includes rejected patch calls and fallbacks. At or above the threshold, it starts Fusion when `[fusion].enabled = true`. A green validation command does not prove every behavior; use checks that exercise the task's acceptance criteria.

```sh
./target/release/ai-router adaptive --task 'Fix the parser test' --dry-run
./target/release/ai-router adaptive --task 'Fix the parser test' --workdir .
./target/release/ai-router adaptive --task 'Fix the parser test' --file src/parser.rs --workdir .
./target/release/ai-router adaptive --task 'Fix the CLI and core floor' --file src/main.rs --file src/lib.rs --workdir .
./target/release/ai-router adaptive --task 'Investigate a production race condition' --high-stakes --workdir .
```

The classifier decision and the execution path are shown in the dry run. Each `--file` must be a unique relative path inside `--workdir`, with at most four files; no file is sent to OpenRouter without it. `--offline` skips the OpenRouter patch and limits optional decision services, though the selected coding CLI still uses its own model service. `min_tier`, roles, and validation commands are TOML settings. The checked-in `[patch]` is enabled but activates only when `OPENROUTER_API_KEY` exists. For cost comparisons, include failed patch calls, single-agent attempts, and escalations.

This repository implements its own Rust coordinator. The configured Claude, Codex, or Grok CLI may call its own model provider. `fusion` runs a lead planning phase, a sidekick implementation phase, local validation, and a lead review. The lead works in an isolated copy of Git-tracked and nonignored untracked files, refreshed before review; the sidekick works in the original repository. The copy prevents accidental relative-path writes by the lead from changing original files; it is not an OS security sandbox for absolute paths or external tools. The lead and sidekick keep separate resumable CLI sessions on fixed models. A review requesting changes sends bounded feedback to the same sidekick session, then returns to the same lead session. Roles, handoff size, correction limit, per-phase timeout, and validation commands live in `[fusion]`, `[fusion.lead]`, `[fusion.sidekick]`, and `[[fusion.validation]]` in `router.toml`. Validation commands run by the Rust coordinator in the workdir and must pass before acceptance. The selected models must be in the relevant allowlists. The lead's `DECISION: ACCEPT` is an agent review result, not a substitute for independent validation.

```sh
./target/release/ai-router fusion --task 'Add tests for the parser' --dry-run
./target/release/ai-router fusion --task 'Add tests for the parser' --workdir .
```

The command prints a JSON report with phase durations, model IDs, input/output and cache token fields when reported, validation results, provider-reported API cost, and review outcome. CLI subscription usage does not necessarily have a dollar price; `total_cost_usd` remains null when a harness does not report it. Provider usage fields have different meanings and are not directly comparable as billable compute without price and cache adjustments. The lead also receives a bounded git status and tracked diff excerpt; it can inspect the copied files directly. Snapshots have a 256 MiB limit and reject symlinks and non-UTF8 paths. The workflow does not commit or push. It cannot attach to this already-running Codex conversation; launch a new task through `fusion` to use it. The optional local Kev model classifies routing tasks and does not act as a coding sidekick.

## Integrate with another harness

The library exports typed `Request`, `Decision`, and `route` APIs. The CLI also accepts one JSON request from stdin and returns one JSON decision on stdout:

```sh
printf '%s' '{"client":"codex","task":"Review the change","offline":true}' \
  | ./target/release/ai-router route-json
```

`route --client ... --task ...` provides the same decision via flags. Supported request fields are `client`, `task`, optional `model`, optional `min_tier`, `high_stakes`, and `offline`. Supported clients in the sample config are `claude`, `codex`, `grok`, and `openrouter`; library users may supply their own model inventory. Decisions include `model`, `tier`, `effort`, `source`, `reason`, and `duration_ms`. The cache lives under `XDG_CACHE_HOME/ai-router` or `~/.cache/ai-router`, keyed by normalized task, policy, model inventory, and constraints, with a 24-hour lifetime.

## Live routing observatory

Every decision made with a cache directory writes a small event to `events.jsonl`. Events contain the harness, model, tier, decision source, latency, timestamp, and a short task hash. **Prompt text is never written to the event log.** The file rotates at 5 MB. `route`, `route-json`, and `run` all feed the same view.

```sh
./target/release/ai-router watch
./target/release/ai-router watch --once
./target/release/ai-router dashboard --port 8747
```

The dashboard runs on `http://127.0.0.1:8747/` and refreshes every two seconds. It shows recent decisions, model and tier mix, source mix, cache reuse, and p50/p95 routing latency. Fusion also writes phase status and usage to `fusion-events.jsonl`; the dashboard and terminal watch show this stream. Prompt text and review prose are never logged. Both views accept `--measurements path/to/measurements.jsonl` to display compute regain from a separate matched comparison. No savings percentage is inferred from routing events alone. The dashboard is rendered by Rust and uses HTML/CSS with no JavaScript or external application service; Google Fonts are optional. It binds only to localhost.

## Optional decision backends: PAW, local System One, Jev

For PAW, build with `--features paw`. With PAW credentials configured, compile the included classifier specification with `ai-router paw-compile --spec examples/paw-router.spec.txt --slug your-router`; the command returns a program ID and local bundle directory. Set `paw.enabled=true` and `paw.local_dir` to that directory, or set `paw.slug` to the compiled program. The program output contract is exactly one of `fast`, `balanced`, `deep`, or `abstain`. The local directory loader performs local file I/O only and supports `--offline` when the base model is cached. The slug loader may use the network, so `--offline` skips that path. PAW errors and abstentions use local fallback. This repository does not contain a trained, labeled PAW classifier program.

To use the Kev server running on this Mac, pass the local `router.local.toml` created during setup, or change only the `[decision_service]` table in `router.toml`:

```toml
[decision_service]
name = "kev"
enabled = true
base_url = "http://127.0.0.1:8009"
model = "kev-latest"
path = "/v1/systemone"
min_margin = 0.2
```

Start that server separately with `uv run --extra serve python -m kev.serve --run jaredpalmer/kev-0.8b --port 8009` from a Kev checkout. The Rust router can then use it with `--config router.local.toml --offline` (put `--config` before the subcommand).

The `[decision_service]` TOML table accepts any compatible typed-choice endpoint. Set `name`, `base_url`, `model`, `path`, `enabled=true`, and `min_margin`. The adapter sends a choice question with `fast`, `balanced`, `deep`, and `abstain` candidates. It validates the returned class and confidence margin, then applies the configured tier floor and model allowlist. An unavailable server or low margin falls through to Jev or local policy. `--offline` permits only loopback endpoints. The score is a provider-specific margin, not calibrated probability. For [CLM](https://github.com/Contrastive-LM/CLM), use `/v1/systemone`; its released reference stack requires Linux, an NVIDIA GPU, and a Qwen3-8B pooling encoder. The trend's reported speedups and coding verifier results are upstream benchmarks, not measured ai-router results. [Kev](https://github.com/jaredpalmer/kev) uses the same path and supports an Apple Silicon 0.8B checkpoint. [Laya](https://github.com/NandhaKishorM/laya) documents a `/predict` endpoint, so set `path="/predict"` and leave `model=""` when using its example server. Other OpenJev-style servers may use the same wire shape; confirm their exact response and run your own labeled routing evaluation. On an 8 GB M3 Mac, Kev-0.8B returned a routed choice through this adapter in about 700 ms warm after a roughly 10.5 s first request; that is a local compatibility test, not a quality benchmark. The external model server is separate from this Rust codebase.

For Jev, set `jev.enabled=true` and `JEV_API_KEY`. Balanced requests may go to [Jev's model-route endpoint](https://www.jevai.org/docs). The Rust policy validates confidence, candidate ID, and risk floor before accepting a response. Errors fall back locally. `--offline` prevents task text from being sent to Jev or the PAW slug loader. Provider CLIs themselves still send tasks to their model services when you use `run`.

No optional decision backend is active in the checked-in configuration. No API credential or PAW classifier program is bundled.

## OpenRouter API models

`[openrouter]` in `router.toml` holds the API base URL and the name of the environment variable containing the key. `[[models.openrouter]]` declares the allowed fast, balanced, and deep model IDs. No key is stored in TOML. Check configured IDs against OpenRouter's live catalog, inspect a decision, then make a one-shot API call:

```sh
./target/release/ai-router openrouter-models
./target/release/ai-router run openrouter --task 'Summarize this function' --dry-run
OPENROUTER_API_KEY=... ./target/release/ai-router run openrouter --task 'Summarize this function'
```

The final command uses [OpenRouter's chat completions API](https://openrouter.ai/docs/quickstart) and prints the response plus reported token usage. It requires a key and network connection and supports single-turn text tasks; it does not replace a coding harness's tool loop or its subscription billing. The endpoint can be changed in TOML for a compatible local test server. Remote endpoints must use HTTPS. A live key-backed call to `google/gemini-3.5-flash-lite` returned `ROUTER_OK` with 6 input and 4 output tokens on 25 September 2026.

Gemma 4 26B A4B is also allowlisted as paid and free OpenRouter models. Select either ID explicitly with `run openrouter --model ...`; the free endpoint is rate limited and model outputs still need local validation. For local Gemma, [Google documents Ollama tags](https://ai.google.dev/gemma/docs/integrations/ollama) including `gemma4:e2b` and `gemma4:e4b`. Start an OpenAI-compatible local server, set `[openrouter].base_url = "http://127.0.0.1:11434/v1"` and a matching `[[models.openrouter]]` ID in a separate TOML config, then use a nonsecret placeholder `OPENROUTER_API_KEY=local`. On this 8 GB M3 laptop, start with a quantized E2B model and measure memory, latency, and patch quality before considering larger variants. Inference kernels are supplied by the local runtime, outside ai-router's Rust routing layer.

## TOON

The `toon` command converts JSON using the official Rust TOON implementation, verifies a lossless roundtrip, and keeps JSON when TOON is longer by bytes unless `--force` is supplied.

```sh
./target/release/ai-router toon --input structured-context.json > context.txt
```

Byte savings are a screening heuristic, not a token measurement. Use TOON for repeated structured data, and compare actual provider input-token usage. The router does not rewrite code, prose, or provider protocol messages.

## Evaluate routing and regain

Label task examples with the minimum acceptable tier, following `examples/evaluation.jsonl`:

```sh
./target/release/ai-router evaluate examples/evaluation.jsonl
```

Record matched baseline and routed runs as JSONL with `baseline_compute`, `routed_compute`, `baseline_success`, and `routed_success`, following `examples/measurements.jsonl`. Use the same compute unit for both runs: API spend, billable token equivalent, or subscription quota consumption. Include lead planning, sidekick execution, all review and correction rounds, retries, and router overhead in Fusion compute. Test on isolated copies or worktrees of the same starting commit with the same tasks, acceptance checks, and time window. Compare completed tasks at quality parity; split tasks by size and difficulty, report p50/p95 cost and latency, and track unsuccessful runs separately. A single development task is a smoke test, not evidence of a 50% gain.

```sh
./target/release/ai-router savings examples/measurements.jsonl
```

`savings` computes `1 - routed_compute / baseline_compute`; it marks the 50% target met only if regain is at least 0.5 and routed failures do not exceed baseline failures. The `examples/measurements.jsonl` file is illustrative. The reproducible [ten-task ai-router coding benchmark](benchmarks/README.md) measured **76.9% lower provider-reported cost** with 0 failures on either path, including rejected edits and fallback calls. The suite includes policy and CLI repairs, a new feature, and a two-file repair. This establishes the target for that documented workload; broader production and subscription savings remain unmeasured.

### Measured Fusion smoke test

On 25 September 2026, the same one-line README edit passed in two fresh Git fixtures. A single Grok 4.7 agent reported $0.02316556 equivalent cost and finished in about 11 seconds. Grok 4.7 as lead plus Claude Haiku as sidekick reported $0.07125830 combined equivalent cost and finished in about 45 seconds. Fusion cost **3.08× more** on this tiny task; the 50% target was not met. These are provider-reported CLI cost figures, not an invoice or a representative workload. The sample is recorded in `examples/fusion-smoke-measurement.jsonl` and can be checked with `ai-router savings examples/fusion-smoke-measurement.jsonl`. This result argues for keeping simple tasks on one agent and evaluating Fusion on larger tasks where implementation, retries, and review dominate cost.

## Limits

- New task and new session integration is tested. Existing live-session model switching and subagent routing are harness-specific and are not installed automatically.
- The PAW adapter compiles and supports a local program directory, but this project does not ship trained classifier weights or a program bundle.
- Jev requires a key and network access; its response is a routing signal, not an authorization.
- CLM cannot run with its released reference stack on the tested 8 GB M3 Mac; Kev-0.8B can run locally, but its measured warm routing latency was about 700 ms on this machine.
- Quality and savings depend on workload, model access, retries, and pricing or subscription quotas.

Apache-2.0. [Project site](https://alphaonedev.github.io/ai-router/).
