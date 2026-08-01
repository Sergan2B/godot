#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use godot_codex_operations::{
    PRODUCT_VERSION, SetupOptions, SetupProfile, SurfaceCaptureArmOptions,
    SurfaceCaptureProjectContext, SurfaceCaptureSurface, apply_setup_plan, arm_surface_capture,
    prepare_setup, verify_surface_capture_project,
};
use godot_codex_product::{
    COMPATIBILITY_MATRIX_JSON, HOST_COORDINATE_PROFILE_JSON, REGISTRY_PROFILE_JSON,
    SERVER_INSTRUCTIONS_TEXT,
};
use godot_codex_surface_capture::{
    ArmRequest, BindingDigests, CaptureDirection, CaptureEvent, CaptureEventClass, CaptureJournal,
    LeaseState, LeaseStore, MetadataBinding, SemanticProjection, Surface, ToolObservation,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const PROCESS_DEADLINE: Duration = Duration::from_secs(8);
const RESPONSE_DEADLINE: Duration = Duration::from_secs(30);
const CLAIM_DEADLINE: Duration = Duration::from_secs(5);
const HELPER_DEADLINE: Duration = Duration::from_secs(60);
const PACKAGE_MANIFEST_SCHEMA: &str = "godot-codex-package/1.0";
const PACKAGE_MANIFEST_NAME: &str = "package-manifest.json";
const CHECKSUMS_NAME: &str = "checksums.sha256";
const OWNER_NAME: &str = ".godot-codex-owned";
const CAPTURE_DIRECTORY: &str = "surface-capture-v1";
const RUNS_DIRECTORY: &str = "runs";
const CONFIG_PATH: &str = ".codex/config.toml";
const RECEIPT_PATH: &str = ".godot/codex/setup-receipt-v1.json";
const HELPER_MARKER: &str = "GODOT_CODEX_CAPTURE_PROCESS_HELPER";
const HELPER_ACTION: &str = "GODOT_CODEX_CAPTURE_PROCESS_ACTION";
const HELPER_PROJECT: &str = "GODOT_CODEX_CAPTURE_PROCESS_PROJECT";
const HELPER_PLAN_STORE: &str = "GODOT_CODEX_CAPTURE_PROCESS_PLAN_STORE";
const HELPER_METADATA: &str = "GODOT_CODEX_CAPTURE_PROCESS_METADATA";
const HELPER_REPORT: &str = "GODOT_CODEX_CAPTURE_PROCESS_REPORT";
const THIRD_PARTY_LICENSES: &[u8] = include_bytes!("../../../product/THIRD_PARTY_LICENSES.txt");

fn fixture_digest(character: char) -> String {
    format!("sha256:{}", character.to_string().repeat(64))
}

fn valid_surface_metadata(context: &SurfaceCaptureProjectContext) -> Value {
    let package_manifest_sha256 = fixture_digest('7');
    json!({
        "schema_version": "s11-surface-metadata/1.1",
        "surface": "cli",
        "package": {
            "version": PRODUCT_VERSION,
            "detached_manifest_sha256": package_manifest_sha256,
            "internal_manifest_sha256": context.internal_manifest_sha256(),
            "mcp_binary_sha256": context.package_launcher_sha256(),
        },
        "host": {
            "name": "Codex",
            "identifier": "com.openai.codex",
            "artifact_kind": "macos_application",
            "version": "1.2.3",
            "build": "123",
            "commit": null,
            "client_version": "1.2.3",
            "host_metadata_sha256": null,
            "host_code_signature": null,
            "client_code_signature": null,
            "ide_host_version": null,
            "ide_shell_identifier": null,
            "ide_shell_artifact_sha256": null,
            "ide_shell_team_id": null,
            "ide_shell_code_signature": null,
            "architecture": "arm64",
        },
        "bindings": {
            "package_source_commit": "8".repeat(40),
            "package_manifest_sha256": package_manifest_sha256,
            "compatibility_matrix_sha256": fixture_digest('9'),
            "host_coordinate_profile_sha256": fixture_digest('a'),
            "project_fixture_sha256": fixture_digest('b'),
            "registry_sha256": fixture_digest('c'),
            "prompt_pack_sha256": fixture_digest('d'),
            "host_artifact_sha256": fixture_digest('e'),
            "client_artifact_sha256": fixture_digest('f'),
            "host_provenance_sha256": fixture_digest('0'),
            "mcp_binary_sha256": context.package_launcher_sha256(),
            "godot_artifact_sha256": fixture_digest('1'),
        },
        "machine_bindings": {
            "project_identity_sha256": context.project_identity_sha256(),
            "package_launcher_sha256": context.package_launcher_sha256(),
            "project_config_sha256": context.project_config_sha256(),
            "setup_receipt_sha256": context.setup_receipt_sha256(),
        },
        "registry": {
            "tools": (0..41)
                .map(|index| format!("godot_tool_{index:02}"))
                .collect::<Vec<_>>(),
            "fixed_resources": [
                "godot://one",
                "godot://two",
                "godot://three",
                "godot://four",
            ],
            "resource_templates": ["godot://resource/{id}"],
            "instructions_sha256": fixture_digest('2'),
        },
        "redaction": {
            "absolute_paths_absent": true,
            "account_identity_absent": true,
            "host_controls_absent": true,
            "secrets_absent": true,
        },
    })
}

#[test]
fn production_sidecar_capture_is_absent_by_default_one_shot_and_crash_safe() {
    let fixture = InstalledFixture::new();
    let expected_sources = fixture.source_digests();
    let expected_config = fs::read(fixture.project.path().join(CONFIG_PATH)).unwrap();
    let expected_receipt = fs::read(fixture.project.path().join(RECEIPT_PATH)).unwrap();
    let capture_root = fixture.data_root.path().join(CAPTURE_DIRECTORY);

    assert!(
        !capture_root.exists(),
        "setup must not create surface-capture state"
    );
    let (baseline_initialize, baseline_tools, baseline_output) = {
        let mut session =
            McpProcess::start(fixture.project.path(), fixture.data_root.path(), false);
        session.initialize();
        let tools = session.request("tools/list", json!({}));
        assert!(
            tools
                .pointer("/result/tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| tools.iter().any(|tool| {
                    tool.get("name").and_then(Value::as_str) == Some("godot_get_connection_status")
                })),
            "normal no-capture session did not pass tools/list through: {tools}"
        );
        let initialize_frame = session.response_frame(1);
        let tools_frame = session.response_frame(2);
        let output = session.finish();
        assert_eq!(
            output.stdout,
            [initialize_frame.as_slice(), tools_frame.as_slice()].concat(),
            "no-capture sidecar emitted non-response stdout bytes"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .all(|line| line
                    == "[godot-codex-index] connect failed: Godot bridge is unavailable"),
            "no-capture sidecar emitted unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        (initialize_frame, tools_frame, output)
    };
    assert!(
        !capture_root.exists(),
        "normal sidecar startup must remain a read-only capture no-op"
    );

    let first = fixture.arm("first");
    let first_run = first["run_id"].as_str().unwrap();
    let store = LeaseStore::open(fixture.data_root.path()).unwrap();
    assert_eq!(store.status(first_run).unwrap().state, LeaseState::Armed);

    fixture.run_version();
    assert_eq!(
        store.status(first_run).unwrap().state,
        LeaseState::Armed,
        "--version claimed an armed lease"
    );

    {
        let mut doctor = McpProcess::start(fixture.project.path(), fixture.data_root.path(), true);
        doctor.initialize();
        let status = doctor.call_tool("godot_get_connection_status", json!({}));
        assert!(status.get("status").and_then(Value::as_str).is_some());
        doctor.finish();
    }
    assert_eq!(
        store.status(first_run).unwrap().state,
        LeaseState::Armed,
        "--doctor-probe claimed an armed lease"
    );

    let first_journal_path =
        run_directory(fixture.data_root.path(), first_run).join("journal.json");
    let captured_output = {
        let mut captured =
            McpProcess::start(fixture.project.path(), fixture.data_root.path(), false);
        captured.initialize();
        wait_for_lease_state(&store, first_run, LeaseState::Claimed);
        let tools = captured.request("tools/list", json!({}));
        assert!(tools.pointer("/result/tools").is_some());
        let status = captured.call_tool("godot_get_connection_status", json!({}));
        assert!(status.get("status").and_then(Value::as_str).is_some());
        assert_eq!(
            captured.response_frame(1),
            baseline_initialize,
            "capture tap changed the deterministic initialize response bytes"
        );
        assert_eq!(
            captured.response_frame(2),
            baseline_tools,
            "capture tap changed the deterministic tools/list response bytes"
        );
        let status_frame = captured.response_frame(3);
        let output = captured.finish();
        assert_eq!(
            output.stdout,
            [
                baseline_initialize.as_slice(),
                baseline_tools.as_slice(),
                status_frame.as_slice(),
            ]
            .concat(),
            "captured sidecar emitted non-response stdout bytes"
        );
        output
    };
    assert_eq!(
        captured_output.stderr, baseline_output.stderr,
        "capture tap changed sidecar stderr"
    );
    wait_for_lease_state(&store, first_run, LeaseState::Finalized);
    let first_journal_bytes = fs::read(&first_journal_path).unwrap();
    let first_run_directory = run_directory(fixture.data_root.path(), first_run);
    assert!(
        !first_run_directory.join("claimed.lease.json").exists(),
        "completed capture retained its claimed lease"
    );
    assert!(
        !first_run_directory.join("armed.lease.json").exists(),
        "completed capture retained its armed lease"
    );
    assert_private_capture_tree(fixture.data_root.path(), first_run, true);
    assert_journal_outcome(&first_journal_bytes, &first, "completed");

    {
        let mut replay = McpProcess::start(fixture.project.path(), fixture.data_root.path(), false);
        replay.initialize();
        replay.finish();
    }
    assert_eq!(
        fs::read(&first_journal_path).unwrap(),
        first_journal_bytes,
        "a finalized capture was reused or rewritten"
    );
    assert_eq!(
        run_ids(fixture.data_root.path()),
        vec![first_run.to_owned()],
        "a normal restart created an unarmed capture run"
    );

    let terminated = fixture.arm_direct(&first);
    let terminated_run = terminated["run_id"].as_str().unwrap();
    let terminated_journal_path =
        run_directory(fixture.data_root.path(), terminated_run).join("journal.json");
    {
        let mut captured =
            McpProcess::start(fixture.project.path(), fixture.data_root.path(), false);
        captured.initialize();
        wait_for_lease_state(&store, terminated_run, LeaseState::Claimed);
        let tools = captured.request("tools/list", json!({}));
        assert!(tools.pointer("/result/tools").is_some());
        let status = captured.call_tool("godot_get_connection_status", json!({}));
        assert!(status.get("status").and_then(Value::as_str).is_some());
        captured.terminate_for_test();
    }
    wait_for_lease_state(&store, terminated_run, LeaseState::Finalized);
    assert_journal_outcome(
        &fs::read(&terminated_journal_path).unwrap(),
        &terminated,
        "cancelled",
    );

    let crash = fixture.arm_direct(&first);
    let crash_run = crash["run_id"].as_str().unwrap();
    let crash_directory = run_directory(fixture.data_root.path(), crash_run);
    {
        let mut captured =
            McpProcess::start(fixture.project.path(), fixture.data_root.path(), false);
        captured.initialize();
        wait_for_lease_state(&store, crash_run, LeaseState::Claimed);
        captured.kill_for_test();
    }
    assert_eq!(store.status(crash_run).unwrap().state, LeaseState::Claimed);
    assert!(
        crash_directory.join("claimed.lease.json").is_file(),
        "SIGKILL did not preserve claimed state"
    );
    assert!(
        !crash_directory.join("journal.json").exists(),
        "SIGKILL published a falsely complete journal"
    );
    assert!(
        !crash_directory.join(".journal.pending").exists(),
        "SIGKILL left a pending artifact despite no finalization"
    );
    assert_private_capture_tree(fixture.data_root.path(), crash_run, false);

    {
        let mut after_crash =
            McpProcess::start(fixture.project.path(), fixture.data_root.path(), false);
        after_crash.initialize();
        after_crash.finish();
    }
    assert_eq!(
        store.status(crash_run).unwrap().state,
        LeaseState::Claimed,
        "a second sidecar reused a previously claimed lease"
    );
    assert!(!crash_directory.join("journal.json").exists());

    assert_eq!(
        fs::read(fixture.project.path().join(CONFIG_PATH)).unwrap(),
        expected_config,
        "capture mutated setup-owned config"
    );
    assert_eq!(
        fs::read(fixture.project.path().join(RECEIPT_PATH)).unwrap(),
        expected_receipt,
        "capture mutated setup receipt"
    );
    assert_eq!(
        fixture.source_digests(),
        expected_sources,
        "capture mutated project source bytes"
    );
}

