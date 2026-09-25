use crate::patch::{self, PatchAttempt};
use ai_router::{Config, FusionRole, Model, Tier};
use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Serialize)]
pub struct Phase {
    pub role: String,
    pub client: String,
    pub model: String,
    pub duration_ms: u128,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub session_id: String,
}

#[derive(Serialize)]
pub struct Report {
    pub run_id: String,
    pub outcome: String,
    pub lead: String,
    pub sidekick: String,
    pub phases: Vec<Phase>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    pub total_cost_usd: Option<f64>,
    pub duration_ms: u128,
    pub review: String,
    pub validation: Vec<ValidationResult>,
}
#[derive(Serialize)]
pub struct AdaptiveReport {
    pub run_id: String,
    pub mode: String,
    pub outcome: String,
    pub classification_tier: Tier,
    pub phases: Vec<Phase>,
    pub validation: Vec<ValidationResult>,
    pub fusion: Option<Report>,
    pub patch: Option<PatchAttempt>,
    pub patch_attempts: Vec<PatchAttempt>,
    pub patch_validation: Vec<ValidationResult>,
    pub total_cost_usd: Option<f64>,
    pub duration_ms: u128,
}
#[derive(Serialize)]
pub struct ValidationResult {
    pub program: String,
    pub args: Vec<String>,
    pub success: bool,
    pub duration_ms: u128,
    pub output_tail: String,
}
#[derive(Serialize, serde::Deserialize)]
pub struct FusionEvent {
    pub timestamp: u64,
    pub run_id: String,
    pub stage: String,
    pub status: String,
    pub client: String,
    pub model: String,
    pub duration_ms: Option<u128>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}
fn event(
    cache: Option<&Path>,
    run_id: &str,
    stage: &str,
    status: &str,
    role: &FusionRole,
    phase: Option<&Phase>,
) {
    let Some(dir) = cache else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let item = FusionEvent {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        run_id: run_id.into(),
        stage: stage.into(),
        status: status.into(),
        client: role.client.clone(),
        model: role.model.clone(),
        duration_ms: phase.map(|p| p.duration_ms),
        input_tokens: phase.and_then(|p| p.input_tokens),
        output_tokens: phase.and_then(|p| p.output_tokens),
        cache_read_tokens: phase.and_then(|p| p.cache_read_tokens),
        cache_write_tokens: phase.and_then(|p| p.cache_write_tokens),
        cost_usd: phase.and_then(|p| p.cost_usd),
    };
    let path = dir.join("fusion-events.jsonl");
    if fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
        let _ = fs::rename(&path, dir.join("fusion-events.1.jsonl"));
    }
    if let (Ok(mut file), Ok(line)) = (
        OpenOptions::new().create(true).append(true).open(path),
        serde_json::to_string(&item),
    ) {
        let _ = writeln!(file, "{line}");
    }
}

struct ResultText {
    text: String,
    phase: Phase,
}
struct ParsedOutput {
    text: String,
    session_id: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    cache_write_tokens: Option<u64>,
    cost_usd: Option<f64>,
}
#[derive(Clone, Copy)]
struct PhaseRequest<'a> {
    label: &'a str,
    prompt: &'a str,
    session: &'a str,
    resume: bool,
    readonly: bool,
    cwd: &'a Path,
}
fn tracked_phase(
    cfg: &Config,
    role: &FusionRole,
    req: PhaseRequest<'_>,
    cache: Option<&Path>,
    run_id: &str,
) -> Result<ResultText, String> {
    event(cache, run_id, req.label, "running", role, None);
    match run_phase(cfg, role, req) {
        Ok(result) => {
            event(
                cache,
                run_id,
                &result.phase.role,
                "complete",
                role,
                Some(&result.phase),
            );
            Ok(result)
        }
        Err(error) => {
            event(cache, run_id, req.label, "failed", role, None);
            Err(error)
        }
    }
}

fn model<'a>(cfg: &'a Config, role: &FusionRole) -> Result<&'a Model, String> {
    if !["claude", "codex", "grok"].contains(&role.client.as_str()) {
        return Err(format!("unsupported fusion client: {}", role.client));
    }
    cfg.models
        .get(&role.client)
        .and_then(|ms| ms.iter().find(|m| m.id == role.model))
        .ok_or_else(|| {
            format!(
                "fusion model {} is not allowed for {}",
                role.model, role.client
            )
        })
}

