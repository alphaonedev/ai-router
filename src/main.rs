use ai_router::{
    load_config, openrouter_chat, openrouter_models, route, savings, Decision, Measurement,
    Request, Tier,
};
use clap::{Parser, Subcommand};
use std::{
    io::{self, Read},
    path::PathBuf,
    process::Command,
};
mod fusion;
mod observability;
mod patch;
#[derive(Parser)]
#[command(
    name = "ai-router",
    version,
    about = "Local-first model routing for coding CLIs"
)]
struct Cli {
    #[arg(long, global = true, default_value = "router.toml")]
    config: PathBuf,
    #[arg(long, global = true)]
    cache_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Select one validated sidekick or the full Fusion workflow for a new task.
    Adaptive {
        #[arg(long)]
        task: String,
        #[arg(long, default_value = ".")]
        workdir: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        high_stakes: bool,
        #[arg(long)]
        offline: bool,
        #[arg(long)]
        min_tier: Option<String>,
        #[arg(long)]
        file: Vec<PathBuf>,
    },
    /// Run a persistent lead and sidekick workflow from TOML roles.
    Fusion {
        #[arg(long)]
        task: String,
        #[arg(long, default_value = ".")]
        workdir: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// Check configured OpenRouter model IDs against the live catalog.
    OpenrouterModels,
    /// Compile and download a PAW classifier program (requires --features paw and PAW credentials).
    PawCompile {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        slug: String,
    },
    /// Watch routing decisions in the terminal.
    Watch {
        #[arg(long, default_value_t = 2)]
        interval: u64,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        measurements: Option<PathBuf>,
    },
    /// Serve the local live dashboard on 127.0.0.1.
    Dashboard {
        #[arg(long, default_value_t = 8747)]
        port: u16,
        #[arg(long)]
        measurements: Option<PathBuf>,
    },
    RouteJson,
    Route {
        #[arg(long)]
        client: String,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        min_tier: Option<String>,
        #[arg(long)]
        high_stakes: bool,
        #[arg(long)]
        offline: bool,
    },
    Run {
        client: String,
        #[arg(long)]
        task: String,
        #[arg(long)]
        interactive: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        min_tier: Option<String>,
        #[arg(long)]
        high_stakes: bool,
        #[arg(long)]
        offline: bool,
        #[arg(last = true)]
        extra: Vec<String>,
    },
    Toon {
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    Evaluate {
        file: PathBuf,
    },
    Savings {
        file: PathBuf,
    },
}
#[derive(serde::Serialize)]
struct LaunchPlan {
    program: String,
    args: Vec<String>,
    decision: Decision,
}
fn launch_plan(
    client: &str,
    task: &str,
    interactive: bool,
    extra: &[String],
    decision: Decision,
) -> Result<LaunchPlan, String> {
    if !["claude", "codex", "grok"].contains(&client) {
        return Err("unsupported client".into());
    }
    if extra.iter().any(|arg| {
        ["--model", "-m", "--effort", "--reasoning-effort"].contains(&arg.as_str())
            || arg.starts_with("--model=")
    }) {
        return Err("model and effort flags must use ai-router options or router.toml".into());
    }
    let mut args = vec!["--model".to_string(), decision.model.clone()];
    if let Some(e) = &decision.effort {
        match client {
            "claude" => args.extend(["--effort".into(), e.clone()]),
            "grok" => args.extend(["--reasoning-effort".into(), e.clone()]),
            "codex" => args.extend(["-c".into(), format!("model_reasoning_effort=\"{e}\"")]),
            _ => unreachable!(),
        }
    }
    match client {
        "claude" => {
            if !interactive {
                args.push("--print".into());
            }
            args.push(task.into());
        }
        "codex" => {
            if !interactive {
                args.push("exec".into());
            }
            args.push(task.into());
        }
        "grok" => {
            if !interactive {
                args.push("--single".into());
            }
            args.push(task.into());
        }
        _ => unreachable!(),
    }
    args.extend(extra.iter().cloned());
    Ok(LaunchPlan {
        program: client.into(),
        args,
        decision,
    })
}
fn tier(s: Option<String>) -> Result<Option<Tier>, String> {
    s.map(|v| {
        serde_json::from_str(&format!("\"{v}\""))
            .map_err(|_| "tier must be fast, balanced or deep".into())
    })
    .transpose()
}
fn task(s: Option<String>) -> Result<String, String> {
    if let Some(s) = s {
        return Ok(s);
    }
    let mut x = String::new();
    io::stdin()
        .read_to_string(&mut x)
        .map_err(|e| e.to_string())?;
    if x.trim().is_empty() {
        Err("task required".into())
    } else {
        Ok(x)
    }
}
fn cache(cli: &Cli) -> Option<PathBuf> {
    cli.cache_dir
        .clone()
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(|x| PathBuf::from(x).join("ai-router")))
        .or_else(|| std::env::var_os("HOME").map(|x| PathBuf::from(x).join(".cache/ai-router")))
}
fn main() -> Result<(), String> {
    let cli = Cli::parse();
    match &cli.command {
        Action::Adaptive {
            task,
            workdir,
            dry_run,
            high_stakes,
            offline,
            min_tier,
            file,
        } => {
            let cfg = load_config(&cli.config)?;
            let req = Request {
                client: cfg.fusion.lead.client.clone(),
                task: task.clone(),
                model: None,
                min_tier: tier(min_tier.clone())?,
                high_stakes: *high_stakes,
                offline: *offline,
            };
            let decision = route(&req, &cfg, cache(&cli).as_deref())?;
            if *dry_run {
                println!(
                    "{}",
                    serde_json::json!({"routing":decision,"execution":fusion::adaptive_plan(&cfg, workdir, decision.tier)?,"patch_files":file})
                );
            } else {
                let report = fusion::adaptive(
                    &cfg,
                    task,
                    workdir,
                    cache(&cli).as_deref(),
                    decision.tier,
                    file,
                    *offline,
                )?;
                println!(
                    "{}",
                    serde_json::to_string(&report).map_err(|e| e.to_string())?
                );
                if !["validated", "accepted"].contains(&report.outcome.as_str()) {
                    return Err(
                        "adaptive workflow ended without successful validation or lead acceptance"
                            .into(),
                    );
                }
            }
            Ok(())
        }
        Action::Fusion {
            task,
            workdir,
            dry_run,
        } => {
            let cfg = load_config(&cli.config)?;
            if *dry_run {
                println!("{}", fusion::dry_plan(&cfg, workdir)?);
            } else {
                let report = fusion::run(&cfg, task, workdir, cache(&cli).as_deref())?;
                println!(
                    "{}",
                    serde_json::to_string(&report).map_err(|e| e.to_string())?
                );
                if report.outcome != "accepted" {
                    return Err("fusion ended without lead acceptance".into());
                }
            }
            Ok(())
        }
        Action::OpenrouterModels => {
            let cfg = load_config(&cli.config)?;
            let catalog = openrouter_models(&cfg.openrouter)?;
            let configured = cfg
                .models
                .get("openrouter")
                .ok_or("no OpenRouter models configured")?;
            let result: Vec<_> = configured.iter().map(|m|serde_json::json!({"id":m.id,"tier":m.tier,"available":catalog.contains(&m.id)})).collect();
            println!(
                "{}",
                serde_json::to_string(&result).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Action::PawCompile { spec, slug } => {
            #[cfg(feature = "paw")]
            {
                use paw_rs::paw_core::{CompileRequest, PawClient, PawConfig};
                let instructions = std::fs::read_to_string(spec).map_err(|e| e.to_string())?;
                let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
                let config = PawConfig::from_env();
                let client = PawClient::new(&config);
                let request = CompileRequest::builder()
                    .spec(instructions)
                    .slug(slug)
                    .build()
                    .map_err(|e| e.to_string())?;
                let program = runtime
                    .block_on(client.compile(request))
                    .map_err(|e| e.to_string())?;
                let directory = runtime
                    .block_on(client.download_paw(&program.id))
                    .map_err(|e| e.to_string())?;
                println!(
                    "{}",
                    serde_json::json!({"id":program.id,"slug":program.slug,"local_dir":directory})
                );
                Ok(())
            }
            #[cfg(not(feature = "paw"))]
            {
                let _ = (spec, slug);
                Err("PAW feature not compiled; build with --features paw".into())
            }
        }
        Action::Watch {
            interval,
            once,
            measurements,
        } => {
            let dir = cache(&cli).ok_or("cache directory unavailable")?;
            observability::watch(&dir, measurements.as_deref(), *interval, *once);
            Ok(())
        }
        Action::Dashboard { port, measurements } => {
            let dir = cache(&cli).ok_or("cache directory unavailable")?;
            observability::serve(&dir, measurements.as_deref(), *port)
        }
        Action::RouteJson => {
            let cfg = load_config(&cli.config)?;
            let mut raw = String::new();
            io::stdin()
                .read_to_string(&mut raw)
                .map_err(|e| e.to_string())?;
            let req: Request = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let d = route(&req, &cfg, cache(&cli).as_deref())?;
            println!("{}", serde_json::to_string(&d).map_err(|e| e.to_string())?);
            Ok(())
        }
        Action::Savings { file } => {
            let raw = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
            let records = raw
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(serde_json::from_str::<Measurement>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string(&savings(&records)?).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Action::Toon { input, force } => {
            let raw = if let Some(p) = input {
                std::fs::read_to_string(p).map_err(|e| e.to_string())?
            } else {
                let mut s = String::new();
                io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| e.to_string())?;
                s
            };
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let encoded = toon_format::encode_default(&value).map_err(|e| e.to_string())?;
            let round: serde_json::Value =
                toon_format::decode_default(&encoded).map_err(|e| e.to_string())?;
            if round != value {
                return Err("TOON roundtrip mismatch".into());
            }
            if encoded.len() < raw.len() || *force {
                print!("{encoded}");
                eprintln!(
                    "bytes: {} -> {} ({:.1}% reduction; token count depends on model tokenizer)",
                    raw.len(),
                    encoded.len(),
                    100.0 * (raw.len() - encoded.len()) as f64 / raw.len() as f64
                )
            } else {
                print!("{raw}");
                eprintln!("TOON larger; kept JSON")
            };
            Ok(())
        }
        Action::Evaluate { file } => {
            let cfg = load_config(&cli.config)?;
            let raw = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
            let mut total = 0;
            let mut under = 0;
            let mut over = 0;
            for line in raw.lines().filter(|x| !x.trim().is_empty()) {
                let v: serde_json::Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
                let r: Request =
                    serde_json::from_value(v["request"].clone()).map_err(|e| e.to_string())?;
                let expected: Tier =
                    serde_json::from_value(v["minimum_tier"].clone()).map_err(|e| e.to_string())?;
                let d = route(&r, &cfg, None)?;
                total += 1;
                if d.tier < expected {
                    under += 1
                } else if d.tier > expected {
                    over += 1
                }
            }
            println!(
                "{}",
                serde_json::json!({"total":total,"under_routed":under,"over_routed":over,"under_routing_rate":if total==0{0.0}else{under as f64/total as f64}})
            );
            Ok(())
        }
        Action::Route {
            client,
            task: t,
            model,
            min_tier,
            high_stakes,
            offline,
        } => {
            let cfg = load_config(&cli.config)?;
            let req = Request {
                client: client.clone(),
                task: task(t.clone())?,
                model: model.clone(),
                min_tier: tier(min_tier.clone())?,
                high_stakes: *high_stakes,
                offline: *offline,
            };
            let d = route(&req, &cfg, cache(&cli).as_deref())?;
            println!("{}", serde_json::to_string(&d).map_err(|e| e.to_string())?);
            Ok(())
        }
        Action::Run {
            client,
            task,
            interactive,
            dry_run,
            model,
            min_tier,
            high_stakes,
            offline,
            extra,
        } => {
            let cfg = load_config(&cli.config)?;
            let req = Request {
                client: client.clone(),
                task: task.clone(),
                model: model.clone(),
                min_tier: tier(min_tier.clone())?,
                high_stakes: *high_stakes,
                offline: *offline,
            };
            let d = route(&req, &cfg, cache(&cli).as_deref())?;
            if client == "openrouter" {
                if *interactive || !extra.is_empty() {
                    return Err("OpenRouter API supports noninteractive single tasks here; no CLI pass-through arguments".into());
                }
                if *dry_run {
                    println!(
                        "{}",
                        serde_json::json!({"provider":"openrouter","model":d.model,"decision":d})
                    );
                    return Ok(());
                }
                if *offline {
                    return Err("offline mode cannot invoke OpenRouter API".into());
                }
                let key = std::env::var(&cfg.openrouter.api_key_env)
                    .map_err(|_| format!("{} unset", cfg.openrouter.api_key_env))?;
                let result = openrouter_chat(&cfg.openrouter, &d.model, task, &key)?;
                println!("{}", result.content);
                eprintln!(
                    "ai-router: {} ({}); input tokens {:?}, output tokens {:?}",
                    result.model, d.source, result.prompt_tokens, result.completion_tokens
                );
                return Ok(());
            }
            let plan = launch_plan(client, task, *interactive, extra, d)?;
            if *dry_run {
                println!(
                    "{}",
                    serde_json::to_string(&plan).map_err(|e| e.to_string())?
                );
                return Ok(());
            }
            let mut cmd = Command::new(&plan.program);
            cmd.args(&plan.args);
            eprintln!(
                "ai-router: {} ({})",
                plan.decision.model, plan.decision.source
            );
            let status = cmd.status().map_err(|e| e.to_string())?;
            std::process::exit(status.code().unwrap_or(1))
        }
    }
}