/// The integration-test executable doubles as the installed operations
/// executable only in a spawned, filtered helper process. This lets the real
/// setup APIs observe `versions/<version>/bin/godot-codex` as `current_exe`
/// and produce the same current-launcher-bound receipt as the shipped CLI.
#[test]
#[ignore]
fn installed_surface_capture_setup_helper() {
    if std::env::var_os(HELPER_MARKER).as_deref() != Some("1".as_ref()) {
        return;
    }
    let project = required_helper_path(HELPER_PROJECT);
    match std::env::var(HELPER_ACTION).unwrap().as_str() {
        "setup" => {
            let mut options = SetupOptions::new(&project, SetupProfile::FullBeta);
            options.plan_store = Some(required_helper_path(HELPER_PLAN_STORE));
            let preview = prepare_setup(&options).unwrap();
            apply_setup_plan(&preview.plan_digest, options.plan_store.as_deref()).unwrap();
        }
        "arm" => {
            let metadata_path = required_helper_path(HELPER_METADATA);
            let report_path = required_helper_path(HELPER_REPORT);
            let context = verify_surface_capture_project(&project).unwrap();
            let metadata = valid_surface_metadata(&context);
            let mut metadata_bytes = serde_json::to_vec(&metadata).unwrap();
            metadata_bytes.push(b'\n');
            write_file(&metadata_path, &metadata_bytes, 0o600);
            let report = arm_surface_capture(&SurfaceCaptureArmOptions::new(
                &project,
                SurfaceCaptureSurface::Cli,
                &metadata_path,
                300,
            ))
            .unwrap();
            let mut report_bytes = serde_json::to_vec(&report).unwrap();
            report_bytes.push(b'\n');
            write_file(&report_path, &report_bytes, 0o600);
        }
        action => panic!("unknown installed helper action: {action}"),
    }
}

