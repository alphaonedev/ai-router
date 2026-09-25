# ai-router

Local-first, cost-aware model selection for Claude Code CLI, OpenAI Codex CLI, and Grok Build CLI. The router, adapters, evaluation tools, and optional PAW integration are written in Rust. It selects a model before a **new task or session** and leaves provider execution to the installed harness.

**Status:** integration-ready routing baseline. On 25 September 2026, routed fast, balanced, and deep invocations were tested through all three installed CLIs on macOS. The 50%+ realized compute regain target is **not yet demonstrated** on a representative workload.

## Build

```sh
cargo build --release
cargo test
cargo build --release --features paw   # optional local classifier runtime
```

The default binary does not bundle model weights. The `paw` feature uses the Rust Candle backend and adds a large runtime dependency. PAW model and program assets are supplied separately. The compact base model is not bundled in this repository.

## Configure models

`router.json` lists model IDs and effort levels per harness. The checked-in defaults were validated on the maintainer's laptop, but subscription and API model availability varies. Edit them for your account. Increment `policy_version` after any policy change.

The decision order is: explicit `--model`, cached decision, local rules, optional PAW for ambiguous balanced requests, optional Jev for remaining balanced requests, and a policy fallback. `--high-stakes` sets a deep floor unless the caller explicitly selects a model. The router never grants tool permissions or authorizes actions.

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

Any arguments after `--` are passed to the underlying CLI. Model and effort flags must be set through ai-router options or `router.json` to keep the displayed decision consistent with the launched command. The router does not take over an existing session or change its model mid-conversation.

## Integrate with another harness

The library exports typed `Request`, `Decision`, and `route` APIs. The CLI also accepts one JSON request from stdin and returns one JSON decision on stdout:

```sh
printf '%s' '{"client":"codex","task":"Review the change","offline":true}' \
  | ./target/release/ai-router route-json
```

`route --client ... --task ...` provides the same decision via flags. Supported request fields are `client`, `task`, optional `model`, optional `min_tier`, `high_stakes`, and `offline`. Supported clients in the sample config are `claude`, `codex`, and `grok`; library users may supply their own model inventory. Decisions include `model`, `tier`, `effort`, `source`, `reason`, and `duration_ms`. The cache lives under `XDG_CACHE_HOME/ai-router` or `~/.cache/ai-router`, keyed by normalized task, policy, model inventory, and constraints, with a 24-hour lifetime.

## Live routing observatory

Every decision made with a cache directory writes a small event to `events.jsonl`. Events contain the harness, model, tier, decision source, latency, timestamp, and a short task hash. **Prompt text is never written to the event log.** The file rotates at 5 MB. `route`, `route-json`, and `run` all feed the same view.

```sh
./target/release/ai-router watch
./target/release/ai-router watch --once
./target/release/ai-router dashboard --port 8747
```

The dashboard runs on `http://127.0.0.1:8747/` and refreshes every two seconds. It shows recent decisions, model and tier mix, source mix, cache reuse, and p50/p95 routing latency. Both views accept `--measurements path/to/measurements.jsonl` to display compute regain from a separate matched comparison. No savings percentage is inferred from routing events alone. The dashboard is rendered by Rust and uses HTML/CSS with no JavaScript or external application service; Google Fonts are optional. It binds only to localhost.

## Optional PAW and Jev

For PAW, build with `--features paw`. With PAW credentials configured, compile the included classifier specification with `ai-router paw-compile --spec examples/paw-router.spec.txt --slug your-router`; the command returns a program ID and local bundle directory. Set `paw.enabled=true` and `paw.local_dir` to that directory, or set `paw.slug` to the compiled program. The program output contract is exactly one of `fast`, `balanced`, `deep`, or `abstain`. The local directory loader performs local file I/O only and supports `--offline` when the base model is cached. The slug loader may use the network, so `--offline` skips that path. PAW errors and abstentions use local fallback. This repository does not contain a trained, labeled PAW classifier program.

For Jev, set `jev.enabled=true` and `JEV_API_KEY`. Balanced requests may go to [Jev's model-route endpoint](https://www.jevai.org/docs). The Rust policy validates confidence, candidate ID, and risk floor before accepting a response. Errors fall back locally. `--offline` prevents task text from being sent to Jev or the PAW slug loader. Provider CLIs themselves still send tasks to their model services when you use `run`.

Neither optional backend is active in the checked-in configuration. No API credential or PAW classifier program is bundled.

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
- Quality and savings depend on workload, model access, retries, and pricing or subscription quotas.

Apache-2.0. [Project site](https://alphaonedev.github.io/ai-router/).
