# Matched coding-task benchmark

The [ten-task manifest](regressions.toml) fixes ai-router at source commit `730de44`. It includes eight repairs in routing policy, cache invalidation, savings accounting, whitespace handling, and the TOON CLI; one new reporting feature; and one routing-floor repair spanning `src/main.rs` and `src/lib.rs`. Every task starts from two fresh checkouts with the same source state, prompt, and injected acceptance test. The runner confirms both fixtures fail before either agent starts. After each agent runs, it executes the injected test and the full Rust suite in both checkouts.

The baseline uses one Grok 4.7 high-reasoning coding session. The routed path uses `ai-router adaptive --file`, a bounded OpenRouter exact-edit model, local validation, and the configured Claude Haiku routine fallback. The cross-file task passes two `--file` arguments; edits are validated together and rolled back together on failure. If validation still fails, the configured Relay lead and sidekick run; the benchmark profile uses Grok and Claude so every model phase reports a comparable cost. The runner includes rejected API calls, retries, fallback sessions, and escalations in routed cost. Model-reported USD equivalents measure resource consumption here; they are not a subscription invoice or a measurement of joules.

## Reproduce

Use macOS or another supported platform with Rust, `cargo`, Git, authenticated Grok Build and Claude Code CLIs, and an OpenRouter API key. From the repository root:

```sh
cargo build --release --bins
export OPENROUTER_API_KEY=your_key
./target/release/benchmark
```

For a fresh Claude Opus baseline on the same fixtures, use `./target/release/benchmark --baseline-client claude --baseline-model opus`. To compare a changed routing policy without paying for the same baseline calls again, pass `--baseline-results benchmarks/runs/<prior-run>/results.jsonl` with the same client and model. The runner verifies task ID, source reference, prompt, file, model, and successful prior checks; it then creates and validates fresh routed fixtures. Replayed baselines are marked in each result row and copied into the new run directory. Treat this as a paired replay comparison, not another live baseline run.

The runner writes the exact generated TOML, manifest, per-task records, measurements, summary, fixtures, and raw CLI responses under ignored `benchmarks/runs/<uuid>/`. It stops if an injected regression does not fail before repair or if a provider omits a cost, so neither condition can silently improve the savings result. Run `./target/release/ai-router savings benchmarks/runs/<uuid>/measurements.jsonl` to recalculate the aggregate. Use `--start` and `--limit` only for exploratory slices; the published full results use all tasks.

## Results on 25 September 2026

| Configuration | Tasks | Baseline cost | Routed cost | Regain | Baseline / routed failures |
|---|---:|---:|---:|---:|---:|
| [Initial six tasks](evidence/2026-09-25-six-task-failed/summary.json), Grok fast fallback | 6 | $0.48922532 | $0.25626085 | **47.6%** | 0 / 0 |
| [Revised six tasks](evidence/2026-09-25-six-task-revised/summary.json), Claude Haiku fallback | 6 | $0.65405596 | $0.08458580 | **87.1%** | 0 / 0 |
| [Eight tasks](evidence/2026-09-25-eight-task/summary.json), bounded retries and priced fallback | 8 | $0.89390692 | $0.23613540 | **73.6%** | 0 / 0 |
| [Final ten tasks](evidence/2026-09-25-ten-task/summary.json), including feature and cross-file work | 10 | $1.11732364 | $0.25821210 | **76.9%** | 0 / 0 |

The final run used router implementation commit `f744187` on an 8 GB Apple M3 laptop. Its seven exact-edit successes cost $0.0232890 combined; the three Claude fallback tasks cost $0.2349231 combined, including rejected API edits. The full [per-task records](evidence/2026-09-25-ten-task/results.jsonl), [measurement rows](evidence/2026-09-25-ten-task/measurements.jsonl), and [generated TOML](evidence/2026-09-25-ten-task/router.toml) are checked in. The sequential sum of agent duration was 765.8 seconds for baseline and 124.1 seconds for routed; these timings exclude fixture setup and the post-run acceptance checks, so they are not end-to-end wall-clock comparisons.

This suite supports the 50% target for these ten small and medium coding tasks at observed test parity, including one feature and one two-file repair. It does not establish the same savings for large features, high-impact incidents, every model price, or fixed-price subscription bills. The acceptance tests are visible to both agents, and a passing suite cannot prove all behavior. The earlier 47.6% run remains published because fallback choice materially changed the result.

## Native model families and decision backends

The [Anthropic](anthropic-family.toml) and [OpenAI](openai-family.toml) profiles route within one provider family. Both use local Kev as the decision service and keep the coding agent in charge of executing the selected model. The [nine-task family manifest](family-regressions.toml) uses fresh checkouts at source commit `a57df4c` and injected acceptance tests. Run Anthropic first, then OpenAI:

