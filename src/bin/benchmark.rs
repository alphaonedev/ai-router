use ai_router::{load_config, route, savings, Measurement, RelayRole, RelayValidation, Request};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

fn model_event(run_id: &str, stage: &str, client: &str, model: &str, status: &str) {
    let Some(dir) = std::env::var_os("XDG_CACHE_HOME")
        .map(|path| PathBuf::from(path).join("ai-router"))
        .or_else(|| {
            std::env::var_os("HOME").map(|path| PathBuf::from(path).join(".cache/ai-router"))
        })
    else {
        return;
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let record = serde_json::json!({
        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        "run_id": run_id,
        "stage": stage,
        "status": status,
        "client": client,
        "model": model,
        "duration_ms": null,
        "input_tokens": null,
        "output_tokens": null,
        "cache_read_tokens": null,
        "cache_write_tokens": null,
        "cost_usd": null
    });
    if let Ok(mut file) = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join("relay-events.jsonl"))
    {
        let _ = file.write_all(format!("{record}\n").as_bytes());
    }
}

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value = "benchmarks/regressions.toml")]
    manifest: PathBuf,
    #[arg(long, default_value = "benchmarks/runs")]
    output_dir: PathBuf,
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long, default_value_t = 0)]
    start: usize,
    #[arg(long, default_value = "grok")]
    baseline_client: String,
    #[arg(long)]
    baseline_model: Option<String>,
    /// Reuse measured baseline arms from a previous run on the same manifest.
    #[arg(long)]
    baseline_results: Option<PathBuf>,
    /// Use one provider family and direct coding CLIs for both arms.
    #[arg(long)]
    family_config: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Manifest {
    source_ref: String,
    baseline_model: String,
    patch_model: String,
    tasks: Vec<Task>,
}

#[derive(Deserialize)]
struct Task {
    id: String,
    file: String,
    prompt: String,
    #[serde(default)]
    healthy: String,
    #[serde(default)]
    broken: String,
    #[serde(default)]
    extra_file: Option<String>,
    #[serde(default)]
    extra_healthy: String,
    #[serde(default)]
    extra_broken: String,
    test: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Arm {
    cost_usd: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    duration_ms: u128,
    agent_exit_success: bool,
    acceptance_success: bool,
    full_suite_success: bool,
    mode: String,
}

#[derive(Serialize)]
struct Record<'a> {
    task_id: &'a str,
    source_ref: &'a str,
    task_prompt: &'a str,
    file: &'a str,
    baseline_model: &'a str,
    baseline_client: &'a str,
    baseline_reused: bool,
    routed_model: &'a str,
    baseline: Arm,
    routed: Arm,
}

#[derive(Deserialize)]
struct PreviousRecord {
    task_id: String,
    source_ref: String,
    task_prompt: String,
    file: String,
    baseline_model: String,
    baseline_client: String,
    baseline: Arm,
}

fn run(cmd: &mut Command) -> Result<Output, String> {
    cmd.output().map_err(|e| e.to_string())
}

fn save_output(path: &Path, output: &Output) -> Result<(), String> {
    fs::write(path.with_extension("stdout"), &output.stdout).map_err(|e| e.to_string())?;
    fs::write(path.with_extension("stderr"), &output.stderr).map_err(|e| e.to_string())
}

fn check(cwd: &Path, args: &[&str]) -> Result<bool, String> {
    let status = Command::new("cargo")
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|e| e.to_string())?;
    Ok(status.success())
}

fn mutate(root: &Path, file: &str, healthy: &str, broken: &str, id: &str) -> Result<(), String> {
    if healthy.is_empty() {
        if broken.is_empty() {
            return Ok(());
        }
        return Err(format!("{id} has replacement but no source text"));
    }
    if Path::new(file)
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("{id} has unsafe file path"));
    }
    let path = fs::canonicalize(root.join(file)).map_err(|e| e.to_string())?;
    if !path.starts_with(fs::canonicalize(root).map_err(|e| e.to_string())?) {
        return Err(format!("{id} file leaves fixture"));
    }
    let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if raw.matches(healthy).count() != 1 {
        return Err(format!("{id} mutation in {file} is not unique"));
    }
    fs::write(path, raw.replacen(healthy, broken, 1)).map_err(|e| e.to_string())
}

