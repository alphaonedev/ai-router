use ai_router::{load_config, savings, FusionRole, FusionValidation, Measurement};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    time::Instant,
};

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

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    let repo = std::env::current_dir().map_err(|e| e.to_string())?;
    let manifest: Manifest =
        toml::from_str(&fs::read_to_string(&cli.manifest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if !["grok", "claude"].contains(&cli.baseline_client.as_str()) {
        return Err("baseline client must be grok or claude".into());
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
    let mut cfg = load_config(&repo.join("router.toml"))?;
    if !cfg
        .models
        .get(&cli.baseline_client)
        .is_some_and(|models| models.iter().any(|model| model.id == baseline_model))
    {
        return Err("baseline model is not allowlisted for the selected client".into());
    }
    cfg.patch.enabled = true;
    cfg.patch.model = manifest.patch_model.clone();
    cfg.patch.max_file_bytes = 100_000;
    cfg.fusion.lead = FusionRole {
        client: cli.baseline_client.clone(),
        model: baseline_model.into(),
    };
    cfg.fusion.sidekick = FusionRole {
        client: "claude".into(),
        model: "haiku".into(),
    };
    cfg.fusion.validation = vec![FusionValidation {
        program: "cargo".into(),
        args: vec!["test".into(), "-q".into()],
    }];
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
            let mut baseline_command = Command::new(&cli.baseline_client);
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
            let baseline_output = run(&mut baseline_command)?;
            let baseline_duration = start.elapsed().as_millis();
            save_output(&task_dir.join("baseline-agent"), &baseline_output)?;
            let baseline_json = parse_json(&baseline_output)?;
            let baseline_acceptance = check(
                &baseline_dir,
                &["test", "-q", "--test", "benchmark_regression"],
            )?;
            let baseline_suite = check(&baseline_dir, &["test", "-q"])?;
            Arm {
                cost_usd: baseline_json["total_cost_usd"].as_f64(),
                input_tokens: baseline_json["usage"]["input_tokens"].as_u64(),
                output_tokens: baseline_json["usage"]["output_tokens"].as_u64(),
                duration_ms: baseline_duration,
                agent_exit_success: baseline_output.status.success(),
                acceptance_success: baseline_acceptance,
                full_suite_success: baseline_suite,
                mode: format!("{}-single-agent", cli.baseline_client),
            }
        };
        let start = Instant::now();
        let mut routed_command = Command::new(&router_bin);
        routed_command
            .arg("--config")
            .arg(&config_path)
            .args(["adaptive", "--task", &task.prompt, "--workdir"])
            .arg(&routed_dir)
            .args(["--file", &task.file]);
        if let Some(extra_file) = &task.extra_file {
            routed_command.args(["--file", extra_file]);
        }
        let routed_output = run(&mut routed_command)?;
        let routed_duration = start.elapsed().as_millis();
        save_output(&task_dir.join("routed-agent"), &routed_output)?;
        let routed_json = parse_json(&routed_output)?;
        let routed_acceptance = check(
            &routed_dir,
            &["test", "-q", "--test", "benchmark_regression"],
        )?;
        let routed_suite = check(&routed_dir, &["test", "-q"])?;
        let routed = Arm {
            cost_usd: routed_json["total_cost_usd"].as_f64(),
            input_tokens: routed_json["patch"]["prompt_tokens"].as_u64(),
            output_tokens: routed_json["patch"]["completion_tokens"].as_u64(),
            duration_ms: routed_duration,
            agent_exit_success: routed_output.status.success(),
            acceptance_success: routed_acceptance,
            full_suite_success: routed_suite,
            mode: routed_json["mode"].as_str().unwrap_or("unknown").into(),
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
        writeln!(
            results,
            "{}",
            serde_json::to_string(&Record {
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
        )
        .map_err(|e| e.to_string())?;
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
