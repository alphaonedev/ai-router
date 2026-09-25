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
