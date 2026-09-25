use std::process::{Command, Stdio};

#[test]
fn launch_plans_cover_three_harnesses() {
    let binary = env!("CARGO_BIN_EXE_ai-router");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/router.json");
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
fn route_json_is_stdin_stdout_protocol() {
    let binary = env!("CARGO_BIN_EXE_ai-router");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/router.json");
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