fn required_helper_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("missing helper path: {name}"))
}

struct InstalledFixture {
    project: TempDir,
    data_root: TempDir,
    helper: PathBuf,
    sequence: std::cell::Cell<u64>,
}

impl InstalledFixture {
    fn new() -> Self {
        let project = TempDir::new().unwrap();
        write_file(
            &project.path().join("project.godot"),
            b"[application]\nconfig/name=\"Surface Capture Process\"\n",
            0o644,
        );
        write_file(
            &project.path().join("main.tscn"),
            b"[gd_scene format=3]\n\n[node name=\"Main\" type=\"Node\"]\n",
            0o644,
        );
        write_file(
            &project.path().join("main.gd"),
            b"extends Node\n\nfunc _ready() -> void:\n    pass\n",
            0o644,
        );
        let data_root = installed_package();
        let helper = data_root
            .path()
            .join("current")
            .join("bin")
            .join("godot-codex");
        let fixture = Self {
            project,
            data_root,
            helper,
            sequence: std::cell::Cell::new(0),
        };
        fixture.run_helper("setup", None, None);

        let config = fs::read_to_string(fixture.project.path().join(CONFIG_PATH)).unwrap();
        let expected_launcher = fixture.data_root.path().join("current/bin/godot-codex-mcp");
        assert!(
            config.contains(expected_launcher.to_str().unwrap()),
            "setup did not bind the stable installed current launcher"
        );
        assert!(
            config.contains("args = [\"--project-root\", \".\"]"),
            "setup did not preserve the exact sidecar arguments"
        );
        assert!(
            config.contains("GODOT_CODEX_DATA_ROOT"),
            "installed setup did not bind its private data root"
        );
        assert!(fixture.project.path().join(RECEIPT_PATH).is_file());
        fixture
    }