fn command(
    role: &FusionRole,
    m: &Model,
    prompt: &str,
    session: &str,
    resume: bool,
    readonly: bool,
) -> Command {
    let mut cmd = Command::new(&role.client);
    match role.client.as_str() {
        "claude" => {
            cmd.args(["--model", &role.model, "--print", "--output-format", "json"]);
            if let Some(e) = &m.effort {
                cmd.args(["--effort", e]);
            }
            if resume {
                cmd.args(["--resume", session]);
            } else {
                cmd.args(["--session-id", session]);
            }
            cmd.args([
                "--permission-mode",
                if readonly { "plan" } else { "acceptEdits" },
                prompt,
            ]);
        }
        "codex" => {
            cmd.arg("exec");
            if resume {
                cmd.arg("resume");
            }
            if readonly {
                cmd.arg("--skip-git-repo-check");
            }
            cmd.args(["--json", "--model", &role.model]);
            if let Some(e) = &m.effort {
                cmd.args(["-c", &format!("model_reasoning_effort=\"{e}\"")]);
            }
            cmd.args([
                "-c",
                if readonly {
                    "sandbox_mode=\"read-only\""
                } else {
                    "sandbox_mode=\"workspace-write\""
                },
            ]);
            if resume {
                cmd.arg(session);
            }
            cmd.arg(prompt);
        }
        "grok" => {
            cmd.args(["--model", &role.model, "--output-format", "json"]);
            if let Some(e) = &m.effort {
                cmd.args(["--reasoning-effort", e]);
            }
            if readonly {
                cmd.args(["--sandbox", "read-only"]);
            }
            if resume {
                cmd.args(["--resume", session]);
            } else {
                cmd.args(["--session-id", session]);
            }
            cmd.args(["--permission-mode", "auto", "--single", prompt]);
        }
        _ => unreachable!(),
    }
    cmd
}

fn parse_output(
    client: &str,
    raw: &str,
    default_session: &str,
    resuming: bool,
) -> Result<ParsedOutput, String> {
    if client == "codex" {
        let mut session = None;
        let mut text = String::new();
        let mut input = None;
        let mut output = None;
        let mut cached = None;
        for line in raw.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value =
                serde_json::from_str(line).map_err(|e| format!("invalid Codex event: {e}"))?;
            if v["type"] == "thread.started" {
                if let Some(s) = v["thread_id"].as_str() {
                    if !s.trim().is_empty() {
                        session = Some(s.to_string());
                    }
                }
            }
            if v["type"] == "item.completed" && v["item"]["type"] == "agent_message" {
                if let Some(s) = v["item"]["text"].as_str() {
                    text = s.into();
                }
            }
            if v["type"] == "turn.completed" {
                input = v["usage"]["input_tokens"].as_u64();
                output = v["usage"]["output_tokens"].as_u64();
                cached = v["usage"]["cached_input_tokens"].as_u64();
            }
        }
        if text.trim().is_empty() {
            return Err("Codex returned no final message".into());
        }
        let session = session
            .or_else(|| resuming.then(|| default_session.to_string()))
            .ok_or("Codex did not return a resumable thread ID")?;
        return Ok(ParsedOutput {
            text,
            session_id: session,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cached,
            cache_write_tokens: None,
            cost_usd: None,
        });
    }
    let v: Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid {client} JSON result: {e}"))?;
    let text = v["result"]
        .as_str()
        .or_else(|| v["content"].as_str())
        .or_else(|| v["text"].as_str())
        .or_else(|| v["message"]["content"].as_str())
        .ok_or_else(|| format!("{client} returned no final text"))?
        .to_string();
    let session = v["session_id"]
        .as_str()
        .or_else(|| v["sessionId"].as_str())
        .unwrap_or(default_session)
        .to_string();
    let input = v["usage"]["input_tokens"]
        .as_u64()
        .or_else(|| v["usage"]["inputTokens"].as_u64());
    let output = v["usage"]["output_tokens"]
        .as_u64()
        .or_else(|| v["usage"]["outputTokens"].as_u64());
    let cost = v["total_cost_usd"]
        .as_f64()
        .or_else(|| v["cost_usd"].as_f64());
    let cache_read = v["usage"]["cache_read_input_tokens"].as_u64();
    let cache_write = v["usage"]["cache_creation_input_tokens"].as_u64();
    Ok(ParsedOutput {
        text,
        session_id: session,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        cost_usd: cost,
    })
}