```sh
./target/release/benchmark --manifest benchmarks/family-regressions.toml --family-config benchmarks/anthropic-family.toml --baseline-client claude --baseline-model claude-opus-5-5
./target/release/benchmark --manifest benchmarks/family-regressions.toml --family-config benchmarks/openai-family.toml --baseline-client codex --baseline-model gpt-6-astra
```

Family rows record the configured decision backend, the actual route source, chosen model, decision duration, task checks, and costs. Claude costs come from the CLI's provider-reported total. Codex costs are **short-context API price equivalents** computed from CLI token usage, including cached input; they are not subscription charges. The benchmark records every agent call and rejects missing usage. Regain is a cost ratio only when both arms pass the same checks.

The [Jev via OpenRouter profile](jev-openrouter.toml) is an optional hosted decision backend. It reads `OPENROUTER_API_KEY` from the environment and sends task text to OpenRouter; it is not used by the family benchmark profiles. Local Kev keeps task text on the laptop. The community [openrouter-rs SDK](https://github.com/realmorrisliu/openrouter-rs) gained typed Decisions support in v0.16.0. This project currently uses a small synchronous HTTP adapter for that one endpoint; the tested OpenRouter path works without adding the SDK's wider async dependency graph.

[Laya](https://huggingface.co/convaiinnovations/laya) is another local System One backend. Its server advertises the same `/v1/systemone` request shape, so the [Laya example TOML](../examples/laya-local.toml) works with the existing Rust adapter when `laya-serve` is running on port 8000. Its English checkpoint is about 808 MB and its published preloaded CPU latency is 193–464 ms; those are vendor measurements, not an ai-router result. Laya is not used in the results below.

In a nine-prompt matched decision probe on the 8 GB Apple M3 laptop, both backends returned HTTP 200 on all nine calls. Local Kev had a 114 ms median and an 819 ms nearest-rank p95, driven by one slow call. Jev via OpenRouter had a 296 ms median and 366 ms nearest-rank p95. These are decision-only timings, not model execution times, and nine samples are too few for a stable tail estimate. The configurable local timeout is 750 ms in the family profiles, so a slower Kev response falls back to the safe Rust policy rather than blocking for the old three-second limit.

### Anthropic family result

The first nine-task family run used Claude Opus 5.5 for every baseline task and local Kev plus the family profile for each routed task. Both paths passed all nine injected acceptance tests and full Rust suites. Baseline provider-reported cost was $1.7822454; routed cost was $2.0787604. **Regain was −16.6%; the 50% target failed.** Haiku saved money on the two tasks it handled, but Sonnet spent more on several tasks and outweighed those savings. The [per-task records](evidence/2026-09-25-anthropic-family/results.jsonl), [summary](evidence/2026-09-25-anthropic-family/summary.json), and [TOML profile](evidence/2026-09-25-anthropic-family/router.toml) are public. The public rows omit raw CLI transcripts and correct the decision-backend label to null when a local rule selected the route; the unmodified run artifacts remain ignored on the test laptop.

An aggressive [Haiku/Opus value profile](anthropic-value.toml) reused those nine verified Opus baselines and reran all nine routed arms. It reduced reported cost from $1.7822454 to $1.4278225, a **19.9% regain**, but Haiku failed the explicit-model-override acceptance test and full suite. It therefore failed quality parity and the target. The [per-task records](evidence/2026-09-25-anthropic-value/results.jsonl), [summary](evidence/2026-09-25-anthropic-value/summary.json), and [TOML](evidence/2026-09-25-anthropic-value/router.toml) are retained as a rejected policy experiment. Reusing baselines saves repeated Opus calls, but this is a paired replay comparison rather than a second live baseline.

### OpenAI family result in progress

Eight of nine tasks finished with a Codex Astra baseline and Kev-routed Codex Luna/Sol/Astra. All eight passed their injected acceptance tests and full Rust suites. Their aggregate **short-context API price equivalent** fell from $4.8840 to $1.1958, a **75.5% regain on the completed tasks**. The ninth task stopped before its baseline produced usage because the Codex CLI session returned an authentication error; the separate API key had no quota. This is an incomplete run and does not establish nine-task quality parity or measured subscription savings. The [sanitized eight-task records](evidence/2026-09-25-openai-family-eight-task/results.jsonl), [summary](evidence/2026-09-25-openai-family-eight-task/summary.json), and [TOML](evidence/2026-09-25-openai-family-eight-task/router.toml) exclude the failed raw transcript.