fn prepare(repo: &Path, root: &Path, source_ref: &str, task: &Task) -> Result<(), String> {
    let cloned = run(Command::new("git")
        .args(["clone", "--quiet", "--local"])
        .arg(repo)
        .arg(root))?;
    if !cloned.status.success() {
        return Err(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&cloned.stderr)
        ));
    }
    let checkout = run(Command::new("git")
        .args(["checkout", "--quiet", "--detach", source_ref])
        .current_dir(root))?;
    if !checkout.status.success() {
        return Err(format!(
            "git checkout failed: {}",
            String::from_utf8_lossy(&checkout.stderr)
        ));
    }
    mutate(root, &task.file, &task.healthy, &task.broken, &task.id)?;
    if let Some(file) = &task.extra_file {
        mutate(
            root,
            file,
            &task.extra_healthy,
            &task.extra_broken,
            &task.id,
        )?;
    }
    fs::write(root.join("tests/benchmark_regression.rs"), &task.test).map_err(|e| e.to_string())?;
    Ok(())
}

fn parse_json(output: &Output) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&output.stdout).map_err(|e| format!("agent returned invalid JSON: {e}"))
}

fn agent_usage(client: &str, model: &str, output: &Output) -> Result<(f64, u64, u64), String> {
    if client == "claude" {
        let value = parse_json(output)?;
        let cost = value["total_cost_usd"]
            .as_f64()
            .ok_or("Claude cost missing")?;
        let usage = &value["usage"];
        return Ok((
            cost,
            usage["input_tokens"].as_u64().unwrap_or(0),
            usage["output_tokens"].as_u64().unwrap_or(0),
        ));
    }
    if client == "codex" {
        let (input_price, cached_price, output_price) = match model {
            "gpt-6-luna" => (0.1, 0.01, 0.5),
            "gpt-6-sol" => (2.0, 0.2, 10.0),
            "gpt-6-astra" => (10.0, 1.0, 50.0),
            _ => return Err("no verified API-equivalent price for Codex model".into()),
        };
        let mut input = 0_u64;
        let mut cached = 0_u64;
        let mut output_tokens = 0_u64;
        let mut turns = 0;
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let value: serde_json::Value =
                serde_json::from_str(line).map_err(|e| format!("invalid Codex event: {e}"))?;
            if value["type"] == "turn.completed" {
                let usage = &value["usage"];
                input = input
                    .checked_add(
                        usage["input_tokens"]
                            .as_u64()
                            .ok_or("Codex input tokens missing")?,
                    )
                    .ok_or("input token overflow")?;
                cached = cached
                    .checked_add(usage["cached_input_tokens"].as_u64().unwrap_or(0))
                    .ok_or("cached token overflow")?;
                output_tokens = output_tokens
                    .checked_add(
                        usage["output_tokens"]
                            .as_u64()
                            .ok_or("Codex output tokens missing")?,
                    )
                    .ok_or("output token overflow")?;
                turns += 1;
            }
        }
        if turns == 0 || cached > input {
            return Err("Codex usage missing or invalid".into());
        }
        let cost = ((input - cached) as f64 * input_price
            + cached as f64 * cached_price
            + output_tokens as f64 * output_price)
            / 1_000_000.0;
        return Ok((cost, input, output_tokens));
    }
    Err("unsupported family client".into())
}

#[cfg(test)]
mod family_tests {
    use super::*;

    #[test]
    fn codex_price_equivalent_respects_cached_tokens() {
        let mut output = Command::new("cargo").arg("--version").output().unwrap();
        output.stdout = b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1000000,\"cached_input_tokens\":500000,\"output_tokens\":100000}}\n".to_vec();
        let (cost, input, generated) = agent_usage("codex", "gpt-6-sol", &output).unwrap();
        assert!((cost - 2.1).abs() < 1e-9);
        assert_eq!((input, generated), (1_000_000, 100_000));
    }
}