    fn arm(&self, label: &str) -> Value {
        let sequence = self.sequence.get().saturating_add(1);
        self.sequence.set(sequence);
        let metadata = self
            .data_root
            .path()
            .join(format!("capture-test-{label}-{sequence}.metadata.json"));
        let report = self
            .data_root
            .path()
            .join(format!("capture-test-{label}-{sequence}.report.json"));
        self.run_helper("arm", Some(&metadata), Some(&report));
        let value: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
        fs::remove_file(metadata).unwrap();
        fs::remove_file(report).unwrap();
        assert_eq!(value["state"], "armed");
        assert_eq!(value["surface"], "cli");
        value
    }

    fn arm_direct(&self, template: &Value) -> Value {
        let template_run = template["run_id"].as_str().unwrap();
        let metadata_document: Value = serde_json::from_slice(
            &fs::read(run_directory(self.data_root.path(), template_run).join("metadata.json"))
                .unwrap(),
        )
        .unwrap();
        let bindings = BindingDigests {
            package_launcher_sha256: template["package_launcher_sha256"]
                .as_str()
                .unwrap()
                .to_owned(),
            project_config_sha256: template["project_config_sha256"]
                .as_str()
                .unwrap()
                .to_owned(),
            setup_receipt_sha256: template["setup_receipt_sha256"]
                .as_str()
                .unwrap()
                .to_owned(),
        };
        let expires_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_add(300);
        let armed = LeaseStore::open(self.data_root.path())
            .unwrap()
            .arm(ArmRequest {
                project_root: self.project.path().to_path_buf(),
                surface: Surface::Cli,
                metadata: MetadataBinding::from_document(metadata_document).unwrap(),
                bindings,
                expires_at_unix,
            })
            .unwrap();
        json!({
            "run_id": armed.run_id,
            "project_identity": armed.project_identity,
            "metadata_sha256": armed.metadata_sha256,
            "package_launcher_sha256": armed.bindings.package_launcher_sha256,
            "project_config_sha256": armed.bindings.project_config_sha256,
            "setup_receipt_sha256": armed.bindings.setup_receipt_sha256,
        })
    }

