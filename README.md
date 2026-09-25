# ai-router

Local-first, cost-aware model selection for Claude Code CLI, OpenAI Codex CLI, Grok Build CLI, and one-shot OpenRouter API calls. The router, adapters, evaluation tools, and optional PAW integration are written in Rust. It selects a model before a **new task or session** and leaves provider execution to the installed harness.

**Status:** integration-ready routing baseline. On 25 September 2026, routed fast, balanced, and deep invocations were tested through all three installed CLIs on macOS. The 50%+ realized compute regain target is **not yet demonstrated** on a representative workload.

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

The dashboard runs on `http://127.0.0.1:8747/` and refreshes every two seconds. It shows recent decisions, model and tier mix, source mix, cache reuse, and p50/p95 routing latency. Both views accept `--measurements path/to/measurements.jsonl` to display compute regain from a separate matched comparison. No savings percentage is inferred from routing events alone. The dashboard is rendered by Rust and uses HTML/CSS with no JavaScript or external application service; Google Fonts are optional. It binds only to localhost.

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

The final command uses [OpenRouter's chat completions API](https://openrouter.ai/docs/quickstart) and prints the response plus reported token usage. It requires a key and network connection and supports single-turn text tasks; it does not replace a coding harness's tool loop or its subscription billing. The endpoint can be changed in TOML for a compatible local test server. Remote endpoints must use HTTPS. No live paid OpenRouter call was made during development because no `OPENROUTER_API_KEY` was present; the request/response path is covered by a local server test.

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

Record matched baseline and routed runs as JSONL with `baseline_compute`, `routed_compute`, `baseline_success`, and `routed_success`, following `examples/measurements.jsonl`. Use the same compute unit for both runs: API spend, billable token equivalent, or subscription quota consumption. Include retries and router overhead in routed compute.

```sh
./target/release/ai-router savings examples/measurements.jsonl
```

`savings` computes `1 - routed_compute / baseline_compute`; it marks the 50% target met only if regain is at least 0.5 and routed failures do not exceed baseline failures. The checked-in file is an illustration, not evidence of project savings. A representative shadow and live comparison remains necessary before claiming 50% in production.

## Limits

- New task and new session integration is tested. Existing live-session model switching and subagent routing are harness-specific and are not installed automatically.
- The PAW adapter compiles and supports a local program directory, but this project does not ship trained classifier weights or a program bundle.
- Jev requires a key and network access; its response is a routing signal, not an authorization.
- CLM cannot run with its released reference stack on the tested 8 GB M3 Mac; Kev-0.8B can run locally, but its measured warm routing latency was about 700 ms on this machine.
- Quality and savings depend on workload, model access, retries, and pricing or subscription quotas.

Apache-2.0. [Project site](https://alphaonedev.github.io/ai-router/).
