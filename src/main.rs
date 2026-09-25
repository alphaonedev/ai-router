use ai_router::{load_config, route, Request, Tier};
use clap::{Parser, Subcommand};
use std::{
    io::{self, Read},
    path::PathBuf,
    process::Command,
};
#[derive(Parser)]
#[command(
    name = "ai-router",
    version,
    about = "Local-first model routing for coding CLIs"
)]
struct Cli {
    #[arg(long, global = true, default_value = "router.json")]
    config: PathBuf,
    #[arg(long, global = true)]
    cache_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
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
            let mut cmd = Command::new(client);
            cmd.arg("--model").arg(&d.model);
            if client == "claude" {
                if let Some(e) = &d.effort {
                    cmd.arg("--effort").arg(e);
                }
            }
            if client == "claude" {
                cmd.arg("--print").arg(task);
            } else if client == "codex" {
                cmd.arg("exec").arg(task);
            } else if client == "grok" {
                cmd.arg("--single").arg(task);
            } else {
                return Err("unsupported client".into());
            }
            cmd.args(extra);
            eprintln!("ai-router: {} ({})", d.model, d.source);
            let status = cmd.status().map_err(|e| e.to_string())?;
            std::process::exit(status.code().unwrap_or(1))
        }
    }
}
