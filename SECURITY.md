# Security and review notes

The router executes locally with the user's permissions. `run` launches an installed coding CLI; `adaptive` and `fusion` let those CLIs edit the selected worktree and run the TOML validation commands. The optional exact-edit path sends the task and selected file contents to the configured OpenRouter endpoint. The local dashboard binds to `127.0.0.1` and omits task text from its event log.

## Review on 25 September 2026

The Rust source and CLI tests were traced with CodeGraph and reviewed across routing, cache, HTTP adapters, patching, Fusion, observability, and the benchmark runner. `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo check --features paw` passed after the fixes.

| Area | Finding and resolution |
| --- | --- |
| Routing cache | The default tier was absent from the cache key, and delimiter-joined fields could collide. The key now hashes a structured representation including the default tier. |
| Classification | Fast keywords could match inside unrelated words, such as `format` inside `information`. Keyword matching now checks word boundaries. |
| Savings accounting | The report omitted `saved_compute`, and finite inputs could overflow when summed or divided. The report now includes signed savings and rejects non-finite totals and ratios. |
| Quality parity | Equal aggregate failure counts could hide a new failure on a task the baseline passed. The target now requires task-by-task parity. |
| HTTP adapters | API base URLs could contain credentials, queries, or fragments; redirects could move requests away from the configured endpoint. These URL forms are rejected and automatic redirects are disabled. |
| Exact edits | A model response could overwrite a file changed during the request. The file is checked again before replacement, and symlink targets are rejected at write time. |
| Exact-match ambiguity | Overlapping occurrences of `old` could be mistaken for one match. These are now rejected before an edit is applied. |
| Validation failure | An error starting or running validation could leave exact edits in place. Applied edits are now rolled back before returning that error. |
| Fusion validation | A lead could accept when no validation commands were configured. Fusion execution now requires at least one validation command. |
| CLI pass-through | Additional CLI arguments could override the routed model or effort through configuration flags. Those flags are rejected. |
| Terminal observability | Control characters in event labels could affect the terminal display. The watch view now strips them. |
| Benchmark fixtures | A custom manifest could point mutation paths outside its cloned fixture. File paths and task IDs are now constrained before any mutation. |
| TOON CLI | Forced conversion could underflow its percentage calculation when TOON expanded the input. The calculation now uses signed floating-point differences. |

The lockfile was checked against the OSV database for its registry packages. It reported [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436.html) for the unmaintained `paste` crate, reached through the optional PAW/Candle dependency tree. This is an unmaintained advisory, not a reported vulnerability. The default dependency path did not report a known vulnerability in that check.

## Operating boundaries

Keep `router.toml` and any configured validation programs under trusted control. Model output remains untrusted: exact edits are bounded and locally validated, while agent CLI modes can make broader changes with the permissions given to the installed harness. A passing validation suite and a lead acceptance do not prove that a change is correct or safe. Review diffs before committing or deploying. Do not put credentials into prompts or files intentionally sent to a remote model.

For a suspected vulnerability, open a private GitHub security advisory for this repository rather than a public issue.
