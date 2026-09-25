use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Model {
    pub id: String,
    pub tier: Tier,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub description: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Fast,
    Balanced,
    Deep,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub policy_version: u32,
    pub default: Tier,
    pub models: HashMap<String, Vec<Model>>,
    #[serde(default)]
    pub jev: JevConfig,
    #[serde(default)]
    pub paw: PawConfig,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PawConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub local_dir: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct JevConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
}
fn default_min_confidence() -> f64 {
    0.75
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    pub client: String,
    pub task: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub min_tier: Option<Tier>,
    #[serde(default)]
    pub high_stakes: bool,
    #[serde(default)]
    pub offline: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Decision {
    pub model: String,
    pub tier: Tier,
    pub effort: Option<String>,
    pub source: String,
    pub reason: String,
    pub duration_ms: u128,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Measurement {
    pub baseline_compute: f64,
    pub routed_compute: f64,
    pub baseline_success: bool,
    pub routed_success: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SavingsReport {
    pub tasks: usize,
    pub baseline_compute: f64,
    pub routed_compute: f64,
    pub regain_fraction: f64,
    pub baseline_failures: usize,
    pub routed_failures: usize,
    pub target_met: bool,
}
pub fn savings(records: &[Measurement]) -> Result<SavingsReport, String> {
    if records.is_empty() {
        return Err("measurement file is empty".into());
    }
    if records.iter().any(|r| {
        !r.baseline_compute.is_finite()
            || !r.routed_compute.is_finite()
            || r.baseline_compute < 0.0
            || r.routed_compute < 0.0
    }) {
        return Err("compute values must be finite and nonnegative".into());
    }
    let baseline_compute: f64 = records.iter().map(|r| r.baseline_compute).sum();
    if baseline_compute <= 0.0 {
        return Err("baseline compute must be positive".into());
    }
    let routed_compute: f64 = records.iter().map(|r| r.routed_compute).sum();
    let baseline_failures = records.iter().filter(|r| !r.baseline_success).count();
    let routed_failures = records.iter().filter(|r| !r.routed_success).count();
    let regain_fraction = 1.0 - routed_compute / baseline_compute;
    Ok(SavingsReport {
        tasks: records.len(),
        baseline_compute,
        routed_compute,
        regain_fraction,
        baseline_failures,
        routed_failures,
        target_met: regain_fraction >= 0.5 && routed_failures <= baseline_failures,
    })
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheEntry {
    decision: Decision,
    expires_at: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RouterEvent {
    pub timestamp: u64,
    pub client: String,
    pub task_id: String,
    pub model: String,
    pub tier: Tier,
    pub source: String,
    pub duration_ms: u128,
}
pub fn read_events(cache_dir: &Path) -> Vec<RouterEvent> {
    let Ok(raw) = fs::read_to_string(cache_dir.join("events.jsonl")) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
fn record_event(req: &Request, cfg: &Config, dir: Option<&Path>, decision: &Decision) {
    let Some(dir) = dir else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join("events.jsonl");
    if fs::metadata(&path).is_ok_and(|m| m.len() > 5_000_000) {
        let _ = fs::rename(&path, dir.join("events.previous.jsonl"));
    }
    let event = RouterEvent {
        timestamp: now(),
        client: req.client.clone(),
        task_id: cache_key(req, cfg)[..10].into(),
        model: decision.model.clone(),
        tier: decision.tier,
        source: decision.source.clone(),
        duration_ms: decision.duration_ms,
    };
    if let (Ok(mut file), Ok(line)) = (
        fs::OpenOptions::new().create(true).append(true).open(path),
        serde_json::to_string(&event),
    ) {
        let _ = writeln!(file, "{line}");
    }
}

pub fn load_config(path: &Path) -> Result<Config, String> {
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
pub fn normalize(task: &str) -> String {
    task.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
pub fn cache_key(req: &Request, cfg: &Config) -> String {
    let mut h = Sha256::new();
    h.update(format!(
        "{}|{}|{}|{:?}|{}|{}|{:?}|{:?}|{}|{}|{}|{}",
        cfg.policy_version,
        req.client,
        normalize(&req.task),
        req.min_tier,
        req.high_stakes,
        req.offline,
        cfg.models.get(&req.client).map(|m| m
            .iter()
            .map(|x| (&x.id, x.tier, &x.effort, &x.description))
            .collect::<Vec<_>>()),
        cfg.jev.enabled,
        cfg.jev.min_confidence,
        cfg.paw.enabled,
        cfg.paw.slug,
        cfg.paw.local_dir
    ));
    hex::encode(h.finalize())
}
pub fn classify(task: &str) -> Tier {
    let t = normalize(task);
    let deep = [
        "security",
        "vulnerability",
        "production",
        "migration",
        "architecture",
        "refactor",
        "race condition",
        "payment",
        "legal",
        "medical",
        "financial",
        "deploy",
        "incident",
        "multi-file",
    ];
    let fast = [
        "summarize",
        "format",
        "rename",
        "typo",
        "explain",
        "list",
        "extract",
        "translate",
        "boilerplate",
        "comment",
    ];
    if deep.iter().any(|s| t.contains(s)) {
        Tier::Deep
    } else if fast.iter().any(|s| t.contains(s)) && t.len() < 400 {
        Tier::Fast
    } else {
        Tier::Balanced
    }
}
fn choose(models: &[Model], floor: Tier) -> Option<&Model> {
    models
        .iter()
        .filter(|m| m.tier >= floor)
        .min_by_key(|m| m.tier)
}
#[cfg(feature = "paw")]
fn paw_classify(config: &PawConfig, task: &str) -> Result<Option<Tier>, String> {
    use paw_rs::prelude::*;
    let result = if !config.local_dir.is_empty() {
        let mut classifier = paw_rs::paw_candle::PawFnLoader::new(&config.local_dir)
            .load()
            .map_err(|e| e.to_string())?;
        classifier
            .run(task, &paw_rs::paw_candle::PawRuntimeOptions::default())
            .map_err(|e| e.to_string())?
    } else {
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let mut classifier = runtime
            .block_on(PawFnBuilder::builder().slug(&config.slug).load())
            .map_err(|e| e.to_string())?;
        classifier.run(task).map_err(|e| e.to_string())?
    };
    match result.trim().to_ascii_lowercase().as_str() {
        "fast" => Ok(Some(Tier::Fast)),
        "balanced" => Ok(Some(Tier::Balanced)),
        "deep" => Ok(Some(Tier::Deep)),
        "abstain" => Ok(None),
        _ => Err("PAW returned an unknown class".into()),
    }
}
#[cfg(not(feature = "paw"))]
fn paw_classify(_config: &PawConfig, _task: &str) -> Result<Option<Tier>, String> {
    Err("PAW feature not compiled; build with --features paw".into())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn jev_choice(req: &Request, models: &[Model], min_confidence: f64) -> Result<String, String> {
    let key = std::env::var("JEV_API_KEY").map_err(|_| "JEV_API_KEY unset".to_string())?;
    let candidates: Vec<_> = models.iter().map(|m| serde_json::json!({"id":m.id,"description":m.description,"cost": match m.tier {Tier::Fast=>"low",Tier::Balanced=>"medium",Tier::Deep=>"high"}})).collect();
    let body = serde_json::json!({"task":req.task,"candidates":candidates,"priorities":["quality","cost","latency"],"stakes":if req.high_stakes {"high"} else {"normal"}});
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .build()
        .into();
    let response: serde_json::Value = agent
        .post("https://www.jevai.org/api/v1/decisions/model-route")
        .header("Authorization", &format!("Bearer {key}"))
        .send_json(&body)
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    if response["code"].as_i64() != Some(0) {
        return Err("Jev returned error".into());
    }
    let data = &response["data"];
    let confidence = data["confidence"]
        .as_f64()
        .ok_or("Jev confidence missing")?;
    if confidence < min_confidence {
        return Err("Jev confidence below threshold".into());
    }
    let id = data["decision"].as_str().ok_or("Jev decision missing")?;
    if models.iter().any(|m| m.id == id) {
        Ok(id.to_string())
    } else {
        Err("Jev chose unavailable model".into())
    }
}
pub fn route(req: &Request, cfg: &Config, cache_dir: Option<&Path>) -> Result<Decision, String> {
    let start = std::time::Instant::now();
    let models = cfg.models.get(&req.client).ok_or("unsupported client")?;
    if models.is_empty() {
        return Err("no models configured".into());
    }
    if models.iter().any(|m| m.id.trim().is_empty()) {
        return Err("model ID cannot be empty".into());
    }
    let floor = if req.high_stakes {
        Tier::Deep
    } else {
        req.min_tier.unwrap_or(Tier::Fast)
    };
    if let Some(id) = &req.model {
        let m = models
            .iter()
            .find(|m| &m.id == id)
            .ok_or("explicit model unavailable")?;
        let decision = Decision {
            model: m.id.clone(),
            tier: m.tier,
            effort: m.effort.clone(),
            source: "explicit".into(),
            reason: "caller selected model".into(),
            duration_ms: start.elapsed().as_millis(),
        };
        record_event(req, cfg, cache_dir, &decision);
        return Ok(decision);
    }
    let key = cache_key(req, cfg);
    if let Some(dir) = cache_dir {
        if let Ok(bytes) = fs::read(dir.join(&key)) {
            if let Ok(entry) = serde_json::from_slice::<CacheEntry>(&bytes) {
                if entry.expires_at > now()
                    && models.iter().any(|m| {
                        m.id == entry.decision.model
                            && m.tier == entry.decision.tier
                            && m.effort == entry.decision.effort
                            && m.tier >= floor
                    })
                {
                    let mut d = entry.decision;
                    d.source = "cache".into();
                    d.duration_ms = start.elapsed().as_millis();
                    record_event(req, cfg, cache_dir, &d);
                    return Ok(d);
                }
            }
        }
    }
    if cfg.default == Tier::Fast {
        return Err("default tier must be balanced or deep".into());
    }
    let local = classify(&req.task);
    let mut tier = if local == Tier::Balanced {
        cfg.default.max(floor)
    } else {
        local.max(floor)
    };
    let mut paw_reason = None;
    if cfg.paw.enabled
        && (!req.offline || !cfg.paw.local_dir.is_empty())
        && (!cfg.paw.slug.is_empty() || !cfg.paw.local_dir.is_empty())
        && tier == Tier::Balanced
    {
        match paw_classify(&cfg.paw, &req.task) {
            Ok(Some(t)) => {
                tier = t.max(floor);
                paw_reason = Some("PAW classification".to_string());
            }
            Ok(None) => paw_reason = Some("PAW abstained; local fallback".to_string()),
            Err(e) => paw_reason = Some(format!("PAW unavailable; local fallback: {e}")),
        }
    }
    let mut selected = choose(models, tier).ok_or("no model")?;
    let mut source = if paw_reason.as_deref() == Some("PAW classification") {
        "paw"
    } else {
        "rules"
    }
    .to_string();
    let mut reason = paw_reason.unwrap_or_else(|| format!("local classification: {tier:?}"));
    if cfg.jev.enabled && !req.offline && tier == Tier::Balanced && models.len() >= 2 {
        match jev_choice(req, models, cfg.jev.min_confidence) {
            Ok(id) => {
                if let Some(m) = models.iter().find(|m| m.id == id && m.tier >= floor) {
                    selected = m;
                    source = "jev".into();
                    reason = "validated hosted decision".into();
                }
            }
            Err(e) => {
                reason = format!("local fallback: {e}");
            }
        }
    }
    let decision = Decision {
        model: selected.id.clone(),
        tier: selected.tier,
        effort: selected.effort.clone(),
        source,
        reason,
        duration_ms: start.elapsed().as_millis(),
    };
    if let Some(dir) = cache_dir {
        let _ = fs::create_dir_all(dir);
        if let Ok(bytes) = serde_json::to_vec(&CacheEntry {
            decision: decision.clone(),
            expires_at: now() + 86400,
        }) {
            let _ = fs::write(dir.join(key), bytes);
        }
    }
    record_event(req, cfg, cache_dir, &decision);
    Ok(decision)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn savings_requires_quality_parity() {
        let r = savings(&[Measurement {
            baseline_compute: 100.0,
            routed_compute: 40.0,
            baseline_success: true,
            routed_success: false,
        }])
        .unwrap();
        assert!(!r.target_met);
        assert!((r.regain_fraction - 0.6).abs() < 1e-9);
    }
    #[test]
    fn precedence_and_risk() {
        let cfg: Config = serde_json::from_str(include_str!("../router.json")).unwrap();
        let mut r = Request {
            client: "codex".into(),
            task: "format this".into(),
            model: None,
            min_tier: None,
            high_stakes: false,
            offline: true,
        };
        assert_eq!(route(&r, &cfg, None).unwrap().tier, Tier::Fast);
        r.high_stakes = true;
        assert_eq!(route(&r, &cfg, None).unwrap().tier, Tier::Deep);
        r.model = Some("gpt-6-luna".into());
        assert_eq!(route(&r, &cfg, None).unwrap().source, "explicit");
    }
    #[test]
    fn cache_roundtrip() {
        let cfg: Config = serde_json::from_str(include_str!("../router.json")).unwrap();
        let r = Request {
            client: "claude".into(),
            task: "explain this".into(),
            model: None,
            min_tier: None,
            high_stakes: false,
            offline: true,
        };
        let d = tempfile::tempdir().unwrap();
        assert_eq!(route(&r, &cfg, Some(d.path())).unwrap().source, "rules");
        assert_eq!(route(&r, &cfg, Some(d.path())).unwrap().source, "cache");
    }
}