fn run_phase(cfg: &Config, role: &FusionRole, req: PhaseRequest<'_>) -> Result<ResultText, String> {
    let m = model(cfg, role)?;
    let mut cmd = command(role, m, req.prompt, req.session, req.resume, req.readonly);
    let mut stdout = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut stderr = tempfile::tempfile().map_err(|e| e.to_string())?;
    cmd.current_dir(req.cwd)
        .stdout(Stdio::from(stdout.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(stderr.try_clone().map_err(|e| e.to_string())?));
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", role.client))?;
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break s;
        }
        if start.elapsed() > Duration::from_secs(cfg.fusion.timeout_secs) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{} timed out after {} seconds",
                req.label, cfg.fusion.timeout_secs
            ));
        }
        thread::sleep(Duration::from_millis(100));
    };
    let mut raw = String::new();
    stdout.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    stdout.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    if !status.success() {
        let mut err = String::new();
        stderr.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        stderr.read_to_string(&mut err).map_err(|e| e.to_string())?;
        return Err(format!(
            "{} failed ({}): {}",
            req.label,
            status,
            err.chars().take(1500).collect::<String>()
        ));
    }
    let parsed = parse_output(&role.client, &raw, req.session, req.resume)?;
    Ok(ResultText {
        text: parsed.text,
        phase: Phase {
            role: req.label.into(),
            client: role.client.clone(),
            model: role.model.clone(),
            duration_ms: start.elapsed().as_millis(),
            input_tokens: parsed.input_tokens,
            output_tokens: parsed.output_tokens,
            cache_read_tokens: parsed.cache_read_tokens,
            cache_write_tokens: parsed.cache_write_tokens,
            cost_usd: parsed.cost_usd,
            session_id: parsed.session_id,
        },
    })
}

fn bounded(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
fn git_output(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output.stdout)
}
fn snapshot(cwd: &Path, destination: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(destination).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(path).map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    let files = git_output(
        cwd,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let mut total = 0u64;
    for raw in files.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let name = std::str::from_utf8(raw)
            .map_err(|_| "non-UTF8 repository path unsupported in Fusion snapshot")?;
        let rel = Path::new(name);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("unsafe repository path".into());
        }
        if rel.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some(".git" | "target" | "node_modules" | "cache" | ".cache")
            )
        }) || rel
            .file_name()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x == ".env" || x.starts_with(".env."))
        {
            continue;
        }
        let source = cwd.join(rel);
        let meta = match std::fs::symlink_metadata(&source) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        if meta.file_type().is_symlink() {
            return Err(format!("Fusion snapshot does not support symlink: {name}"));
        }
        if !meta.is_file() {
            continue;
        }
        total = total.saturating_add(meta.len());
        if total > 256 * 1024 * 1024 {
            return Err("Fusion snapshot exceeds 256 MiB".into());
        }
        let target = destination.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::copy(source, target).map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn change_summary(cwd: &Path, max: usize) -> Result<String, String> {
    let status =
        String::from_utf8(git_output(cwd, &["status", "--short"])?).map_err(|e| e.to_string())?;
    let diff = String::from_utf8(git_output(
        cwd,
        &[
            "diff",
            "--no-ext-diff",
            "HEAD",
            "--",
            ".",
            ":(exclude).env",
            ":(exclude).env.*",
            ":(exclude)**/.env",
            ":(exclude)**/.env.*",
        ],
    )?)
    .map_err(|e| e.to_string())?;
    Ok(bounded(&format!("Git status:\n{status}\nTracked diff excerpt:\n{diff}\nIf truncated, inspect the copied files directly."), max))
}
fn review_decision(text: &str) -> Result<bool, String> {
    match (
        text.matches("DECISION: ACCEPT").count(),
        text.matches("DECISION: REVISE").count(),
    ) {
        (1, 0) => Ok(true),
        (0, 1) => Ok(false),
        _ => Err(
            "lead review must contain one unambiguous DECISION: ACCEPT or DECISION: REVISE marker"
                .into(),
        ),
    }
}
fn validate(cfg: &Config, cwd: &Path) -> Result<Vec<ValidationResult>, String> {
    let mut results = Vec::new();
    for check in &cfg.fusion.validation {
        if check.program.trim().is_empty() {
            return Err("fusion validation program is empty".into());
        }
        let mut output = tempfile::tempfile().map_err(|e| e.to_string())?;
        let error = output.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&check.program);
        cmd.args(&check.args)
            .current_dir(cwd)
            .stdout(Stdio::from(output.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(error));
        let start = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("validation {}: {e}", check.program))?;
        let status = loop {
            if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
                break s;
            }
            if start.elapsed() > Duration::from_secs(cfg.fusion.timeout_secs) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("validation {} timed out", check.program));
            }
            thread::sleep(Duration::from_millis(100));
        };
        let mut raw = String::new();
        output.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        output.read_to_string(&mut raw).map_err(|e| e.to_string())?;
        let tail: String = raw
            .chars()
            .rev()
            .take(3000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        results.push(ValidationResult {
            program: check.program.clone(),
            args: check.args.clone(),
            success: status.success(),
            duration_ms: start.elapsed().as_millis(),
            output_tail: tail,
        });
    }
    Ok(results)
}

pub fn dry_plan(cfg: &Config, cwd: &Path) -> Result<Value, String> {
    model(cfg, &cfg.fusion.lead)?;
    model(cfg, &cfg.fusion.sidekick)?;
    if !cwd.is_dir() {
        return Err("workdir is not a directory".into());
    }
    Ok(
        serde_json::json!({"workflow":"lead brief -> sidekick implementation -> validation -> lead review -> optional correction", "lead":cfg.fusion.lead,"sidekick":cfg.fusion.sidekick,"max_corrections":cfg.fusion.max_corrections,"max_handoff_chars":cfg.fusion.max_handoff_chars,"timeout_secs_per_phase":cfg.fusion.timeout_secs,"validation":cfg.fusion.validation,"workdir":cwd}),
    )
}

