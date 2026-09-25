use ai_router::{load_config, savings, FusionRole, FusionValidation, Measurement};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
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
    healthy: String,
    broken: String,
    test: String,
}

#[derive(Serialize)]
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
    routed_model: &'a str,
    baseline: Arm,
    routed: Arm,
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
    let path = root.join(&task.file);
    let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if task.healthy.is_empty() || raw.matches(&task.healthy).count() != 1 {
        return Err(format!("{} mutation is not unique", task.id));
    }
    fs::write(path, raw.replacen(&task.healthy, &task.broken, 1)).map_err(|e| e.to_string())?;
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
    let run_id = uuid::Uuid::new_v4().to_string();
    let run_dir = repo.join(&cli.output_dir).join(&run_id);
    fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
    fs::copy(&cli.manifest, run_dir.join("manifest.toml")).map_err(|e| e.to_string())?;
    let router_bin = repo.join("target/release/ai-router");
    if !router_bin.exists() {
        return Err("build first with cargo build --release --bins".into());
    }
    let mut cfg = load_config(&repo.join("router.toml"))?;
    cfg.patch.enabled = true;
    cfg.patch.model = manifest.patch_model.clone();
    cfg.patch.max_file_bytes = 100_000;
    cfg.fusion.lead = FusionRole {
        client: "grok".into(),
        model: manifest.baseline_model.clone(),
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
        eprintln!("benchmark: {}", task.id);
        let task_dir = run_dir.join(&task.id);
        fs::create_dir_all(&task_dir).map_err(|e| e.to_string())?;
        let baseline_dir = task_dir.join("baseline");
        let routed_dir = task_dir.join("routed");
        prepare(&repo, &baseline_dir, &manifest.source_ref, task)?;
        prepare(&repo, &routed_dir, &manifest.source_ref, task)?;
        for fixture in [&baseline_dir, &routed_dir] {
            if check(fixture, &["test", "-q", "--test", "benchmark_regression"])? {
                return Err(format!("{} fixture does not fail before repair", task.id));
            }
        }
        let start = Instant::now();
        let baseline_output = run(Command::new("grok")
            .args([
                "--single",
                &task.prompt,
                "--model",
                &manifest.baseline_model,
            ])
            .args(["--reasoning-effort", "high", "--output-format", "json"])
            .args(["--always-approve", "--cwd"])
            .arg(&baseline_dir))?;
        let baseline_duration = start.elapsed().as_millis();
        save_output(&task_dir.join("baseline-agent"), &baseline_output)?;
        let baseline_json = parse_json(&baseline_output)?;
        let baseline_acceptance = check(
            &baseline_dir,
            &["test", "-q", "--test", "benchmark_regression"],
        )?;
        let baseline_suite = check(&baseline_dir, &["test", "-q"])?;
        let baseline = Arm {
            cost_usd: baseline_json["total_cost_usd"].as_f64(),
            input_tokens: baseline_json["usage"]["input_tokens"].as_u64(),
            output_tokens: baseline_json["usage"]["output_tokens"].as_u64(),
            duration_ms: baseline_duration,
            agent_exit_success: baseline_output.status.success(),
            acceptance_success: baseline_acceptance,
            full_suite_success: baseline_suite,
            mode: "grok-single-agent".into(),
        };
        let start = Instant::now();
        let routed_output = run(Command::new(&router_bin)
            .arg("--config")
            .arg(&config_path)
            .args(["adaptive", "--task", &task.prompt, "--workdir"])
            .arg(&routed_dir)
            .args(["--file", &task.file]))?;
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
                baseline_model: &manifest.baseline_model,
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
