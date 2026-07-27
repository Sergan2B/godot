#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use godot_codex_index_store::{
    DependencyEdge, DependencyResolution, GenerationState, IdentityStrength, IndexGeneration,
    IngestionCheckpoint, LOGICAL_SCHEMA_V1, RecordValidity, ResourceEntity, SceneDomainGeneration,
    ScriptAdapterAvailability, ScriptAdapterProfile, ScriptAdapterStatus, ScriptDomainGeneration,
    ScriptLanguage, SegmentStore, SourceDocument,
};
use godot_codex_product::{
    COMPATIBILITY_MATRIX_JSON, HOST_COORDINATE_PROFILE_JSON, READ_ONLY_TOOLS,
    REGISTRY_PROFILE_JSON, SERVER_INSTRUCTIONS_TEXT,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const PROCESS_DEADLINE: Duration = Duration::from_secs(8);
const RESPONSE_DEADLINE: Duration = Duration::from_secs(20);
const PACKAGE_MANIFEST_SCHEMA: &str = "godot-codex-package/1.0";
const PACKAGE_MANIFEST_NAME: &str = "package-manifest.json";
const CHECKSUMS_NAME: &str = "checksums.sha256";
const OWNER_NAME: &str = ".godot-codex-owned";
const THIRD_PARTY_LICENSES: &[u8] = include_bytes!("../../../product/THIRD_PARTY_LICENSES.txt");

#[test]
fn production_sidecar_serves_only_verified_disk_cache_and_fails_closed() {
    let project = offline_project();
    let data_root = installed_package();
    install_offline_generation(project.path());
    write_project_config(project.path(), data_root.path());
    let expected_sources = source_digests(project.path());

    let mut session = McpProcess::start(project.path(), data_root.path());
    session.initialize();
    let status = session.wait_for_status("offline_cached", "editor_offline");
    assert_eq!(
        status.pointer("/static_cache/source_hashes_verified"),
        Some(&json!(true))
    );
    assert_eq!(
        status.pointer("/static_cache/condition"),
        Some(&json!("verified_current"))
    );
    assert_eq!(status.pointer("/bridge/condition"), Some(&json!("offline")));
    assert!(!project.path().join(".godot/codex/bridge.json").exists());

    let dependencies = session.call_tool(
        "godot_get_resource_dependencies",
        json!({"resource": "res://a.tres", "limit": 10}),
    );
    assert_eq!(dependencies["freshness"], "offline_cached");
    assert_eq!(dependencies["offline_cached"], true);
    assert_eq!(
        dependencies.pointer("/dependencies/0/target/path"),
        Some(&json!("res://b.tres"))
    );
    assert!(!dependencies.to_string().contains("editor_session_id"));

    let owners = session.call_tool(
        "godot_find_resource_owners",
        json!({"resource": "res://b.tres", "limit": 10}),
    );
    assert_eq!(owners["freshness"], "offline_cached");
    assert_eq!(owners["offline_cached"], true);
    assert_eq!(owners["owners"].as_array().map(Vec::len), Some(1));
    assert!(!owners.to_string().contains("editor_session_id"));

    for (tool, expected_code) in [
        ("godot_get_editor_state", "editor_offline"),
        ("godot_get_runtime_tree", "runtime_unavailable"),
        ("godot_prepare_create_node", "editor_offline"),
    ] {
        let blocked = session.call_tool(tool, json!({}));
        assert_eq!(
            blocked.pointer("/error/code"),
            Some(&json!(expected_code)),
            "{tool}"
        );
        assert_eq!(
            blocked.pointer("/error/status"),
            Some(&json!("offline_cached")),
            "{tool}"
        );
    }

    fs::write(
        project.path().join("a.tres"),
        b"[gd_resource]\nresource_name = \"changed\"\n",
    )
    .unwrap();
    session.wait_for_status("offline_empty", "static_cache_stale");
    assert_static_query_unavailable(&mut session, "index_not_current");

    fs::write(
        project.path().join("a.tres"),
        b"[gd_resource]\nresource_name = \"a\"\n",
    )
    .unwrap();
    session.wait_for_status("offline_cached", "editor_offline");

    fs::write(project.path().join("player.gd.uid"), b"uid://changed\n").unwrap();
    session.wait_for_status("offline_empty", "static_cache_stale");
    assert_static_query_unavailable(&mut session, "index_not_current");
    fs::write(project.path().join("player.gd.uid"), b"uid://player\n").unwrap();
    session.wait_for_status("offline_cached", "editor_offline");
    session.finish();

    corrupt_active_commit(project.path());
    let mut corrupt_session = McpProcess::start(project.path(), data_root.path());
    corrupt_session.initialize();
    let corrupt_status = corrupt_session.wait_for_offline_empty();
    assert_eq!(
        corrupt_status.pointer("/static_cache/source_hashes_verified"),
        Some(&json!(false))
    );
    assert_static_query_unavailable(&mut corrupt_session, "index_not_ready");
    corrupt_session.finish();

    assert_eq!(source_digests(project.path()), expected_sources);
}

fn assert_static_query_unavailable(session: &mut McpProcess, expected_code: &str) {
    let result = session.call_tool(
        "godot_get_resource_dependencies",
        json!({"resource": "res://a.tres", "limit": 10}),
    );
    assert_eq!(
        result.pointer("/error/code").and_then(Value::as_str),
        Some(expected_code),
        "a non-current cache did not fail with the canonical index error: {result}"
    );
    assert_ne!(result.get("freshness"), Some(&json!("offline_cached")));
    assert!(result.get("dependencies").is_none());
}

struct McpProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    responses: Receiver<Result<Value, String>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<Vec<u8>>>,
    next_id: u64,
}

