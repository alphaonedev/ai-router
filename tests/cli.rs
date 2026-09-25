use std::process::{Command, Stdio};

#[test]
fn launch_plans_cover_three_harnesses() {
    let binary = env!("CARGO_BIN_EXE_ai-router");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/router.toml");
    for (client, expected) in [
        ("claude", "--print"),
        ("codex", "exec"),
        ("grok", "--single"),
    ] {
        let output = Command::new(binary)
            .args([
                "--config",
                config,
                "run",
                client,
                "--task",
                "Summarize this",
                "--offline",
                "--dry-run",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(plan["program"], client);
        assert!(plan["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == expected));
        assert_eq!(plan["decision"]["tier"], "fast");
    }
}

#[test]
fn passthrough_cannot_override_routed_model_or_effort() {
    let binary = env!("CARGO_BIN_EXE_ai-router");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/router.toml");
    for extra in ["--model=opus", "--effort=high", "-cmodel=\"other\""] {
        let output = Command::new(binary)
            .args([
                "--config",
                config,
                "run",
                "codex",
                "--task",
                "Summarize this",
                "--offline",
                "--dry-run",
                "--",
                extra,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{extra}");
    }
}

#[test]
fn fusion_dry_run_loads_workflow_and_roles_from_config() {
    let binary = env!("CARGO_BIN_EXE_ai-router");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/router.toml");
    let output = Command::new(binary)
        .args([
            "--config",
            config,
            "fusion",
            "--task",
            "Add tests for the parser",
            "--dry-run",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        plan["workflow"],
        "lead brief -> sidekick implementation -> validation -> lead review -> optional correction"
    );
    assert_eq!(plan["lead"]["client"], "codex");
    assert_eq!(plan["lead"]["model"], "gpt-6-astra");
    assert_eq!(plan["sidekick"]["client"], "codex");
    assert_eq!(plan["sidekick"]["model"], "gpt-5.6-sol");
    assert_eq!(plan["lead"]["model"], "gpt-6-astra");
    assert_eq!(plan["sidekick"]["client"], "codex");
    assert_eq!(plan["sidekick"]["model"], "gpt-5.6-sol");
}

#[test]
fn route_json_is_stdin_stdout_protocol() {
    let binary = env!("CARGO_BIN_EXE_ai-router");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/router.toml");
    let mut child = Command::new(binary)
        .args(["--config", config, "route-json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"client":"grok","task":"format code","offline":true}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let decision: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(decision["tier"], "fast");
}

#[test]
fn forced_toon_reports_expansion_without_overflow() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.json");
    std::fs::write(&input, "[]").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ai-router"))
        .args(["toon", "--input", input.to_str().unwrap(), "--force"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("reduction"));
}