    fn run_helper(&self, action: &str, metadata: Option<&Path>, report: Option<&Path>) {
        let plan_store = self.data_root.path().join("capture-test-plans");
        let mut command = Command::new(&self.helper);
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("installed_surface_capture_setup_helper")
            .arg("--nocapture")
            .env(HELPER_MARKER, "1")
            .env(HELPER_ACTION, action)
            .env(HELPER_PROJECT, self.project.path())
            .env(HELPER_PLAN_STORE, &plan_store)
            .env("GODOT_CODEX_DATA_ROOT", self.data_root.path())
            .env_remove("GODOT_CODEX_DEBUG_ERRORS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(metadata) = metadata {
            command.env(HELPER_METADATA, metadata);
        }
        if let Some(report) = report {
            command.env(HELPER_REPORT, report);
        }
        let output = bounded_output(command, HELPER_DEADLINE, "installed setup helper");
        assert!(
            output.status.success(),
            "installed setup helper failed for {action}: stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_version(&self) {
        let mut command = Command::new(self.data_root.path().join("current/bin/godot-codex-mcp"));
        command
            .arg("--version")
            .env("GODOT_CODEX_DATA_ROOT", self.data_root.path())
            .env_remove("GODOT_CODEX_DEBUG_ERRORS")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = bounded_output(command, PROCESS_DEADLINE, "--version");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("godot-codex-mcp {PRODUCT_VERSION}\n")
        );
    }