pub fn adaptive_plan(cfg: &Config, cwd: &Path, tier: Tier) -> Result<Value, String> {
    dry_plan(cfg, cwd)?;
    if !(1..=3).contains(&cfg.patch.max_attempts) {
        return Err("patch max_attempts must be 1..3".into());
    }
    let routine = cfg.fusion.routine.as_ref().unwrap_or(&cfg.fusion.sidekick);
    model(cfg, routine)?;
    if tier < cfg.fusion.min_tier && cfg.fusion.validation.is_empty() {
        return Err(
            "adaptive single-agent path requires at least one fusion.validation command".into(),
        );
    }
    let mode = if tier >= cfg.fusion.min_tier {
        "fusion"
    } else {
        "single_sidekick"
    };
    Ok(
        serde_json::json!({"mode":mode,"classification_tier":tier,"fusion_min_tier":cfg.fusion.min_tier,"lead":cfg.fusion.lead,"sidekick":cfg.fusion.sidekick,"routine":routine,"validation":cfg.fusion.validation,"workdir":cwd}),
    )
}

pub fn adaptive(
    cfg: &Config,
    task: &str,
    cwd: &Path,
    cache: Option<&Path>,
    tier: Tier,
    file: Option<&Path>,
    offline: bool,
) -> Result<AdaptiveReport, String> {
    adaptive_plan(cfg, cwd, tier)?;
    if task.trim().is_empty() {
        return Err("task required".into());
    }
    let start = Instant::now();
    let run_id = uuid::Uuid::new_v4().to_string();
    if tier >= cfg.fusion.min_tier {
        let report = run(cfg, task, cwd, cache)?;
        return Ok(AdaptiveReport {
            run_id,
            mode: "fusion".into(),
            outcome: report.outcome.clone(),
            classification_tier: tier,
            total_cost_usd: report.total_cost_usd,
            duration_ms: start.elapsed().as_millis(),
            phases: Vec::new(),
            validation: Vec::new(),
            fusion: Some(report),
            patch: None,
            patch_attempts: Vec::new(),
            patch_validation: Vec::new(),
        });
    }
    let mut patch_record = None;
    let mut patch_attempts = Vec::new();
    let mut patch_cost = Some(0.0);
    let mut patch_validation = Vec::new();
    if let Some(file) = file.filter(|_| {
        !offline && cfg.patch.enabled && std::env::var(&cfg.openrouter.api_key_env).is_ok()
    }) {
        let mut feedback = String::new();
        for index in 0..cfg.patch.max_attempts {
            eprintln!(
                "adaptive: bounded OpenRouter patch attempt {} for {}",
                index + 1,
                file.display()
            );
            let patch_task = if feedback.is_empty() {
                task.to_string()
            } else {
                format!("{task}\n\nThe previous exact-edit attempt failed. Correct it using this feedback:\n{feedback}")
            };
            match patch::attempt(cfg, &patch_task, cwd, file) {
                Ok((attempt, applied)) => {
                    patch_cost = patch_cost.zip(attempt.cost_usd).map(|(a, b)| a + b);
                    let patch_role = FusionRole {
                        client: "openrouter".into(),
                        model: attempt.model.clone(),
                    };
                    event(
                        cache,
                        &run_id,
                        "patch",
                        if attempt.applied {
                            "applied"
                        } else {
                            "rejected"
                        },
                        &patch_role,
                        None,
                    );
                    feedback = attempt.reason.clone();
                    patch_record = Some(attempt.clone());
                    patch_attempts.push(attempt);
                    if let Some(applied) = applied {
                        patch_validation = validate(cfg, cwd)?;
                        if patch_validation.iter().all(|v| v.success) {
                            event(cache, &run_id, "validation", "passed", &patch_role, None);
                            return Ok(AdaptiveReport {
                                run_id,
                                mode: "openrouter_patch".into(),
                                outcome: "validated".into(),
                                classification_tier: tier,
                                phases: Vec::new(),
                                validation: patch_validation,
                                patch_validation: Vec::new(),
                                fusion: None,
                                total_cost_usd: patch_cost,
                                duration_ms: start.elapsed().as_millis(),
                                patch: patch_record,
                                patch_attempts,
                            });
                        }
                        event(cache, &run_id, "validation", "failed", &patch_role, None);
                        applied.rollback()?;
                        feedback = patch_validation
                            .iter()
                            .filter(|v| !v.success)
                            .map(|v| v.output_tail.as_str())
                            .collect::<Vec<_>>()
                            .join("\n");
                        feedback = feedback
                            .chars()
                            .rev()
                            .take(3000)
                            .collect::<String>()
                            .chars()
                            .rev()
                            .collect();
                    }
                }
                Err(error) => {
                    eprintln!("adaptive: patch skipped: {error}");
                    break;
                }
            }
        }
    }
    let session = uuid::Uuid::new_v4().to_string();
    let prompt = format!("Implement this task in the current repository. Inspect files as needed, run relevant checks, and report the change and any unresolved issue. Do not commit or push.\n\nTask:\n{task}");
    let routine = cfg.fusion.routine.as_ref().unwrap_or(&cfg.fusion.sidekick);
    eprintln!(
        "adaptive: single sidekick {} / {}",
        routine.client, routine.model
    );
    let work = tracked_phase(
        cfg,
        routine,
        PhaseRequest {
            label: "single_sidekick",
            prompt: &prompt,
            session: &session,
            resume: false,
            readonly: false,
            cwd,
        },
        cache,
        &run_id,
    )?;
    let validation = validate(cfg, cwd)?;
    let passed = validation.iter().all(|v| v.success);
    let local_role = FusionRole {
        client: "local".into(),
        model: "validation".into(),
    };
    event(
        cache,
        &run_id,
        "validation",
        if passed { "passed" } else { "failed" },
        &local_role,
        None,
    );
    let cost = work.phase.cost_usd;
    let total_cost = if patch_attempts.is_empty() {
        cost
    } else {
        patch_cost.zip(cost).map(|(a, b)| a + b)
    };
    if passed {
        event(cache, &run_id, "outcome", "validated", routine, None);
        return Ok(AdaptiveReport {
            run_id,
            mode: "single_sidekick".into(),
            outcome: "validated".into(),
            classification_tier: tier,
            phases: vec![work.phase],
            validation,
            fusion: None,
            total_cost_usd: total_cost,
            duration_ms: start.elapsed().as_millis(),
            patch: patch_record,
            patch_attempts,
            patch_validation,
        });
    }
    eprintln!("adaptive: validation failed; escalating to Fusion");
    let report = run(cfg, task, cwd, cache)?;
    let total_cost = total_cost.zip(report.total_cost_usd).map(|(a, b)| a + b);
    Ok(AdaptiveReport {
        run_id,
        mode: "escalated".into(),
        outcome: report.outcome.clone(),
        classification_tier: tier,
        phases: vec![work.phase],
        validation,
        total_cost_usd: total_cost,
        duration_ms: start.elapsed().as_millis(),
        fusion: Some(report),
        patch: patch_record,
        patch_attempts,
        patch_validation,
    })
}

