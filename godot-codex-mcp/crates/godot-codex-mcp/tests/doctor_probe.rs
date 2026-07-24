use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const DEADLINE: Duration = Duration::from_secs(5);

#[test]
fn doctor_probe_is_project_read_only_and_never_starts_bridge_sync() {
    let project = TempDir::new().unwrap();
    fs::write(project.path().join("project.godot"), "[application]\n").unwrap();
    let before = project_tree(project.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_godot-codex-mcp"))
        .arg("--doctor-probe")
        .arg("--project-root")
        .arg(project.path())
        .current_dir(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdout_thread = thread::spawn(move || {
        let mut output = Vec::new();
        stdout
            .take(8 * 1024 * 1024)
            .read_to_end(&mut output)
            .unwrap();
        output
    });
    let stderr_thread = thread::spawn(move || {
        let mut output = Vec::new();
        stderr.take(64 * 1024).read_to_end(&mut output).unwrap();
        output
    });
    {
        let mut stdin = child.stdin.take().unwrap();
        for frame in [
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "elicitation": {
                            "form": {"schemaValidation": true}
                        }
                    },
                    "clientInfo": {"name": "doctor-integration-test", "version": "1"}
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "resources/list", "params": {}}),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "resources/templates/list",
                "params": {}
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "godot_get_connection_status",
                    "arguments": {}
                }
            }),
        ] {
            serde_json::to_writer(&mut stdin, &frame).unwrap();
            stdin.write_all(b"\n").unwrap();
        }
        stdin.flush().unwrap();
    }
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() >= DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!("doctor stdio probe exceeded its deadline");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_thread.join().unwrap();
    let stderr = stderr_thread.join().unwrap();
    assert!(
        status.success(),
        "doctor probe failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(stdout.len() < 8 * 1024 * 1024);
    let responses = BufReader::new(stdout.as_slice())
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
        .collect::<Vec<_>>();
    let responses = responses
        .into_iter()
        .map(|response| {
            (
                response.get("id").and_then(Value::as_u64).unwrap(),
                response,
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        responses.keys().copied().collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    let tools = responses[&2]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 41);
    assert!(tools.iter().any(|tool| {
        tool.get("name").and_then(Value::as_str) == Some("godot_get_connection_status")
    }));
    assert_eq!(
        responses[&5]["result"]["structuredContent"]["package_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        project_tree(project.path()),
        before,
        "doctor-probe mode mutated the project"
    );
    assert!(!project.path().join(".godot/codex/bridge.json").exists());
}

fn project_tree(root: &std::path::Path) -> Vec<String> {
    fn visit(root: &std::path::Path, path: &std::path::Path, output: &mut Vec<String>) {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            output.push(entry.strip_prefix(root).unwrap().display().to_string());
            if entry.is_dir() {
                visit(root, &entry, output);
            }
        }
    }
    let mut output = Vec::new();
    visit(root, root, &mut output);
    output
}