    fn source_digests(&self) -> BTreeMap<String, String> {
        ["project.godot", "main.tscn", "main.gd"]
            .into_iter()
            .map(|relative| {
                (
                    relative.to_owned(),
                    raw_sha256(&fs::read(self.project.path().join(relative)).unwrap()),
                )
            })
            .collect()
    }
}

fn bounded_output(mut command: Command, deadline: Duration, label: &str) -> Output {
    let mut child = command.spawn().unwrap();
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if started.elapsed() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "{label} exceeded its deadline; stdout={}; stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

struct McpProcess {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    responses: Receiver<Result<(Value, Vec<u8>), String>>,
    stdout_thread: Option<JoinHandle<Vec<u8>>>,
    stderr_thread: Option<JoinHandle<Vec<u8>>>,
    response_frames: BTreeMap<u64, Vec<u8>>,
    next_id: u64,
}

struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl McpProcess {
    fn start(project_root: &Path, data_root: &Path, doctor_probe: bool) -> Self {
        let executable = data_root.join("current/bin/godot-codex-mcp");
        let mut command = Command::new(executable);
        if doctor_probe {
            command.arg("--doctor-probe");
        }
        command
            .arg("--project-root")
            .arg(".")
            .current_dir(project_root)
            .env("GODOT_CODEX_DATA_ROOT", data_root)
            .env_remove("GODOT_CODEX_DEBUG_ERRORS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (sender, responses) = mpsc::sync_channel(128);
        let stdout_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut output = Vec::new();
            loop {
                let mut frame = Vec::new();
                match reader.read_until(b'\n', &mut frame) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        break;
                    }
                }
                output.extend_from_slice(&frame);
                let without_lf = frame.strip_suffix(b"\n").unwrap_or(&frame);
                let json_bytes = without_lf.strip_suffix(b"\r").unwrap_or(without_lf);
                let parsed = serde_json::from_slice(json_bytes)
                    .map(|value| (value, frame))
                    .map_err(|error| error.to_string());
                if sender.send(parsed).is_err() {
                    break;
                }
            }
            output
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
            response_frames: BTreeMap::new(),
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
                    "name": "surface-capture-process-test",
                    "version": "1"
                }
            }),
        );
        assert!(response.get("result").is_some(), "{response}");
        self.notify("notifications/initialized", json!({}));
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
            let (response, raw) = match self.responses.recv_timeout(remaining) {
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
                self.response_frames.insert(id, raw);
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

    fn response_frame(&self, id: u64) -> Vec<u8> {
        self.response_frames
            .get(&id)
            .unwrap_or_else(|| panic!("response frame {id} was not retained"))
            .clone()
    }

    fn finish(mut self) -> ProcessOutput {
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
        let stdout = self.stdout_thread.take().unwrap().join().unwrap();
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
        ProcessOutput { stdout, stderr }
    }

    fn kill_for_test(mut self) {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            child.kill().unwrap();
            let status = child.wait().unwrap();
            assert!(
                !status.success(),
                "kill_for_test did not SIGKILL the sidecar"
            );
        }
        self.stdout_thread.take().unwrap().join().unwrap();
        self.stderr_thread.take().unwrap().join().unwrap();
    }

    fn terminate_for_test(mut self) {
        let child = self.child.as_mut().unwrap();
        rustix::process::kill_process(
            rustix::process::Pid::from_child(child),
            rustix::process::Signal::TERM,
        )
        .unwrap();
        let started = Instant::now();
        let (status, timed_out) = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break (status, false);
            }
            if started.elapsed() >= PROCESS_DEADLINE {
                child.kill().unwrap();
                break (child.wait().unwrap(), true);
            }
            thread::sleep(Duration::from_millis(10));
        };
        self.stdin.take();
        self.child.take();
        let _ = self.stdout_thread.take().unwrap().join().unwrap();
        let stderr = self.stderr_thread.take().unwrap().join().unwrap();
        assert!(
            !timed_out,
            "production sidecar did not exit after SIGTERM: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(
            status.success(),
            "SIGTERM shutdown failed: {}",
            String::from_utf8_lossy(&stderr)
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

fn wait_for_lease_state(store: &LeaseStore, run_id: &str, expected: LeaseState) {
    let deadline = Instant::now() + CLAIM_DEADLINE;
    loop {
        let actual = store.status(run_id).unwrap().state;
        if actual == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "capture lease did not reach {expected:?}; last state: {actual:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_journal_outcome(bytes: &[u8], arm: &Value, expected_outcome: &str) {
    let artifact: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(
        artifact["schema_version"],
        "sprint11-surface-capture-artifact/1.1"
    );
    assert_eq!(artifact["outcome"], expected_outcome);
    assert_eq!(artifact["metadata_file"], "metadata.json");
    assert_eq!(artifact["metadata_sha256"], arm["metadata_sha256"]);

    let journal: CaptureJournal = serde_json::from_value(artifact["journal"].clone()).unwrap();
    assert_eq!(
        journal.schema_version,
        "sprint11-surface-capture-journal/1.1"
    );
    assert_eq!(journal.run_id, arm["run_id"]);
    assert_eq!(journal.transport_origin, "official_host_stdio");
    assert_eq!(journal.surface, "cli");
    assert_eq!(journal.project_identity, arm["project_identity"]);
    assert_eq!(journal.metadata_sha256, arm["metadata_sha256"]);
    assert_eq!(
        journal.package_launcher_sha256,
        arm["package_launcher_sha256"]
    );
    assert_eq!(journal.project_config_sha256, arm["project_config_sha256"]);
    assert_eq!(journal.setup_receipt_sha256, arm["setup_receipt_sha256"]);
    assert!(journal.observed_event_count >= 7);
    assert_eq!(journal.dropped_event_count, 0);
    assert!(!journal.events.is_empty());

    let mut previous = format!("sha256:{}", "0".repeat(64));
    for (index, event) in journal.events.iter().enumerate() {
        assert_eq!(event.sequence, u64::try_from(index).unwrap() + 1);
        assert_eq!(event.previous_event_sha256, previous);
        assert_eq!(event.event_sha256, capture_event_hash(event));
        previous = event.event_sha256.clone();
    }
    assert_eq!(journal.final_event_sha256, previous);

    for (method, phase) in [
        ("initialize", "request"),
        ("initialize", "response"),
        ("tools/list", "request"),
        ("tools/list", "response"),
    ] {
        assert!(
            journal
                .events
                .iter()
                .any(|event| event.method.as_deref() == Some(method) && event.phase == phase),
            "journal omitted {method}/{phase}"
        );
    }
    for phase in ["request", "response"] {
        assert!(
            journal.events.iter().any(|event| {
                event.tool.as_deref() == Some("godot_get_connection_status") && event.phase == phase
            }),
            "journal omitted godot_get_connection_status/{phase}"
        );
    }
}

fn capture_event_hash(event: &CaptureEvent) -> String {
    #[derive(Serialize)]
    struct HashPayload<'a> {
        sequence: u64,
        direction: CaptureDirection,
        class: CaptureEventClass,
        phase: &'a str,
        method: &'a Option<String>,
        tool: &'a Option<String>,
        registry_names: &'a [String],
        protocol_version: &'a Option<String>,
        client_name: &'a Option<String>,
        client_version: &'a Option<String>,
        instructions_sha256: &'a Option<String>,
        error_code: Option<i64>,
        semantic_projection: &'a Option<SemanticProjection>,
        tool_observation: &'a Option<ToolObservation>,
        previous_event_sha256: &'a str,
    }

    let payload = HashPayload {
        sequence: event.sequence,
        direction: event.direction,
        class: event.class,
        phase: &event.phase,
        method: &event.method,
        tool: &event.tool,
        registry_names: &event.registry_names,
        protocol_version: &event.protocol_version,
        client_name: &event.client_name,
        client_version: &event.client_version,
        instructions_sha256: &event.instructions_sha256,
        error_code: event.error_code,
        semantic_projection: &event.semantic_projection,
        tool_observation: &event.tool_observation,
        previous_event_sha256: &event.previous_event_sha256,
    };
    let bytes = serde_json::to_vec(&payload).unwrap();
    let mut digest = Sha256::new();
    digest.update(b"sprint11-surface-capture-event-v1.1\0");
    digest.update(bytes);
    format!("sha256:{:x}", digest.finalize())
}

fn assert_private_capture_tree(data_root: &Path, run_id: &str, finalized: bool) {
    let capture = data_root.join(CAPTURE_DIRECTORY);
    let runs = capture.join(RUNS_DIRECTORY);
    let run = runs.join(run_id);
    for directory in [&capture, &runs, &run] {
        let metadata = fs::symlink_metadata(directory).unwrap();
        assert!(metadata.is_dir());
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(metadata.mode() & 0o777, 0o700);
    }
    for file in [
        run.join("metadata.json"),
        if finalized {
            run.join("journal.json")
        } else {
            run.join("claimed.lease.json")
        },
    ] {
        let metadata = fs::symlink_metadata(file).unwrap();
        assert!(metadata.is_file());
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(metadata.mode() & 0o777, 0o600);
    }
}

fn run_directory(data_root: &Path, run_id: &str) -> PathBuf {
    data_root
        .join(CAPTURE_DIRECTORY)
        .join(RUNS_DIRECTORY)
        .join(run_id)
}

fn run_ids(data_root: &Path) -> Vec<String> {
    let mut values = fs::read_dir(data_root.join(CAPTURE_DIRECTORY).join(RUNS_DIRECTORY))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    values.sort();
    values
}

fn installed_package() -> TempDir {
    let root = TempDir::new().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let package_root = root.path().join("versions").join(PRODUCT_VERSION);
    let bin = package_root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_file(
        &bin.join("godot-codex-mcp"),
        &fs::read(env!("CARGO_BIN_EXE_godot-codex-mcp")).unwrap(),
        0o755,
    );
    write_file(
        &bin.join("godot-codex"),
        &fs::read(std::env::current_exe().unwrap()).unwrap(),
        0o755,
    );

    let matrix: Value = serde_json::from_str(COMPATIBILITY_MATRIX_JSON).unwrap();
    let source_commit = "a".repeat(40);
    let version = format!("{PRODUCT_VERSION}\n");
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
        "package_version": PRODUCT_VERSION,
        "source_commit": source_commit,
        "target": target,
        "build_provenance": {
            "cargo_lock_sha256": format!("sha256:{}", "1".repeat(64)),
            "cargo_version": "cargo 1.94.1 (surface-capture-process-test)",
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