pub fn run(cfg: &Config, task: &str, cwd: &Path, cache: Option<&Path>) -> Result<Report, String> {
    dry_plan(cfg, cwd)?;
    if task.trim().is_empty() {
        return Err("task required".into());
    }
    if cfg.fusion.max_handoff_chars < 500 || cfg.fusion.max_handoff_chars > 100_000 {
        return Err("fusion max_handoff_chars must be 500..100000".into());
    }
    if cfg.fusion.timeout_secs == 0 {
        return Err("fusion timeout_secs must be positive".into());
    }
    let start = Instant::now();
    let lead_id = uuid::Uuid::new_v4().to_string();
    let run_id = uuid::Uuid::new_v4().to_string();
    let sidekick_id = uuid::Uuid::new_v4().to_string();
    let lead_workspace = tempfile::tempdir().map_err(|e| e.to_string())?;
    snapshot(cwd, lead_workspace.path())?;
    eprintln!(
        "fusion: lead planning with {} / {}",
        cfg.fusion.lead.client, cfg.fusion.lead.model
    );
    let plan_prompt = format!("You are the lead agent. You are in an isolated copy of the working tree. Inspect files here and prepare a concise implementation brief for another coding agent working in the original repository. Include objective, scope, constraints, files to inspect, acceptance tests, and risks. Do not edit files. Stay under {} characters. User task:\n{}", cfg.fusion.max_handoff_chars, task);
    let brief = tracked_phase(
        cfg,
        &cfg.fusion.lead,
        PhaseRequest {
            label: "lead_plan",
            prompt: &plan_prompt,
            session: &lead_id,
            resume: false,
            readonly: true,
            cwd: lead_workspace.path(),
        },
        cache,
        &run_id,
    )?;
    let mut phases = vec![brief.phase];
    let lead_session = phases[0].session_id.clone();
    let mut sidekick_session = sidekick_id;
    let mut feedback = String::new();
    let mut final_review = String::new();
    let mut outcome = "needs_review".to_string();
    let mut validation = Vec::new();
    for round in 0..=cfg.fusion.max_corrections {
        eprintln!(
            "fusion: sidekick execution round {} with {} / {}",
            round + 1,
            cfg.fusion.sidekick.client,
            cfg.fusion.sidekick.model
        );
        let work_prompt = if round == 0 {
            format!("Implement this task in the current workspace. Follow the lead's brief, inspect code as needed, run relevant tests, and report changes, tests, and unresolved issues. Do not commit or push. Keep the final report under {} characters.\n\nOriginal task:\n{}\n\nLead brief:\n{}", cfg.fusion.max_handoff_chars, task, bounded(&brief.text, cfg.fusion.max_handoff_chars))
        } else {
            format!("Address the lead review feedback in this same session. Run relevant tests and report changes and unresolved issues. Do not commit or push.\n\nFeedback:\n{}", feedback)
        };
        let work = tracked_phase(
            cfg,
            &cfg.fusion.sidekick,
            PhaseRequest {
                label: "sidekick_work",
                prompt: &work_prompt,
                session: &sidekick_session,
                resume: round > 0,
                readonly: false,
                cwd,
            },
            cache,
            &run_id,
        )?;
        sidekick_session = work.phase.session_id.clone();
        phases.push(work.phase);
        eprintln!(
            "fusion: running {} validation commands",
            cfg.fusion.validation.len()
        );
        let local_role = FusionRole {
            client: "local".into(),
            model: "validation".into(),
        };
        event(cache, &run_id, "validation", "running", &local_role, None);
        validation = validate(cfg, cwd)?;
        snapshot(cwd, lead_workspace.path())?;
        let changes = change_summary(cwd, cfg.fusion.max_handoff_chars)?;
        let validation_ok = validation.iter().all(|v| v.success);
        event(
            cache,
            &run_id,
            "validation",
            if validation_ok { "passed" } else { "failed" },
            &local_role,
            None,
        );
        let validation_text = serde_json::to_string(&validation).map_err(|e| e.to_string())?;
        eprintln!("fusion: lead reviewing round {}", round + 1);
        let review_prompt = format!("Review the sidekick's changes in this isolated copy of the working tree. The coordinator copied the latest files from the original repository and supplied git status and a diff excerpt below. You may inspect copied files here. The coordinator ran validation outside your sandbox; use its results and do not rerun checks that require localhost or writes. Assess the original task and your brief, not just the sidekick report. Include exactly one DECISION: ACCEPT or DECISION: REVISE marker, then evidence and concrete fixes if revising. If uncertain or validation fails, choose REVISE.\n\nOriginal task:\n{}\n\nSidekick report:\n{}\n\nCoordinator validation:\n{}\n\nChanges:\n{}", task, bounded(&work.text, cfg.fusion.max_handoff_chars), bounded(&validation_text, cfg.fusion.max_handoff_chars), changes);
        let review = tracked_phase(
            cfg,
            &cfg.fusion.lead,
            PhaseRequest {
                label: "lead_review",
                prompt: &review_prompt,
                session: &lead_session,
                resume: true,
                readonly: true,
                cwd: lead_workspace.path(),
            },
            cache,
            &run_id,
        )?;
        let decision = review_decision(&review.text)?;
        final_review = bounded(&review.text, cfg.fusion.max_handoff_chars);
        phases.push(review.phase);
        if decision && validation_ok {
            outcome = "accepted".into();
            break;
        }
        feedback = final_review.clone();
    }
    let sum = |f: fn(&Phase) -> Option<u64>| -> Option<u64> {
        phases
            .iter()
            .map(f)
            .collect::<Option<Vec<_>>>()
            .map(|v| v.into_iter().sum())
    };
    let costs: Option<Vec<f64>> = phases.iter().map(|p| p.cost_usd).collect();
    event(cache, &run_id, "outcome", &outcome, &cfg.fusion.lead, None);
    Ok(Report {
        run_id,
        outcome,
        lead: format!("{}/{}", cfg.fusion.lead.client, cfg.fusion.lead.model),
        sidekick: format!(
            "{}/{}",
            cfg.fusion.sidekick.client, cfg.fusion.sidekick.model
        ),
        total_input_tokens: sum(|p| p.input_tokens),
        total_output_tokens: sum(|p| p.output_tokens),
        total_cost_usd: costs.map(|v| v.into_iter().sum()),
        phases,
        duration_ms: start.elapsed().as_millis(),
        review: final_review,
        validation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_plan_reserves_fusion_for_deep_tasks() {
        let cfg: Config = toml::from_str(include_str!("../router.toml")).unwrap();
        let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            adaptive_plan(&cfg, cwd, Tier::Balanced).unwrap()["mode"],
            "single_sidekick"
        );
        assert_eq!(
            adaptive_plan(&cfg, cwd, Tier::Deep).unwrap()["mode"],
            "fusion"
        );
    }

    #[test]
    fn review_accepts_one_standalone_marker_after_preface() {
        assert!(review_decision("I checked the diff.\nDECISION: ACCEPT\nTests passed.").unwrap());
        assert!(review_decision("I checked the diff.DECISION: ACCEPT").unwrap());
        assert!(!review_decision("DECISION: REVISE\nFix the test.").unwrap());
        assert!(review_decision("DECISION: ACCEPT\nDECISION: REVISE").is_err());
    }

    #[test]
    fn lead_snapshot_isolated_and_refreshed() {
        let root = tempfile::tempdir().unwrap();
        let copy = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(root.path().join("README.md"), "original\n").unwrap();
        std::fs::write(root.path().join(".env"), "secret\n").unwrap();
        snapshot(root.path(), copy.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(copy.path().join("README.md")).unwrap(),
            "original\n"
        );
        assert!(!copy.path().join(".env").exists());
        std::fs::write(copy.path().join("README.md"), "lead edit\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path().join("README.md")).unwrap(),
            "original\n"
        );
        std::fs::write(root.path().join("README.md"), "sidekick edit\n").unwrap();
        snapshot(root.path(), copy.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(copy.path().join("README.md")).unwrap(),
            "sidekick edit\n"
        );
    }

    fn config() -> Config {
        toml::from_str(
            r#"
policy_version = 1
default = "balanced"

[fusion]

[fusion.lead]
client = "codex"
model = "lead-model"

[fusion.sidekick]
client = "claude"
model = "sidekick-model"

[[models.codex]]
id = "lead-model"
tier = "deep"

[[models.claude]]
id = "sidekick-model"
tier = "balanced"

[[models.openrouter]]
id = "configured-but-unsupported"
tier = "fast"
"#,
        )
        .expect("test configuration should parse")
    }

    fn parse_error(raw: &str) -> String {
        parse_output("codex", raw, "default-session", false)
            .err()
            .expect("fixture should fail")
    }

    #[test]
    fn parses_codex_jsonl_result() {
        let parsed = parse_output(
            "codex",
            r#"{"type":"thread.started","thread_id":"thread-123"}
{"type":"item.completed","item":{"type":"agent_message","text":"done"}}
{"type":"turn.completed","usage":{"input_tokens":17,"output_tokens":5}}"#,
            "default-session",
            false,
        )
        .expect("valid Codex output should parse");

        assert_eq!(parsed.session_id, "thread-123");
        assert_eq!(parsed.text, "done");
        assert_eq!(parsed.input_tokens, Some(17));
        assert_eq!(parsed.output_tokens, Some(5));
        assert_eq!(parsed.cost_usd, None);
    }

    #[test]
    fn ignores_blank_lines_and_unrelated_events_and_uses_last_message() {
        let parsed = parse_output(
            "codex",
            r#"
{"type":"thread.started","thread_id":"thread-123"}
{"type":"item.completed","item":{"type":"agent_message","text":"first"}}
{"type":"item.started","item":{"type":"command_execution"}}

{"type":"item.completed","item":{"type":"agent_message","text":"last"}}
"#,
            "default-session",
            false,
        )
        .expect("valid Codex output should parse");

        assert_eq!(parsed.text, "last");
        assert_eq!(parsed.input_tokens, None);
        assert_eq!(parsed.output_tokens, None);
    }

    #[test]
    fn reports_malformed_codex_event() {
        let error = parse_error("{not json}");
        assert!(error.starts_with("invalid Codex event:"), "{error}");
    }

    #[test]
    fn rejects_missing_or_blank_final_message() {
        for raw in [
            r#"{"type":"thread.started","thread_id":"thread-123"}"#,
            r#"{"type":"thread.started","thread_id":"thread-123"}
{"type":"item.completed","item":{"type":"agent_message","text":"  \n "}}"#,
        ] {
            assert_eq!(parse_error(raw), "Codex returned no final message");
        }
    }

    #[test]
    fn rejects_missing_or_blank_thread_id() {
        for raw in [
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
            r#"{"type":"thread.started","thread_id":"   "}
{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
        ] {
            assert_eq!(
                parse_error(raw),
                "Codex did not return a resumable thread ID"
            );
        }
    }

    #[test]
    fn accepts_explicit_resumed_thread_id() {
        let parsed = parse_output(
            "codex",
            r#"{"type":"thread.started","thread_id":"existing-thread"}
{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
            "existing-thread",
            true,
        )
        .expect("an explicitly returned resumed thread ID should be accepted");

        assert_eq!(parsed.session_id, "existing-thread");
    }

    #[test]
    fn resumed_codex_thread_may_omit_thread_started_event() {
        let parsed = parse_output(
            "codex",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"review done"}}"#,
            "existing-thread",
            true,
        )
        .unwrap();
        assert_eq!(parsed.session_id, "existing-thread");
        assert_eq!(parsed.text, "review done");
    }

    #[test]
    fn parses_grok_headless_json_fields() {
        let parsed = parse_output("grok", r#"{"text":"ROUTER_OK","sessionId":"grok-session","usage":{"input_tokens":25,"cache_read_input_tokens":5,"output_tokens":3},"total_cost_usd":0.01}"#, "fallback", false).unwrap();
        assert_eq!(parsed.text, "ROUTER_OK");
        assert_eq!(parsed.session_id, "grok-session");
        assert_eq!(parsed.cache_read_tokens, Some(5));
        assert_eq!(parsed.cost_usd, Some(0.01));
    }

    #[test]
    fn parses_claude_cache_usage_fields() {
        let parsed = parse_output(
            "claude",
            r#"{"result":"finished","session_id":"claude-session","usage":{"input_tokens":101,"output_tokens":23,"cache_read_input_tokens":47,"cache_creation_input_tokens":11},"total_cost_usd":0.125}"#,
            "fallback",
            false,
        )
        .expect("valid Claude output should parse");

        assert_eq!(parsed.text, "finished");
        assert_eq!(parsed.session_id, "claude-session");
        assert_eq!(parsed.input_tokens, Some(101));
        assert_eq!(parsed.output_tokens, Some(23));
        assert_eq!(parsed.cache_read_tokens, Some(47));
        assert_eq!(parsed.cache_write_tokens, Some(11));
        assert_eq!(parsed.cost_usd, Some(0.125));
    }

    #[test]
    fn preserves_absent_and_zero_claude_cache_usage() {
        let absent = parse_output(
            "claude",
            r#"{"result":"no cache fields","session_id":"claude-session","usage":{}}"#,
            "fallback",
            false,
        )
        .expect("Claude output without cache fields should parse");
        assert_eq!(absent.cache_read_tokens, None);
        assert_eq!(absent.cache_write_tokens, None);

        let zero = parse_output(
            "claude",
            r#"{"result":"zero cache counts","session_id":"claude-session","usage":{"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#,
            "fallback",
            false,
        )
        .expect("Claude output with zero cache counts should parse");
        assert_eq!(zero.cache_read_tokens, Some(0));
        assert_eq!(zero.cache_write_tokens, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn returns_successful_configured_validation_result() {
        let mut cfg = config();
        let args = vec!["-c".into(), "printf validation-marker".into()];
        cfg.fusion.validation = vec![ai_router::FusionValidation {
            program: "/bin/sh".into(),
            args: args.clone(),
        }];

        let results = validate(&cfg, Path::new(env!("CARGO_MANIFEST_DIR")))
            .expect("successful validation should return a result");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].program, "/bin/sh");
        assert_eq!(results[0].args, args);
        assert!(results[0].success);
        assert_eq!(results[0].output_tail, "validation-marker");
    }

    #[test]
    fn rejects_unsupported_fusion_client_even_when_configured() {
        let error = model(
            &config(),
            &FusionRole {
                client: "openrouter".into(),
                model: "configured-but-unsupported".into(),
            },
        )
        .expect_err("unsupported client should fail");

        assert_eq!(error, "unsupported fusion client: openrouter");
    }

    #[test]
    fn rejects_unallowlisted_fusion_models() {
        for role in [
            FusionRole {
                client: "grok".into(),
                model: "missing-inventory".into(),
            },
            FusionRole {
                client: "codex".into(),
                model: "sidekick-model".into(),
            },
        ] {
            let error = model(&config(), &role).expect_err("unallowlisted model should fail");
            assert_eq!(
                error,
                format!(
                    "fusion model {} is not allowed for {}",
                    role.model, role.client
                )
            );
        }
    }

    #[test]
    fn accepts_allowlisted_fusion_roles() {
        let cfg = config();
        assert_eq!(
            model(&cfg, &cfg.fusion.lead).expect("valid lead").id,
            "lead-model"
        );
        assert_eq!(
            model(&cfg, &cfg.fusion.sidekick)
                .expect("valid sidekick")
                .id,
            "sidekick-model"
        );
    }

    #[test]
    fn dry_plan_validates_lead_and_sidekick() {
        let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
        dry_plan(&config(), cwd).expect("valid roles should produce a plan");

        let mut invalid_lead = config();
        invalid_lead.fusion.lead.model = "missing-lead".into();
        assert_eq!(
            dry_plan(&invalid_lead, cwd).expect_err("invalid lead should fail"),
            "fusion model missing-lead is not allowed for codex"
        );

        let mut invalid_sidekick = config();
        invalid_sidekick.fusion.sidekick.model = "missing-sidekick".into();
        assert_eq!(
            dry_plan(&invalid_sidekick, cwd).expect_err("invalid sidekick should fail"),
            "fusion model missing-sidekick is not allowed for claude"
        );
    }
}