impl McpProcess {
    fn start(project_root: &Path, data_root: &Path) -> Self {
        let executable = data_root
            .join("current")
            .join("bin")
            .join("godot-codex-mcp");
        let mut child = Command::new(executable)
            .arg("--project-root")
            .arg(project_root)
            .current_dir(project_root)
            .env("GODOT_CODEX_DATA_ROOT", data_root)
            .env_remove("GODOT_CODEX_DEBUG_ERRORS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sender, responses) = mpsc::sync_channel(128);
        let stdout_thread = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let parsed = line.map_err(|error| error.to_string()).and_then(|line| {
                    serde_json::from_str(&line).map_err(|error| error.to_string())
                });
                if sender.send(parsed).is_err() {
                    return;
                }
            }
        });
        let stderr_thread = thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.take(256 * 1024).read_to_end(&mut bytes).unwrap();
            bytes
        });
        Self {
            child: Some(child),
            stdin: Some(stdin),
            responses,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
            next_id: 1,
        }
    }

    fn initialize(&mut self) {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {
                    "elicitation": {
                        "form": {"schemaValidation": true}
                    }
                },
                "clientInfo": {
                    "name": "offline-subprocess-integration",
                    "version": "1"
                }
            }),
        );
        assert!(response.get("result").is_some(), "{response}");
        self.notify("notifications/initialized", json!({}));
    }

    fn wait_for_status(&mut self, expected_status: &str, expected_diagnostic: &str) -> Value {
        let deadline = Instant::now() + RESPONSE_DEADLINE;
        let mut last = Value::Null;
        while Instant::now() < deadline {
            last = self.call_tool("godot_get_connection_status", json!({}));
            if last.get("status").and_then(Value::as_str) == Some(expected_status)
                && last.pointer("/diagnostic/code").and_then(Value::as_str)
                    == Some(expected_diagnostic)
            {
                return last;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("connection status did not become {expected_status}/{expected_diagnostic}: {last}");
    }

    fn wait_for_offline_empty(&mut self) -> Value {
        let deadline = Instant::now() + RESPONSE_DEADLINE;
        let mut last = Value::Null;
        while Instant::now() < deadline {
            last = self.call_tool("godot_get_connection_status", json!({}));
            if last.get("status").and_then(Value::as_str) == Some("offline_empty")
                && matches!(
                    last.pointer("/diagnostic/code").and_then(Value::as_str),
                    Some(
                        "static_cache_stale"
                            | "static_cache_rebuilding"
                            | "static_cache_unavailable"
                    )
                )
            {
                return last;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("connection status did not become fail-closed offline_empty: {last}");
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        response
            .pointer("/result/structuredContent")
            .unwrap_or_else(|| panic!("{name} returned no structured content: {response}"))
            .clone()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let deadline = Instant::now() + RESPONSE_DEADLINE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let response = match self.responses.recv_timeout(remaining) {
                Ok(response) => response
                    .unwrap_or_else(|error| panic!("{method} returned invalid JSON: {error}")),
                Err(error) => {
                    let (status, stderr) = self.stop_for_failure();
                    panic!(
                        "{method} response timed out: {error}; status={status:?}; stderr={}",
                        String::from_utf8_lossy(&stderr)
                    );
                }
            };
            if response.get("id").and_then(Value::as_u64) == Some(id) {
                return response;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    fn send(&mut self, value: &Value) {
        let stdin = self.stdin.as_mut().expect("sidecar stdin is open");
        serde_json::to_writer(&mut *stdin, value).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn finish(mut self) {
        self.stdin.take();
        let started = Instant::now();
        let status = loop {
            let child = self.child.as_mut().unwrap();
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() >= PROCESS_DEADLINE {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("production sidecar did not exit after MCP stdin closed");
            }
            thread::sleep(Duration::from_millis(10));
        };
        self.child.take();
        self.stdout_thread.take().unwrap().join().unwrap();
        let stderr = self.stderr_thread.take().unwrap().join().unwrap();
        assert!(
            status.success(),
            "production sidecar failed: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(
            stderr.len() < 256 * 1024,
            "production sidecar stderr exceeded its bound"
        );
    }

    fn stop_for_failure(&mut self) -> (Option<ExitStatus>, Vec<u8>) {
        self.stdin.take();
        let status = self.child.as_mut().and_then(|child| {
            child.try_wait().unwrap_or(None).or_else(|| {
                child.kill().ok()?;
                child.wait().ok()
            })
        });
        self.child.take();
        if let Some(thread) = self.stdout_thread.take() {
            let _ = thread.join();
        }
        let stderr = self
            .stderr_thread
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_default();
        (status, stderr)
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(thread) = self.stdout_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.stderr_thread.take() {
            let _ = thread.join();
        }
    }
}

fn offline_project() -> TempDir {
    let project = TempDir::new().unwrap();
    fs::write(
        project.path().join("project.godot"),
        b"[application]\nconfig/name=\"Offline Subprocess\"\n",
    )
    .unwrap();
    fs::write(
        project.path().join("a.tres"),
        b"[gd_resource]\nresource_name = \"a\"\n",
    )
    .unwrap();
    fs::write(
        project.path().join("b.tres"),
        b"[gd_resource]\nresource_name = \"b\"\n",
    )
    .unwrap();
    fs::write(project.path().join("player.gd"), b"extends Node\n").unwrap();
    fs::write(project.path().join("player.gd.uid"), b"uid://player\n").unwrap();
    project
}

fn install_offline_generation(project_root: &Path) {
    let project_id = godot_codex_bridge_client::project_id_for_path(project_root).unwrap();
    let a = fs::read(project_root.join("a.tres")).unwrap();
    let b = fs::read(project_root.join("b.tres")).unwrap();
    let player = fs::read(project_root.join("player.gd")).unwrap();
    let resource = |entity_id: &str, uid: &str, path: &str, bytes: &[u8]| ResourceEntity {
        entity_id: entity_id.to_owned(),
        identity_input: uid.to_owned(),
        uid: Some(uid.to_owned()),
        display_path: path.to_owned(),
        comparison_path: path.to_owned(),
        identity_strength: IdentityStrength::ResourceUid,
        resource_type: "Resource".to_owned(),
        source_kind: "source".to_owned(),
        import_state: "not_imported".to_owned(),
        authority: "editor_file_system".to_owned(),
        content_generation: Some(prefixed_sha256(bytes)),
        mtime_ns: 1,
        byte_size: bytes.len() as u64,
        validity: RecordValidity::Valid,
        resource_revision: 1,
    };
    let resources = vec![
        resource("entity-a", "uid://a", "res://a.tres", &a),
        resource("entity-b", "uid://b", "res://b.tres", &b),
        resource("entity-player", "uid://player", "res://player.gd", &player),
    ];
    let source_documents = resources
        .iter()
        .map(|resource| SourceDocument {
            entity_id: resource.entity_id.clone(),
            comparison_path: resource.comparison_path.clone(),
            size_before: resource.byte_size,
            size_after: resource.byte_size,
            mtime_before_ns: 1,
            mtime_after_ns: 1,
            content_generation: resource.content_generation.clone(),
            ingest_state: "ready".to_owned(),
        })
        .collect();
    let mut generation = IndexGeneration {
        generation_id: format!("generation:sha256:{}", "8".repeat(64)),
        parent_generation_id: None,
        schema_version: LOGICAL_SCHEMA_V1,
        project_id: project_id.clone(),
        index_revision: 1,
        state: GenerationState::Active,
        creation_reason: "full_snapshot".to_owned(),
        checkpoint: IngestionCheckpoint {
            editor_session_id: format!("editor:{}", "1".repeat(32)),
            resource_revision: 1,
            project_revision: 1,
            index_revision: 1,
            source_complete: true,
            snapshot_checksum: format!("sha256:{}", "2".repeat(64)),
            last_batch_id: None,
            last_batch_checksum: None,
        },
        resources,
        source_documents,
        dependencies: vec![DependencyEdge {
            edge_id: "edge-a-b".to_owned(),
            source_entity_id: "entity-a".to_owned(),
            target_uid: Some("uid://b".to_owned()),
            target_comparison_path: Some("res://b.tres".to_owned()),
            target_display_path: Some("res://b.tres".to_owned()),
            target_entity_id: Some("entity-b".to_owned()),
            resolved_target_path: Some("res://b.tres".to_owned()),
            relation: "references".to_owned(),
            declared_type: Some("Resource".to_owned()),
            authority: "godot_resource_loader".to_owned(),
            resolution: DependencyResolution::Resolved,
            resource_revision: 1,
        }],
        diagnostics: Vec::new(),
        tombstones: Vec::new(),
        scene: SceneDomainGeneration {
            editor_session_id: format!("editor:{}", "1".repeat(32)),
            resource_revision: 1,
            scene_graph_revision: 1,
            source_complete: true,
            snapshot_checksum: format!("sha256:{}", "3".repeat(64)),
            ..SceneDomainGeneration::default()
        },
        script: ScriptDomainGeneration {
            editor_session_id: format!("editor:{}", "1".repeat(32)),
            resource_revision: 1,
            scene_graph_revision: 1,
            script_graph_revision: 1,
            source_complete: true,
            snapshot_checksum: format!("sha256:{}", "4".repeat(64)),
            semantic_digest: format!("sha256:{}", "5".repeat(64)),
            adapter_statuses: vec![
                ScriptAdapterStatus {
                    language: ScriptLanguage::Gdscript,
                    availability: ScriptAdapterAvailability::Available,
                    profile: Some(ScriptAdapterProfile::GdscriptParserAnalyzerV1),
                    version: Some("1".to_owned()),
                    diagnostic: None,
                },
                ScriptAdapterStatus {
                    language: ScriptLanguage::Csharp,
                    availability: ScriptAdapterAvailability::DiscoveryOnly,
                    profile: Some(ScriptAdapterProfile::CsharpDiscoveryOnlyV1),
                    version: Some("1".to_owned()),
                    diagnostic: None,
                },
            ],
            ..ScriptDomainGeneration::default()
        },
        validation_digest: String::new(),
    };
    generation.scene.validation_digest = generation.scene.compute_validation_digest();
    generation.script.validation_digest = generation.script.compute_validation_digest();
    generation.canonicalize();
    generation.validation_digest = generation.compute_validation_digest();
    generation.validate().unwrap();
    {
        let mut store = SegmentStore::open(project_root, &project_id).unwrap();
        store.activate(&generation, None).unwrap();
    }
    godot_codex_resource_indexer::persist_offline_authority(project_root, &generation).unwrap();
}

fn installed_package() -> TempDir {
    let root = TempDir::new().unwrap();
    let package_root = root.path().join("versions").join(env!("CARGO_PKG_VERSION"));
    let bin = package_root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_file(
        &bin.join("godot-codex-mcp"),
        &fs::read(env!("CARGO_BIN_EXE_godot-codex-mcp")).unwrap(),
        0o755,
    );
    write_file(&bin.join("godot-codex"), b"#!/bin/sh\nexit 2\n", 0o755);

    let matrix: Value = serde_json::from_str(COMPATIBILITY_MATRIX_JSON).unwrap();
    let source_commit = "a".repeat(40);
    let version = format!("{}\n", env!("CARGO_PKG_VERSION"));
    let source_commit_file = format!("{source_commit}\n");
    let content_bytes = [
        (
            "bin/godot-codex",
            fs::read(bin.join("godot-codex")).unwrap(),
            "0755",
        ),
        (
            "bin/godot-codex-mcp",
            fs::read(bin.join("godot-codex-mcp")).unwrap(),
            "0755",
        ),
        ("VERSION", version.into_bytes(), "0644"),
        ("SOURCE_COMMIT", source_commit_file.into_bytes(), "0644"),
        (
            "share/godot-codex/product/compatibility-matrix.v1.json",
            COMPATIBILITY_MATRIX_JSON.as_bytes().to_vec(),
            "0644",
        ),
        (
            "share/godot-codex/product/registry-profile.v1.json",
            REGISTRY_PROFILE_JSON.as_bytes().to_vec(),
            "0644",
        ),
        (
            "share/godot-codex/product/host-coordinate-profile.v1.json",
            HOST_COORDINATE_PROFILE_JSON.as_bytes().to_vec(),
            "0644",
        ),
        (
            "share/godot-codex/product/server-instructions.v1.txt",
            SERVER_INSTRUCTIONS_TEXT.as_bytes().to_vec(),
            "0644",
        ),
        (
            "share/godot-codex/licenses/THIRD_PARTY_LICENSES.txt",
            THIRD_PARTY_LICENSES.to_vec(),
            "0644",
        ),
    ];
    let mut contents = Vec::new();
    for (relative, bytes, mode) in content_bytes {
        let path = package_root.join(relative);
        write_file(&path, &bytes, u32::from_str_radix(mode, 8).unwrap());
        contents.push(json!({
            "mode": mode,
            "path": relative,
            "sha256": prefixed_sha256(&bytes),
            "bytes": bytes.len(),
        }));
    }
    let target = matrix["package"]["target"].clone();
    let godot = &matrix["godot"];
    let manifest = json!({
        "schema_version": PACKAGE_MANIFEST_SCHEMA,
        "package_version": env!("CARGO_PKG_VERSION"),
        "source_commit": source_commit,
        "target": target,
        "build_provenance": {
            "cargo_lock_sha256": format!("sha256:{}", "1".repeat(64)),
            "cargo_version": "cargo 1.94.1 (offline-subprocess-test)",
            "fresh_target": true,
            "rust_toolchain_sha256": format!("sha256:{}", "2".repeat(64)),
            "rustc_commit": "b".repeat(40),
            "rustc_release": "1.94.1",
            "target_triple": "aarch64-apple-darwin",
        },
        "godot_prerequisite": {
            "architecture": godot["target"]["architecture"],
            "commit": godot["source_commit"],
            "expected_install_path": "~/Applications/Godot Codex.app/Contents/MacOS/Godot",
            "sha256": format!("sha256:{}", godot["artifact_sha256"].as_str().unwrap()),
            "verification": {
                "sha256": ["/usr/bin/shasum", "-a", "256", "<godot-binary>"],
                "version": ["<godot-binary>", "--version"],
            },
            "version": godot["build_id"],
        },
        "compatibility_matrix_sha256": prefixed_sha256(COMPATIBILITY_MATRIX_JSON.as_bytes()),
        "registry_sha256": prefixed_sha256(REGISTRY_PROFILE_JSON.as_bytes()),
        "third_party_licenses_sha256": prefixed_sha256(THIRD_PARTY_LICENSES),
        "contents": contents,
        "checksums_path": CHECKSUMS_NAME,
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    write_file(
        &package_root.join(PACKAGE_MANIFEST_NAME),
        &manifest_bytes,
        0o644,
    );
    let mut checksums = manifest["contents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|content| {
            format!(
                "{}  {}\n",
                content["sha256"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("sha256:"),
                content["path"].as_str().unwrap()
            )
        })
        .collect::<Vec<_>>();
    checksums.push(format!(
        "{}  {PACKAGE_MANIFEST_NAME}\n",
        raw_sha256(&manifest_bytes)
    ));
    checksums.sort();
    write_file(
        &package_root.join(CHECKSUMS_NAME),
        checksums.concat().as_bytes(),
        0o644,
    );
    write_file(
        &package_root.join(OWNER_NAME),
        format!("{}\n", raw_sha256(&manifest_bytes)).as_bytes(),
        0o600,
    );
    symlink(&package_root, root.path().join("current")).unwrap();
    root
}

fn write_project_config(project_root: &Path, data_root: &Path) {
    let command = data_root
        .join("current")
        .join("bin")
        .join("godot-codex-mcp");
    let command = serde_json::to_string(command.to_str().unwrap()).unwrap();
    let canonical_project_root = fs::canonicalize(project_root).unwrap();
    let command_cwd = serde_json::to_string(canonical_project_root.to_str().unwrap()).unwrap();
    let enabled = READ_ONLY_TOOLS
        .iter()
        .map(|tool| format!("  {},", serde_json::to_string(tool).unwrap()))
        .collect::<Vec<_>>()
        .join("\n");
    let config = format!(
        "[mcp_servers.godot_editor]\n\
         command = {command}\n\
         args = [\"--project-root\", \".\"]\n\
         cwd = {command_cwd}\n\
         required = true\n\
         startup_timeout_sec = 10\n\
         tool_timeout_sec = 60\n\
         enabled_tools = [\n{enabled}\n]\n"
    );
    let directory = project_root.join(".codex");
    fs::create_dir(&directory).unwrap();
    write_file(&directory.join("config.toml"), config.as_bytes(), 0o600);
}

fn source_digests(project_root: &Path) -> BTreeMap<String, String> {
    [
        "project.godot",
        "a.tres",
        "b.tres",
        "player.gd",
        "player.gd.uid",
    ]
    .into_iter()
    .map(|relative| {
        (
            relative.to_owned(),
            raw_sha256(&fs::read(project_root.join(relative)).unwrap()),
        )
    })
    .collect()
}

fn corrupt_active_commit(project_root: &Path) {
    let directory = project_root.join(".godot/codex/index/commits");
    let mut commits = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    commits.sort();
    let active = commits.last().expect("offline store has an active commit");
    fs::write(active, b"{").unwrap();
}

fn write_file(path: &Path, bytes: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn prefixed_sha256(bytes: &[u8]) -> String {
    format!("sha256:{}", raw_sha256(bytes))
}

fn raw_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
