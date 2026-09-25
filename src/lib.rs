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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct Config {
    pub policy_version: u32,
    pub default: Tier,
    pub models: HashMap<String, Vec<Model>>,
    #[serde(default)]
    pub jev: JevConfig,
    #[serde(default)]
    pub paw: PawConfig,
    #[serde(default)]
    pub decision_service: DecisionServiceConfig,
    #[serde(default)]
    pub openrouter: OpenRouterConfig,
    #[serde(default)]
    pub fusion: FusionConfig,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FusionRole {
    pub client: String,
    pub model: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FusionConfig {
    pub lead: FusionRole,
    pub sidekick: FusionRole,
    #[serde(default = "default_fusion_corrections")]
    pub max_corrections: u8,
    #[serde(default = "default_fusion_chars")]
    pub max_handoff_chars: usize,
    #[serde(default = "default_fusion_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub validation: Vec<FusionValidation>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FusionValidation {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}
fn default_fusion_corrections() -> u8 {
    1
}
fn default_fusion_chars() -> usize {
    6000
}
fn default_fusion_timeout() -> u64 {
    900
}
impl Default for FusionConfig {
    fn default() -> Self {
        Self {
            lead: FusionRole {
                client: "codex".into(),
                model: "gpt-6-astra".into(),
            },
            sidekick: FusionRole {
                client: "codex".into(),
                model: "gpt-5.6-sol".into(),
            },
            max_corrections: default_fusion_corrections(),
            max_handoff_chars: default_fusion_chars(),
            timeout_secs: default_fusion_timeout(),
            validation: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionServiceConfig {
    #[serde(default = "default_decision_service_name")]
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_decision_service_url")]
    pub base_url: String,
    #[serde(default = "default_decision_service_model")]
    pub model: String,
    #[serde(default = "default_decision_service_path")]
    pub path: String,
    #[serde(default = "default_decision_service_margin")]
    pub min_margin: f64,
}
fn default_decision_service_name() -> String {
    "clm".into()
}
fn default_decision_service_url() -> String {
    "http://127.0.0.1:8700".into()
}
fn default_decision_service_model() -> String {
    "clm-latest".into()
}
fn default_decision_service_path() -> String {
    "/v1/systemone".into()
}
fn default_decision_service_margin() -> f64 {
    0.2
}
impl Default for DecisionServiceConfig {
    fn default() -> Self {
        Self {
            name: default_decision_service_name(),
            enabled: false,
            base_url: default_decision_service_url(),
            model: default_decision_service_model(),
            path: default_decision_service_path(),
            min_margin: default_decision_service_margin(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterConfig {
    #[serde(default = "default_openrouter_url")]
    pub base_url: String,
    #[serde(default = "default_openrouter_key_env")]
    pub api_key_env: String,
}
fn default_openrouter_url() -> String {
    "https://openrouter.ai/api/v1".into()
}
fn default_openrouter_key_env() -> String {
    "OPENROUTER_API_KEY".into()
}
impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            base_url: default_openrouter_url(),
            api_key_env: default_openrouter_key_env(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OpenRouterResult {
    pub model: String,
    pub content: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}
fn checked_api_url(base: &str) -> Result<String, String> {
    let url = url::Url::parse(base).map_err(|e| e.to_string())?;
    let loopback = matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
    );
    if (!loopback && url.scheme() != "https") || !["http", "https"].contains(&url.scheme()) {
        return Err("remote API requires HTTPS; localhost may use HTTP".into());
    }
    Ok(base.trim_end_matches('/').to_string())
}
pub fn openrouter_chat(
    cfg: &OpenRouterConfig,
    model: &str,
    task: &str,
    key: &str,
) -> Result<OpenRouterResult, String> {
    if key.is_empty() {
        return Err("OpenRouter API key is empty".into());
    }
    let base = checked_api_url(&cfg.base_url)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(120)))
        .build()
        .into();
    let body = serde_json::json!({"model":model,"messages":[{"role":"user","content":task}],"stream":false});
    let response: serde_json::Value = agent
        .post(&format!("{base}/chat/completions"))
        .header("Authorization", &format!("Bearer {key}"))
        .header("HTTP-Referer", "https://alphaonedev.github.io/ai-router/")
        .header("X-OpenRouter-Title", "ai-router")
        .send_json(&body)
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("OpenRouter response has no text content")?
        .to_string();
    Ok(OpenRouterResult {
        model: response["model"].as_str().unwrap_or(model).to_string(),
        content,
        prompt_tokens: response["usage"]["prompt_tokens"].as_u64(),
        completion_tokens: response["usage"]["completion_tokens"].as_u64(),
    })
}
pub fn openrouter_models(cfg: &OpenRouterConfig) -> Result<Vec<String>, String> {
    let base = checked_api_url(&cfg.base_url)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .build()
        .into();
    let response: serde_json::Value = agent
        .get(&format!("{base}/models"))
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let data = response["data"]
        .as_array()
        .ok_or("OpenRouter model list missing")?;
    Ok(data
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect())
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PawConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub local_dir: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
    toml::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
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
    h.update(format!(
        "|{}|{}|{}|{}|{}|{}|{}|{}",
        cfg.decision_service.name,
        cfg.decision_service.enabled,
        cfg.decision_service.base_url,
        cfg.decision_service.model,
        cfg.decision_service.path,
        cfg.decision_service.min_margin,
        cfg.openrouter.base_url,
        cfg.openrouter.api_key_env
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
fn system_one_choice(req: &Request, cfg: &DecisionServiceConfig) -> Result<Option<Tier>, String> {
    if !(0.0..=1.0).contains(&cfg.min_margin) {
        return Err("decision service min_margin must be in [0,1]".into());
    }
    let url = url::Url::parse(&cfg.base_url).map_err(|e| e.to_string())?;
    let loopback = matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "::1" | "[::1]")
    );
    if req.offline && !loopback {
        return Err("offline request cannot use remote decision service".into());
    }
    let base = checked_api_url(&cfg.base_url)?;
    if !cfg.path.starts_with('/') || cfg.path.starts_with("//") {
        return Err("decision service path must start with one slash".into());
    }
    let endpoint = format!("{base}{}", cfg.path);
    let mut body = serde_json::json!({
        "state": req.task,
        "questions": { "route": {
            "type": "choice",
            "instructions": "Choose the least costly model tier that can complete this software task reliably. Abstain if unclear.",
            "criteria": {
                "fast": "Bounded low-risk formatting, extraction, summary, or explanation",
                "balanced": "Normal implementation, debugging, testing, and planning",
                "deep": "Security, production incidents, architecture, migration, complex debugging, or consequential work",
                "abstain": "Insufficient or conflicting task information"
            }
        }}
    });
    if !cfg.model.is_empty() {
        body["model"] = serde_json::Value::String(cfg.model.clone());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .build()
        .into();
    let response: serde_json::Value = agent
        .post(&endpoint)
        .send_json(&body)
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    let answer = &response["answers"]["route"];
    let margin = answer["confidence"]
        .as_f64()
        .ok_or("decision service confidence missing")?;
    if !margin.is_finite() || margin < cfg.min_margin {
        return Err("decision service margin below threshold".into());
    }
    match answer["choice"]
        .as_str()
        .ok_or("decision service choice missing")?
    {
        "fast" => Ok(Some(Tier::Fast)),
        "balanced" => Ok(Some(Tier::Balanced)),
        "deep" => Ok(Some(Tier::Deep)),
        "abstain" => Ok(None),
        _ => Err("decision service returned an unknown class".into()),
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
    let mut service_accepted = false;
    if cfg.decision_service.enabled && tier == Tier::Balanced && source != "paw" {
        match system_one_choice(req, &cfg.decision_service) {
            Ok(Some(t)) => {
                if let Some(m) = choose(models, t.max(floor)) {
                    selected = m;
                    source = "system_one".into();
                    reason = format!("validated {} choice", cfg.decision_service.name);
                    service_accepted = true;
                }
            }
            Ok(None) => reason = "decision service abstained; local fallback".into(),
            Err(e) => reason = format!("decision service unavailable; local fallback: {e}"),
        }
    }
    if cfg.jev.enabled
        && !req.offline
        && tier == Tier::Balanced
        && models.len() >= 2
        && !service_accepted
        && source != "paw"
    {
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
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    fn mock_json(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{addr}")
    }
    #[test]
    fn local_system_one_choice_is_validated_by_policy() {
        let mut cfg: Config = toml::from_str(include_str!("../router.toml")).unwrap();
        cfg.decision_service.enabled = true;
        cfg.decision_service.base_url =
            mock_json(r#"{"answers":{"route":{"choice":"fast","confidence":0.8}}}"#);
        let req = Request {
            client: "claude".into(),
            task: "Implement a normal feature".into(),
            model: None,
            min_tier: Some(Tier::Balanced),
            high_stakes: false,
            offline: true,
        };
        let decision = route(&req, &cfg, None).unwrap();
        assert_eq!(decision.source, "system_one");
        assert_eq!(decision.tier, Tier::Balanced);
    }
    #[test]
    fn openrouter_chat_uses_selected_model_and_usage() {
        let cfg = OpenRouterConfig {
            base_url: mock_json(
                r#"{"model":"test/model","choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":4,"completion_tokens":1}}"#,
            ),
            api_key_env: "TEST_KEY".into(),
        };
        let result = openrouter_chat(&cfg, "test/model", "hello", "key").unwrap();
        assert_eq!(result.content, "ok");
        assert_eq!(result.prompt_tokens, Some(4));
    }
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
        let cfg: Config = toml::from_str(include_str!("../router.toml")).unwrap();
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
        let cfg: Config = toml::from_str(include_str!("../router.toml")).unwrap();
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
