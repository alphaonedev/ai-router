use ai_router::{openrouter_chat, Config};
use serde::Serialize;
use std::{
    fs,
    path::{Component, Path, PathBuf},
    time::Instant,
};

#[derive(Clone, Serialize)]
pub struct PatchAttempt {
    pub model: String,
    pub file: String,
    pub applied: bool,
    pub reason: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub duration_ms: u128,
}

pub struct AppliedPatch {
    path: PathBuf,
    original: String,
    replacement: String,
}
impl AppliedPatch {
    pub fn rollback(self) -> Result<(), String> {
        let current = fs::read_to_string(&self.path).map_err(|e| e.to_string())?;
        if current != self.replacement {
            return Err("patched file changed during validation; refusing rollback".into());
        }
        write_atomic(&self.path, &self.original)
    }
}

fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("patch path has no parent")?;
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("patch target must remain a regular file".into());
    }
    let permissions = metadata.permissions();
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    use std::io::Write;
    tmp.write_all(content.as_bytes())
        .map_err(|e| e.to_string())?;
    tmp.as_file()
        .set_permissions(permissions)
        .map_err(|e| e.to_string())?;
    tmp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

fn write_if_unchanged(path: &Path, original: &str, replacement: &str) -> Result<(), String> {
    if fs::read_to_string(path).map_err(|e| e.to_string())? != original {
        return Err("patch target changed while awaiting model; refusing to overwrite".into());
    }
    write_atomic(path, replacement)
}

fn parse_edit(content: &str, original: &str) -> Result<String, String> {
    let trimmed = content.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let json = json.strip_suffix("```").unwrap_or(json).trim();
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| "patch model returned invalid JSON")?;
    let old = value["old"]
        .as_str()
        .ok_or("patch JSON missing old string")?;
    let new = value["new"]
        .as_str()
        .ok_or("patch JSON missing new string")?;
    if old.is_empty() || old == new {
        return Err("patch must replace nonempty text with a different value".into());
    }
    if new.len() > original.len().saturating_mul(2).saturating_add(10_000) {
        return Err("patch replacement too large".into());
    }
    let first = original
        .find(old)
        .ok_or("patch old text must occur exactly once")?;
    let next = first
        + original[first..]
            .chars()
            .next()
            .ok_or("patch old text must occur exactly once")?
            .len_utf8();
    if original[next..].contains(old) {
        return Err("patch old text must occur exactly once".into());
    }
    Ok(original.replacen(old, new, 1))
}

pub fn attempt(
    cfg: &Config,
    model: &str,
    task: &str,
    workdir: &Path,
    file: &Path,
) -> Result<(PatchAttempt, Option<AppliedPatch>), String> {
    if !cfg.patch.enabled {
        return Err("patch backend disabled in TOML".into());
    }
    if cfg.patch.max_file_bytes < 100 || cfg.patch.max_file_bytes > 200_000 {
        return Err("patch max_file_bytes must be 100..200000".into());
    }
    if file.is_absolute()
        || file
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("patch file must be a relative path without traversal".into());
    }
    let root = fs::canonicalize(workdir).map_err(|e| e.to_string())?;
    let path = fs::canonicalize(root.join(file)).map_err(|e| e.to_string())?;
    if !path.starts_with(&root) {
        return Err("patch file leaves workdir".into());
    }
    if !fs::metadata(&path).map_err(|e| e.to_string())?.is_file() {
        return Err("patch target is not a file".into());
    }
    let original = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if original.len() > cfg.patch.max_file_bytes {
        return Err("patch target exceeds max_file_bytes".into());
    }
    let allowlisted = cfg
        .models
        .get("openrouter")
        .is_some_and(|models| models.iter().any(|m| m.id == model));
    if !allowlisted {
        return Err("patch model is not in models.openrouter allowlist".into());
    }
    let key = std::env::var(&cfg.openrouter.api_key_env)
        .map_err(|_| format!("{} unset", cfg.openrouter.api_key_env))?;
    let start = Instant::now();
    let prompt = format!("You are making one minimal, exact text edit to a Rust project file. Return only a JSON object with string fields old and new. old must be a contiguous exact substring of the provided file and occur exactly once. new must be the replacement substring. Do not include markdown, explanations, unrelated changes, or an entire file unless necessary.\n\nTASK:\n{task}\n\nFILE: {}\n```\n{original}\n```", file.display());
    let response = match openrouter_chat(&cfg.openrouter, model, &prompt, &key) {
        Ok(r) => r,
        Err(e) => {
            return Ok((
                PatchAttempt {
                    model: model.into(),
                    file: file.display().to_string(),
                    applied: false,
                    reason: format!("OpenRouter request failed: {e}"),
                    prompt_tokens: None,
                    completion_tokens: None,
                    cost_usd: None,
                    duration_ms: start.elapsed().as_millis(),
                },
                None,
            ))
        }
    };
    let replacement = match parse_edit(&response.content, &original) {
        Ok(r) => r,
        Err(e) => {
            return Ok((
                PatchAttempt {
                    model: model.into(),
                    file: file.display().to_string(),
                    applied: false,
                    reason: e,
                    prompt_tokens: response.prompt_tokens,
                    completion_tokens: response.completion_tokens,
                    cost_usd: response.cost_usd,
                    duration_ms: start.elapsed().as_millis(),
                },
                None,
            ))
        }
    };
    write_if_unchanged(&path, &original, &replacement)?;
    Ok((
        PatchAttempt {
            model: model.into(),
            file: file.display().to_string(),
            applied: true,
            reason: "exact replacement applied".into(),
            prompt_tokens: response.prompt_tokens,
            completion_tokens: response.completion_tokens,
            cost_usd: response.cost_usd,
            duration_ms: start.elapsed().as_millis(),
        },
        Some(AppliedPatch {
            path,
            original,
            replacement,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_edit_rejects_ambiguous_or_empty_substrings() {
        assert!(parse_edit(r#"{"old":"x","new":"y"}"#, "x x").is_err());
        assert!(parse_edit(r#"{"old":"aa","new":"b"}"#, "aaa").is_err());
        assert!(parse_edit(r#"{"old":"","new":"y"}"#, "x").is_err());
        assert_eq!(
            parse_edit(r#"{"old":"one","new":"two"}"#, "one\n").unwrap(),
            "two\n"
        );
    }
    #[test]
    fn exact_edit_does_not_overwrite_concurrent_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.rs");
        fs::write(&path, "changed by another process").unwrap();
        assert!(write_if_unchanged(&path, "original", "model edit").is_err());
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "changed by another process"
        );
    }
    #[cfg(unix)]
    #[test]
    fn exact_edit_refuses_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.rs");
        let link = dir.path().join("link.rs");
        fs::write(&target, "original").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(write_if_unchanged(&link, "original", "replacement").is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "original");
    }
}