fn family_command(client: &str, model: &str, prompt: &str, cwd: &Path) -> Result<Command, String> {
    let mut command = Command::new(client);
    match client {
        "claude" => {
            command
                .args([
                    "--print",
                    prompt,
                    "--model",
                    model,
                    "--output-format",
                    "json",
                    "--permission-mode",
                    "acceptEdits",
                ])
                .current_dir(cwd);
        }
        "codex" => {
            command
                .args([
                    "exec",
                    "--model",
                    model,
                    "--json",
                    "--sandbox",
                    "workspace-write",
                    "--skip-git-repo-check",
                    "--ephemeral",
                ])
                .arg("-C")
                .arg(cwd)
                .arg(prompt);
        }
        _ => return Err("family client must be claude or codex".into()),
    }
    Ok(command)
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    let repo = std::env::current_dir().map_err(|e| e.to_string())?;
    let manifest: Manifest =
        toml::from_str(&fs::read_to_string(&cli.manifest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let family_mode = cli.family_config.is_some();
    if !["grok", "claude"].contains(&cli.baseline_client.as_str())
        && !(family_mode && cli.baseline_client == "codex")
    {
        return Err("baseline client must be grok or claude, or codex in family mode".into());
    }
    let baseline_model = cli
        .baseline_model
        .as_deref()
        .unwrap_or(&manifest.baseline_model);
    let mut prior: HashMap<String, PreviousRecord> = HashMap::new();
    if let Some(path) = &cli.baseline_results {
        let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
        for line in raw.lines().filter(|line| !line.trim().is_empty()) {
            let record: PreviousRecord = serde_json::from_str(line).map_err(|e| e.to_string())?;
            if prior.insert(record.task_id.clone(), record).is_some() {
                return Err("duplicate task in baseline results".into());
            }
        }
    }
    let run_id = uuid::Uuid::new_v4().to_string();
    let run_dir = repo.join(&cli.output_dir).join(&run_id);
    fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
    fs::copy(&cli.manifest, run_dir.join("manifest.toml")).map_err(|e| e.to_string())?;
    if let Some(path) = &cli.baseline_results {
        fs::copy(path, run_dir.join("baseline-results.jsonl")).map_err(|e| e.to_string())?;
    }
    let router_bin = repo.join("target/release/ai-router");
    if !router_bin.exists() {
        return Err("build first with cargo build --release --bins".into());
    }
    let mut cfg = load_config(
        cli.family_config
            .as_deref()
            .unwrap_or(&repo.join("router.toml")),
    )?;
    if !cfg
        .models
        .get(&cli.baseline_client)
        .is_some_and(|models| models.iter().any(|model| model.id == baseline_model))
    {
        return Err("baseline model is not allowlisted for the selected client".into());
    }
    if family_mode {
        if cfg.patch.enabled || cfg.relay.enabled || cfg.jev.enabled {
            return Err("family benchmark requires patch, Relay, and Jev disabled".into());
        }
        if cfg.decision_service.name != "kev" || !cfg.decision_service.enabled {
            return Err("family benchmark requires the configured Kev decision service".into());
        }
    } else {
        cfg.patch.enabled = true;
        cfg.patch.model = manifest.patch_model.clone();
        cfg.patch.max_file_bytes = 100_000;
        cfg.relay.lead = RelayRole {
            client: cli.baseline_client.clone(),
            model: baseline_model.into(),
        };
        cfg.relay.sidekick = RelayRole {
            client: "claude".into(),
            model: "haiku".into(),
        };
        cfg.relay.validation = vec![RelayValidation {
            program: "cargo".into(),
            args: vec!["test".into(), "-q".into()],
        }];
    }
    let config_path = run_dir.join("router.toml");
    fs::write(
        &config_path,
        toml::to_string_pretty(&cfg).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut measurements = Vec::new();
    let mut results = fs::File::create(run_dir.join("results.jsonl")).map_err(|e| e.to_string())?;
    let mut savings_file =
        fs::File::create(run_dir.join("measurements.jsonl")).map_err(|e| e.to_string())?;
    let tasks = manifest
        .tasks
        .iter()
        .skip(cli.start)
        .take(cli.limit.unwrap_or(usize::MAX));
    for task in tasks {
        if task.id.is_empty()
            || !task
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(
                "task id must contain only letters, digits, hyphens, or underscores".into(),
            );
        }
        eprintln!("benchmark: {}", task.id);
        let task_dir = run_dir.join(&task.id);
        fs::create_dir_all(&task_dir).map_err(|e| e.to_string())?;
        let baseline_dir = task_dir.join("baseline");
        let routed_dir = task_dir.join("routed");
        if cli.baseline_results.is_none() {
            prepare(&repo, &baseline_dir, &manifest.source_ref, task)?;
        }
        prepare(&repo, &routed_dir, &manifest.source_ref, task)?;
        let fixtures: Vec<&PathBuf> = if cli.baseline_results.is_some() {
            vec![&routed_dir]
        } else {
            vec![&baseline_dir, &routed_dir]
        };
        for fixture in fixtures {
            if check(fixture, &["test", "-q", "--test", "benchmark_regression"])? {
                return Err(format!("{} fixture does not fail before repair", task.id));
            }
        }
        let baseline = if cli.baseline_results.is_some() {
            let previous = prior.remove(&task.id).ok_or("baseline task missing")?;
            if previous.source_ref != manifest.source_ref
                || previous.task_prompt != task.prompt
                || previous.file != task.file
                || previous.baseline_model != baseline_model
                || previous.baseline_client != cli.baseline_client
                || !previous.baseline.agent_exit_success
                || !previous.baseline.acceptance_success
                || !previous.baseline.full_suite_success
                || previous.baseline.cost_usd.is_none()
            {
                return Err(format!("{} prior baseline is not comparable", task.id));
            }
            previous.baseline
        } else {
            let start = Instant::now();
            let mut baseline_command = if family_mode {
                family_command(
                    &cli.baseline_client,
                    baseline_model,
                    &task.prompt,
                    &baseline_dir,
                )?
            } else {
                Command::new(&cli.baseline_client)
            };
            if !family_mode {
                match cli.baseline_client.as_str() {
                    "grok" => {
                        baseline_command
                            .args(["--single", &task.prompt, "--model", baseline_model])
                            .args(["--reasoning-effort", "high", "--output-format", "json"])
                            .args(["--always-approve", "--cwd"])
                            .arg(&baseline_dir);
                    }
                    "claude" => {
                        baseline_command
                            .args(["--print", &task.prompt, "--model", baseline_model])
                            .args([
                                "--output-format",
                                "json",
                                "--permission-mode",
                                "acceptEdits",
                            ])
                            .current_dir(&baseline_dir);
                    }
                    _ => unreachable!(),
                }
            }
            let baseline_run_id = uuid::Uuid::new_v4().to_string();
            model_event(
                &baseline_run_id,
                "benchmark_baseline",
                &cli.baseline_client,
                baseline_model,
                "running",
            );
            let active = Arc::new(AtomicBool::new(true));
            let pulse = Arc::clone(&active);
            let pulse_run = baseline_run_id.clone();
            let pulse_client = cli.baseline_client.clone();
            let pulse_model = baseline_model.to_string();
            let heartbeat = thread::spawn(move || {
                let mut elapsed = 0;
                while pulse.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(1));
                    elapsed += 1;
                    if elapsed >= 30 && pulse.load(Ordering::Relaxed) {
                        model_event(
                            &pulse_run,
                            "benchmark_baseline",
                            &pulse_client,
                            &pulse_model,
                            "running",
                        );
                        elapsed = 0;
                    }
                }
            });
            let result = run(&mut baseline_command);
            active.store(false, Ordering::Relaxed);
            let _ = heartbeat.join();
            let baseline_output = result.inspect_err(|_| {
                model_event(
                    &baseline_run_id,
                    "benchmark_baseline",
                    &cli.baseline_client,
                    baseline_model,
                    "failed",
                );
            })?;
            model_event(
                &baseline_run_id,
                "benchmark_baseline",
                &cli.baseline_client,
                baseline_model,
                if baseline_output.status.success() {
                    "complete"
                } else {
                    "failed"
                },
            );
            let baseline_duration = start.elapsed().as_millis();
            save_output(&task_dir.join("baseline-agent"), &baseline_output)?;
            let (baseline_cost, baseline_input, baseline_output_tokens) = if family_mode {
                agent_usage(&cli.baseline_client, baseline_model, &baseline_output)?
            } else {
                let value = parse_json(&baseline_output)?;
                (
                    value["total_cost_usd"]
                        .as_f64()
                        .ok_or("baseline cost missing")?,
                    value["usage"]["input_tokens"].as_u64().unwrap_or(0),
                    value["usage"]["output_tokens"].as_u64().unwrap_or(0),
                )
            };
            let baseline_acceptance = check(
                &baseline_dir,
                &["test", "-q", "--test", "benchmark_regression"],
            )?;
            let baseline_suite = check(&baseline_dir, &["test", "-q"])?;
            Arm {
                cost_usd: Some(baseline_cost),
                input_tokens: Some(baseline_input),
                output_tokens: Some(baseline_output_tokens),
                duration_ms: baseline_duration,
                agent_exit_success: baseline_output.status.success(),
                acceptance_success: baseline_acceptance,
                full_suite_success: baseline_suite,
                mode: if family_mode && cli.baseline_client == "codex" {
                    "codex-single-agent-api-equivalent".into()
                } else {
                    format!("{}-single-agent", cli.baseline_client)
                },
            }
        };
        let start = Instant::now();
        let decision = if family_mode {
            let cache = std::env::var_os("XDG_CACHE_HOME")
                .map(|p| PathBuf::from(p).join("ai-router"))
                .or_else(|| {
                    std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache/ai-router"))
                });
            Some(route(
                &Request {
                    client: cli.baseline_client.clone(),
                    task: task.prompt.clone(),
                    model: None,
                    min_tier: None,
                    high_stakes: false,
                    offline: true,
                },
                &cfg,
                cache.as_deref(),
            )?)
        } else {
            None
        };
        let routed_model = decision
            .as_ref()
            .map_or(manifest.patch_model.as_str(), |d| d.model.as_str());
        let mut routed_command = if family_mode {
            family_command(
                &cli.baseline_client,
                routed_model,
                &task.prompt,
                &routed_dir,
            )?
        } else {
            let mut command = Command::new(&router_bin);
            command
                .arg("--config")
                .arg(&config_path)
                .args(["adaptive", "--task", &task.prompt, "--workdir"])
                .arg(&routed_dir)
                .args(["--file", &task.file]);
            if let Some(extra_file) = &task.extra_file {
                command.args(["--file", extra_file]);
            }
            command
        };
        let routed_run_id = uuid::Uuid::new_v4().to_string();
        if family_mode {
            model_event(
                &routed_run_id,
                "benchmark_routed",
                &cli.baseline_client,
                routed_model,
                "running",
            );
        }
        let routed_active = Arc::new(AtomicBool::new(true));
        let routed_pulse = Arc::clone(&routed_active);
        let pulse_run = routed_run_id.clone();
        let pulse_client = cli.baseline_client.clone();
        let pulse_model = routed_model.to_string();
        let heartbeat = family_mode.then(|| {
            thread::spawn(move || {
                let mut elapsed = 0;
                while routed_pulse.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(1));
                    elapsed += 1;
                    if elapsed >= 30 && routed_pulse.load(Ordering::Relaxed) {
                        model_event(
                            &pulse_run,
                            "benchmark_routed",
                            &pulse_client,
                            &pulse_model,
                            "running",
                        );
                        elapsed = 0;
                    }
                }
            })
        });
        let routed_result = run(&mut routed_command);
        routed_active.store(false, Ordering::Relaxed);
        if let Some(heartbeat) = heartbeat {
            let _ = heartbeat.join();
        }
        let routed_output = routed_result.inspect_err(|_| {
            if family_mode {
                model_event(
                    &routed_run_id,
                    "benchmark_routed",
                    &cli.baseline_client,
                    routed_model,
                    "failed",
                );
            }
        })?;
        if family_mode {
            model_event(
                &routed_run_id,
                "benchmark_routed",
                &cli.baseline_client,
                routed_model,
                if routed_output.status.success() {
                    "complete"
                } else {
                    "failed"
                },
            );
        }
        let routed_duration = start.elapsed().as_millis();
        save_output(&task_dir.join("routed-agent"), &routed_output)?;
        let (routed_cost, routed_input, routed_output_tokens, routed_mode) = if family_mode {
            let (cost, input, output) =
                agent_usage(&cli.baseline_client, routed_model, &routed_output)?;
            (
                Some(cost),
                Some(input),
                Some(output),
                format!("{}-single-agent-family", cli.baseline_client),
            )
        } else {
            let value = parse_json(&routed_output)?;
            (
                value["total_cost_usd"].as_f64(),
                value["patch"]["prompt_tokens"].as_u64(),
                value["patch"]["completion_tokens"].as_u64(),
                value["mode"].as_str().unwrap_or("unknown").to_string(),
            )
        };
        let routed_acceptance = check(
            &routed_dir,
            &["test", "-q", "--test", "benchmark_regression"],
        )?;
        let routed_suite = check(&routed_dir, &["test", "-q"])?;
        let routed = Arm {
            cost_usd: routed_cost,
            input_tokens: routed_input,
            output_tokens: routed_output_tokens,
            duration_ms: routed_duration,
            agent_exit_success: routed_output.status.success(),
            acceptance_success: routed_acceptance,
            full_suite_success: routed_suite,
            mode: routed_mode,
        };
        let measurement = Measurement {
            baseline_compute: baseline.cost_usd.ok_or("baseline cost missing")?,
            routed_compute: routed.cost_usd.ok_or("routed cost missing")?,
            baseline_success: baseline.agent_exit_success
                && baseline.acceptance_success
                && baseline.full_suite_success,
            routed_success: routed.agent_exit_success
                && routed.acceptance_success
                && routed.full_suite_success,
        };
        let result_row = if family_mode {
            serde_json::json!({
                "task_id": task.id, "source_ref": manifest.source_ref, "task_prompt": task.prompt,
                "file": task.file, "baseline_model": baseline_model, "baseline_client": cli.baseline_client,
                "baseline_reused": cli.baseline_results.is_some(), "routed_model": routed_model,
                "decision_source": decision.as_ref().map(|d| d.source.as_str()),
                "decision_reason": decision.as_ref().map(|d| d.reason.as_str()),
                "decision_backend": decision.as_ref().and_then(|d| (d.source == "system_one").then_some(cfg.decision_service.name.as_str())),
                "configured_decision_backend": cfg.decision_service.name,
                "decision_duration_ms": decision.as_ref().map(|d| d.duration_ms),
                "cost_basis": if cli.baseline_client == "codex" { "OpenAI short-context API price equivalent; subscription billing may differ" } else { "Claude CLI provider-reported USD" },
                "baseline": baseline, "routed": routed,
            })
        } else {
            serde_json::to_value(Record {
                task_id: &task.id,
                source_ref: &manifest.source_ref,
                task_prompt: &task.prompt,
                file: &task.file,
                baseline_model,
                baseline_client: &cli.baseline_client,
                baseline_reused: cli.baseline_results.is_some(),
                routed_model: &manifest.patch_model,
                baseline,
                routed,
            })
            .map_err(|e| e.to_string())?
        };
        writeln!(results, "{result_row}").map_err(|e| e.to_string())?;
        writeln!(
            savings_file,
            "{}",
            serde_json::to_string(&measurement).map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?;
        measurements.push(measurement);
        let interim = savings(&measurements)?;
        eprintln!(
            "benchmark: {} complete; {} tasks, {:.1}% regain, {} routed failures",
            task.id,
            interim.tasks,
            interim.regain_fraction * 100.0,
            interim.routed_failures
        );
    }
    let report = savings(&measurements)?;
    fs::write(
        run_dir.join("summary.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::json!({"run_dir":run_dir,"summary":report})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_mutation_rejects_parent_traversal() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("fixture");
        fs::create_dir(&root).unwrap();
        let outside = parent.path().join("outside.rs");
        fs::write(&outside, "safe").unwrap();
        assert!(mutate(&root, "../outside.rs", "safe", "broken", "case").is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "safe");
    }
}
