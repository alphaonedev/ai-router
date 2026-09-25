use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
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
    pub default: String,
    pub models: HashMap<String, Vec<Model>>,
    #[serde(default)]
    pub jev: JevConfig,
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
struct CacheEntry {
    decision: Decision,
    expires_at: u64,
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
        "{}|{}|{}|{:?}|{}|{}|{:?}",
        cfg.policy_version,
        req.client,
        normalize(&req.task),
        req.min_tier,
        req.high_stakes,
        req.offline,
        cfg.models
            .get(&req.client)
            .map(|m| m.iter().map(|x| &x.id).collect::<Vec<_>>())
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
        .or_else(|| models.iter().max_by_key(|m| m.tier))
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
    let response: serde_json::Value =
        ureq::post("https://www.jevai.org/api/v1/decisions/model-route")
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
        return Ok(Decision {
            model: m.id.clone(),
            tier: m.tier,
            effort: m.effort.clone(),
            source: "explicit".into(),
            reason: "caller selected model".into(),
            duration_ms: start.elapsed().as_millis(),
        });
    }
    let key = cache_key(req, cfg);
    if let Some(dir) = cache_dir {
        if let Ok(bytes) = fs::read(dir.join(&key)) {
            if let Ok(entry) = serde_json::from_slice::<CacheEntry>(&bytes) {
                if entry.expires_at > now()
                    && models
                        .iter()
                        .any(|m| m.id == entry.decision.model && m.tier >= floor)
                {
                    let mut d = entry.decision;
                    d.source = "cache".into();
                    d.duration_ms = start.elapsed().as_millis();
                    return Ok(d);
                }
            }
        }
    }
    let tier = classify(&req.task).max(floor);
    let mut selected = choose(models, tier).ok_or("no model")?;
    let mut source = "rules".to_string();
    let mut reason = format!("local classification: {tier:?}");
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
    Ok(decision)
}
#[cfg(test)]
mod tests {
    use super::*;
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
        r.model = Some("gpt-5.1-codex-mini".into());
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
