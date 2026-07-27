use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use godot_codex_product::{
    FIXED_RESOURCE_URIS, FULL_BETA_TOOLS, READ_ONLY_TOOLS, RESOURCE_TEMPLATE_URIS, RegistryProfile,
    SurfaceKind,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    BridgeProbeObservation, DoctorProbeObservations, GodotProbeObservation, HostProbeObservation,
    McpProbeObservation, SurfaceSelection,
};

const LOCAL_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const BRIDGE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const MCP_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const TOTAL_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_COMMAND_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_MCP_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_MCP_TOTAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_MCP_FRAMES: usize = 64;
const MAX_PATH_ENTRIES: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_GODOT_BYTES: u64 = 512 * 1024 * 1024;
const APP_BUNDLE_ID: &str = "com.openai.codex";
const APP_ROOT: &str = "/Applications/ChatGPT.app";
const IDE_CODE: &str = "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code";

#[derive(Clone, Debug)]
pub(crate) struct LocalProbePrograms {
    pub auto_surface: Option<SurfaceKind>,
    pub cli_codex: Option<PathBuf>,
    pub app_plutil: PathBuf,
    pub app_info_plist: PathBuf,
    pub app_codex: PathBuf,
    pub ide_code: PathBuf,
    pub sidecar: PathBuf,
    pub command_timeout: Duration,
    pub mcp_timeout: Duration,
    pub total_timeout: Duration,
}

#[derive(Clone, Debug)]
pub(crate) struct GodotProbeSpec {
    pub path: PathBuf,
    pub architecture: String,
    pub source_commit: String,
    pub build_id: String,
    pub artifact_sha256: String,
}

impl LocalProbePrograms {
    pub(crate) fn for_system(sidecar: PathBuf) -> Self {
        let app_root = Path::new(APP_ROOT);
        Self {
            auto_surface: detect_auto_surface(),
            cli_codex: find_in_path("codex"),
            app_plutil: PathBuf::from("/usr/bin/plutil"),
            app_info_plist: app_root.join("Contents/Info.plist"),
            app_codex: app_root.join("Contents/Resources/codex"),
            ide_code: PathBuf::from(IDE_CODE),
            sidecar,
            command_timeout: LOCAL_COMMAND_TIMEOUT,
            mcp_timeout: MCP_PROBE_TIMEOUT,
            total_timeout: TOTAL_PROBE_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ProbeDeadline {
    started: Instant,
    budget: Duration,
}

impl ProbeDeadline {
    fn new(budget: Duration) -> Self {
        Self {
            started: Instant::now(),
            budget,
        }
    }

    fn slice(self, cap: Duration) -> Option<Duration> {
        self.budget
            .checked_sub(self.started.elapsed())
            .map(|remaining| remaining.min(cap))
            .filter(|remaining| !remaining.is_zero())
    }
}

pub(crate) fn collect_local_observations(
    project_root: &Path,
    selection: SurfaceSelection,
    protocol_version: &str,
    registry: &RegistryProfile,
    godot_spec: Option<&GodotProbeSpec>,
    programs: &LocalProbePrograms,
    expected_launcher: &Path,
) -> DoctorProbeObservations {
    let deadline = ProbeDeadline::new(programs.total_timeout);
    let selected = selection.explicit().or(programs.auto_surface);
    let host = selected.and_then(|surface| {
        probe_host(project_root, surface, programs, expected_launcher, deadline)
    });
    let host_form = host
        .as_ref()
        .and_then(|observation| observation.supports_form_elicitation);
    let godot = godot_spec.and_then(|spec| {
        probe_godot(
            spec,
            project_root,
            deadline.slice(programs.command_timeout)?,
        )
    });
    let bridge = Some(
        deadline
            .slice(BRIDGE_PROBE_TIMEOUT)
            .map_or_else(BridgeProbeObservation::timed_out, |timeout| {
                probe_bridge(project_root, timeout)
            }),
    );
    let mcp = Some(
        deadline
            .slice(programs.mcp_timeout)
            .and_then(|timeout| {
                probe_mcp(
                    project_root,
                    protocol_version,
                    registry,
                    &programs.sidecar,
                    timeout,
                )
            })
            .unwrap_or_else(|| failed_mcp_observation(protocol_version)),
    );
    let mut observations = DoctorProbeObservations {
        host,
        godot,
        mcp,
        bridge,
    };
    if let Some(mcp) = observations.mcp.as_mut() {
        mcp.supports_form_elicitation = host_form;
    }
    observations
}

fn probe_bridge(project_root: &Path, timeout: Duration) -> BridgeProbeObservation {
    if timeout.is_zero() {
        return BridgeProbeObservation::timed_out();
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return BridgeProbeObservation::timed_out(),
    };
    match runtime.block_on(async {
        tokio::time::timeout(
            timeout,
            godot_codex_bridge_client::BridgeClient::connect(project_root),
        )
        .await
    }) {
        Ok(Ok(client)) => {
            BridgeProbeObservation::ready(client.negotiated_profile().protocol_version)
        }
        Ok(Err(error)) => BridgeProbeObservation::failed(&error),
        Err(_) => BridgeProbeObservation::timed_out(),
    }
}

fn probe_godot(
    spec: &GodotProbeSpec,
    cwd: &Path,
    timeout: Duration,
) -> Option<GodotProbeObservation> {
    let started = Instant::now();
    if !safe_coordinate(&spec.architecture)
        || !safe_coordinate(&spec.source_commit)
        || !safe_coordinate(&spec.build_id)
        || !spec
            .artifact_sha256
            .strip_prefix("sha256:")
            .is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
    {
        return None;
    }
    let executable = safe_executable(&spec.path)?;
    let architecture = macho_architecture(&executable).unwrap_or_else(|| "unknown".to_owned());
    let artifact_sha256 = sha256_file(&executable, MAX_GODOT_BYTES, started, timeout)
        .map(|digest| format!("sha256:{digest}"))?;
    let remaining = timeout.checked_sub(started.elapsed())?;
    let output = run_bounded(
        &executable,
        [OsString::from("--version")],
        Some(cwd),
        remaining,
    );
    let build_id = output
        .filter(|output| output.status.success())
        .and_then(|output| {
            strict_utf8(&output.stdout)
                .map(str::trim)
                .map(ToOwned::to_owned)
        })
        .filter(|value| safe_coordinate(value))
        .unwrap_or_else(|| "unavailable".to_owned());
    Some(GodotProbeObservation {
        architecture,
        // The package builder binds this source commit to the exact artifact
        // digest; the runtime executable has no separate source-commit API.
        source_commit: spec.source_commit.clone(),
        build_id,
        artifact_sha256,
    })
}

fn sha256_file(path: &Path, max: u64, started: Instant, timeout: Duration) -> Option<String> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > max {
        return None;
    }
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    let mut total = 0_u64;
    loop {
        if started.elapsed() >= timeout {
            return None;
        }
        let count = file.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > max {
            return None;
        }
        hasher.update(&buffer[..count]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

fn macho_architecture(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut header = [0_u8; 8];
    file.read_exact(&mut header).ok()?;
    let cpu_type = match &header[..4] {
        b"\xcf\xfa\xed\xfe" => u32::from_le_bytes(header[4..8].try_into().ok()?),
        b"\xfe\xed\xfa\xcf" => u32::from_be_bytes(header[4..8].try_into().ok()?),
        _ => return None,
    };
    match cpu_type {
        0x0100_000c => Some("arm64".to_owned()),
        0x0100_0007 => Some("x86_64".to_owned()),
        _ => None,
    }
}

fn probe_host(
    project_root: &Path,
    surface: SurfaceKind,
    programs: &LocalProbePrograms,
    expected_launcher: &Path,
    deadline: ProbeDeadline,
) -> Option<HostProbeObservation> {
    match surface {
        SurfaceKind::Cli => {
            let executable = programs.cli_codex.as_ref()?;
            let host_version = codex_version(
                executable,
                project_root,
                deadline.slice(programs.command_timeout)?,
            )?;
            Some(HostProbeObservation {
                surface,
                host_version,
                ide_host_version: None,
                effective_project_config: deadline.slice(programs.command_timeout).map(|timeout| {
                    effective_codex_config(executable, project_root, expected_launcher, timeout)
                }),
                restart_required: Some(false),
                supports_form_elicitation: deadline
                    .slice(programs.command_timeout)
                    .and_then(|timeout| probe_form_feature(executable, project_root, timeout)),
            })
        }
        SurfaceKind::App => {
            if plist_value(
                &programs.app_plutil,
                &programs.app_info_plist,
                "CFBundleIdentifier",
                deadline.slice(programs.command_timeout)?,
            )
            .as_deref()
                != Some(APP_BUNDLE_ID)
            {
                return None;
            }
            let host_version = plist_value(
                &programs.app_plutil,
                &programs.app_info_plist,
                "CFBundleShortVersionString",
                deadline.slice(programs.command_timeout)?,
            )
            .filter(|value| safe_coordinate(value))?;
            let executable = safe_executable(&programs.app_codex)?;
            Some(HostProbeObservation {
                surface,
                host_version,
                ide_host_version: None,
                effective_project_config: deadline.slice(programs.command_timeout).map(|timeout| {
                    effective_codex_config(&executable, project_root, expected_launcher, timeout)
                }),
                // No supported local API exposes whether an already-running
                // desktop process has reloaded the project layer.
                restart_required: None,
                supports_form_elicitation: deadline
                    .slice(programs.command_timeout)
                    .and_then(|timeout| probe_form_feature(&executable, project_root, timeout)),
            })
        }
        SurfaceKind::Ide => {
            let code = safe_executable(&programs.ide_code)?;
            let version = run_bounded(
                &code,
                [OsString::from("--version")],
                Some(project_root),
                deadline.slice(programs.command_timeout)?,
            )?;
            if !version.status.success() {
                return None;
            }
            let text = strict_utf8(&version.stdout)?;
            let mut lines = text.lines();
            let ide_host_version = lines.next()?.trim();
            if !safe_coordinate(ide_host_version) {
                return None;
            }
            let extensions = run_bounded(
                &code,
                [
                    OsString::from("--list-extensions"),
                    OsString::from("--show-versions"),
                ],
                Some(project_root),
                deadline.slice(programs.command_timeout)?,
            )?;
            if !extensions.status.success() {
                return None;
            }
            let extension_version =
                extension_version(strict_utf8(&extensions.stdout)?, "openai.chatgpt")?;
            let extension_root = run_bounded(
                &code,
                [
                    OsString::from("--locate-extension"),
                    OsString::from("openai.chatgpt"),
                ],
                Some(project_root),
                deadline.slice(programs.command_timeout)?,
            )
            .filter(|output| output.status.success())
            .and_then(|output| one_path(&output.stdout));
            let extension_codex = extension_root
                .as_deref()
                .and_then(extension_codex_path)
                .and_then(|path| safe_executable(&path));
            let effective = extension_codex.as_ref().and_then(|executable| {
                deadline.slice(programs.command_timeout).map(|timeout| {
                    effective_codex_config(executable, project_root, expected_launcher, timeout)
                })
            });
            let form = extension_codex.as_ref().and_then(|executable| {
                deadline
                    .slice(programs.command_timeout)
                    .and_then(|timeout| probe_form_feature(executable, project_root, timeout))
            });
            Some(HostProbeObservation {
                surface,
                host_version: extension_version,
                ide_host_version: Some(ide_host_version.to_owned()),
                effective_project_config: effective,
                // Extension install/version is observable; live extension
                // reload state is private to VS Code.
                restart_required: None,
                supports_form_elicitation: form,
            })
        }
        SurfaceKind::Cursor => None,
    }
}

fn plist_value(plutil: &Path, plist: &Path, key: &str, timeout: Duration) -> Option<String> {
    let executable = safe_executable(plutil)?;
    let output = run_bounded(
        &executable,
        [
            OsString::from("-extract"),
            OsString::from(key),
            OsString::from("raw"),
            OsString::from("-o"),
            OsString::from("-"),
            plist.as_os_str().to_owned(),
        ],
        plist.parent(),
        timeout,
    )?;
    if !output.status.success() {
        return None;
    }
    let value = strict_utf8(&output.stdout)?.trim();
    (value.len() <= 256 && !value.is_empty()).then(|| value.to_owned())
}

fn codex_version(executable: &Path, cwd: &Path, timeout: Duration) -> Option<String> {
    let executable = safe_executable(executable)?;
    let output = run_bounded(
        &executable,
        [OsString::from("--version")],
        Some(cwd),
        timeout,
    )?;
    if !output.status.success() {
        return None;
    }
    let text = strict_utf8(&output.stdout)?.trim();
    let value = text
        .strip_prefix("codex-cli ")
        .or_else(|| text.strip_prefix("codex "))?;
    safe_coordinate(value).then(|| value.to_owned())
}

fn probe_form_feature(executable: &Path, cwd: &Path, timeout: Duration) -> Option<bool> {
    let output = run_bounded(
        executable,
        [OsString::from("features"), OsString::from("list")],
        Some(cwd),
        timeout,
    )?;
    if !output.status.success() {
        return None;
    }
    let text = strict_utf8(&output.stdout)?;
    text.lines()
        .find_map(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            (fields.first().copied() == Some("tool_call_mcp_elicitation"))
                .then(|| fields.last().copied())
                .flatten()
        })
        .and_then(|value| match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        })
}

fn effective_codex_config(
    executable: &Path,
    project_root: &Path,
    expected_launcher: &Path,
    timeout: Duration,
) -> bool {
    let Some(output) = run_bounded(
        executable,
        [
            OsString::from("mcp"),
            OsString::from("get"),
            OsString::from("godot_editor"),
            OsString::from("--json"),
        ],
        Some(project_root),
        timeout,
    ) else {
        return false;
    };
    output.status.success()
        && serde_json::from_slice::<Value>(&output.stdout)
            .ok()
            .is_some_and(|value| effective_config_value(&value, expected_launcher, project_root))
}

fn effective_config_value(
    value: &Value,
    expected_launcher: &Path,
    expected_project_root: &Path,
) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(transport) = object.get("transport").and_then(Value::as_object) else {
        return false;
    };
    let Some(tools) = object.get("enabled_tools").and_then(Value::as_array) else {
        return false;
    };
    let tools = tools.iter().map(Value::as_str).collect::<Option<Vec<_>>>();
    let exact_tools =
        tools.as_deref() == Some(READ_ONLY_TOOLS) || tools.as_deref() == Some(FULL_BETA_TOOLS);
    let Some(expected_launcher) = expected_launcher.to_str() else {
        return false;
    };
    object.get("name").and_then(Value::as_str) == Some("godot_editor")
        && object.get("enabled").and_then(Value::as_bool) == Some(true)
        && object.get("startup_timeout_sec").and_then(Value::as_f64) == Some(10.0)
        && object.get("tool_timeout_sec").and_then(Value::as_f64) == Some(60.0)
        && transport.get("type").and_then(Value::as_str) == Some("stdio")
        && transport.get("command").and_then(Value::as_str) == Some(expected_launcher)
        && string_array(transport.get("args")) == Some(vec!["--project-root", "."])
        && transport.get("cwd").and_then(Value::as_str) == expected_project_root.to_str()
        && exact_tools
}

fn string_array(value: Option<&Value>) -> Option<Vec<&str>> {
    value?.as_array()?.iter().map(Value::as_str).collect()
}

fn extension_version(output: &str, identifier: &str) -> Option<String> {
    let prefix = format!("{identifier}@");
    let mut matches = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix(&prefix))
        .filter(|value| safe_coordinate(value));
    let value = matches.next()?.to_owned();
    matches.next().is_none().then_some(value)
}

fn one_path(bytes: &[u8]) -> Option<PathBuf> {
    let text = strict_utf8(bytes)?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let path = PathBuf::from(lines.next()?.trim());
    if lines.next().is_some()
        || !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES
    {
        return None;
    }
    fs::canonicalize(path).ok().filter(|path| path.is_dir())
}

fn extension_codex_path(root: &Path) -> Option<PathBuf> {
    let relative = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "bin/macos-aarch64/codex",
        ("macos", "x86_64") => "bin/macos-x86_64/codex",
        ("linux", "aarch64") => "bin/linux-arm64/codex",
        ("linux", "x86_64") => "bin/linux-x64/codex",
        ("windows", "aarch64") => "bin/windows-arm64/codex.exe",
        ("windows", "x86_64") => "bin/windows-x64/codex.exe",
        _ => return None,
    };
    Some(root.join(relative))
}

fn probe_mcp(
    project_root: &Path,
    protocol_version: &str,
    registry: &RegistryProfile,
    executable: &Path,
    timeout: Duration,
) -> Option<McpProbeObservation> {
    let executable = safe_executable(executable)?;
    let mut command = Command::new(executable);
    command
        .arg("--doctor-probe")
        .arg("--project-root")
        .arg(project_root)
        .current_dir(project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    isolate_child_process_group(&mut command);
    let mut child = command.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let (sender, receiver) = mpsc::sync_channel(4);
    let stdout_thread = thread::spawn(move || read_mcp_frames(stdout, sender));
    let stderr_thread = thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = stderr
            .take((MAX_COMMAND_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut sink);
    });
    let started = Instant::now();
    let result = (|| {
        let initialize = mcp_request(
            &mut stdin,
            &receiver,
            started,
            timeout,
            1,
            "initialize",
            json!({
                "protocolVersion": protocol_version,
                "capabilities": {
                    "elicitation": {
                        "form": {"schemaValidation": true}
                    }
                },
                "clientInfo": {
                    "name": "godot-codex-doctor",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        let initialize_result = response_result(&initialize)?;
        let negotiated = initialize_result
            .get("protocolVersion")
            .and_then(Value::as_str)?
            .to_owned();
        let instructions_present = initialize_result
            .get("instructions")
            .and_then(Value::as_str)
            .is_some_and(|instructions| {
                !instructions.is_empty()
                    && instructions.len() <= 4096
                    && instructions.contains("godot_get_connection_status")
            });
        write_frame(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
        )?;
        let tools_response = mcp_request(
            &mut stdin,
            &receiver,
            started,
            timeout,
            2,
            "tools/list",
            json!({}),
        )?;
        let tools = response_result(&tools_response)?.get("tools")?.as_array()?;
        let tool_names = exact_names(tools, "name")?;
        let status_listed = tool_names.contains("godot_get_connection_status");
        let resources_response = mcp_request(
            &mut stdin,
            &receiver,
            started,
            timeout,
            3,
            "resources/list",
            json!({}),
        )?;
        let resources = response_result(&resources_response)?
            .get("resources")?
            .as_array()?;
        let resource_names = exact_names(resources, "uri")?;
        let templates_response = mcp_request(
            &mut stdin,
            &receiver,
            started,
            timeout,
            4,
            "resources/templates/list",
            json!({}),
        )?;
        let templates = response_result(&templates_response)?
            .get("resourceTemplates")?
            .as_array()?;
        let template_names = exact_names(templates, "uriTemplate")?;
        let status = mcp_request(
            &mut stdin,
            &receiver,
            started,
            timeout,
            5,
            "tools/call",
            json!({
                "name": "godot_get_connection_status",
                "arguments": {}
            }),
        )?;
        let status_result = response_result(&status)?;
        let status_tool_available = status_listed
            && status_result.get("isError").and_then(Value::as_bool) != Some(true)
            && status_result
                .get("structuredContent")
                .and_then(Value::as_object)
                .is_some_and(|content| {
                    content.get("status").and_then(Value::as_str).is_some()
                        && content.get("package_version").and_then(Value::as_str)
                            == Some(env!("CARGO_PKG_VERSION"))
                });
        let registry_matches = tool_names
            == registry
                .tools
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
            && resource_names == FIXED_RESOURCE_URIS.iter().copied().collect::<BTreeSet<_>>()
            && template_names
                == RESOURCE_TEMPLATE_URIS
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
        let registry_digest = if registry_matches {
            registry.digest.clone()
        } else {
            observed_registry_digest(&tool_names, &resource_names, &template_names)
        };
        let project_id = godot_codex_bridge_client::project_id_for_path(project_root).ok()?;
        Some(McpProbeObservation {
            initialized: true,
            protocol_version: negotiated,
            project_id,
            status_tool_available,
            registry_digest,
            tool_count: tool_names.len(),
            fixed_resource_count: resource_names.len(),
            resource_template_count: template_names.len(),
            instructions_present,
            supports_form_elicitation: None,
        })
    })();
    drop(stdin);
    stop_child(&mut child, started, timeout);
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    result
}

fn failed_mcp_observation(protocol_version: &str) -> McpProbeObservation {
    McpProbeObservation {
        initialized: false,
        protocol_version: protocol_version.to_owned(),
        project_id: "unavailable".to_owned(),
        status_tool_available: false,
        registry_digest: "unavailable".to_owned(),
        tool_count: 0,
        fixed_resource_count: 0,
        resource_template_count: 0,
        instructions_present: false,
        supports_form_elicitation: None,
    }
}

fn mcp_request(
    stdin: &mut impl Write,
    receiver: &mpsc::Receiver<Result<Vec<u8>, ()>>,
    started: Instant,
    timeout: Duration,
    id: u64,
    method: &str,
    params: Value,
) -> Option<Value> {
    write_frame(
        stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }),
    )?;
    for _ in 0..MAX_MCP_FRAMES {
        let remaining = timeout.checked_sub(started.elapsed())?;
        let frame = receiver.recv_timeout(remaining).ok()?.ok()?;
        let value = serde_json::from_slice::<Value>(&frame).ok()?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            return Some(value);
        }
    }
    None
}

fn write_frame(writer: &mut impl Write, value: &Value) -> Option<()> {
    let bytes = serde_json::to_vec(value).ok()?;
    if bytes.len() > MAX_MCP_FRAME_BYTES {
        return None;
    }
    writer.write_all(&bytes).ok()?;
    writer.write_all(b"\n").ok()?;
    writer.flush().ok()
}

fn response_result(value: &Value) -> Option<&Value> {
    (value.get("error").is_none())
        .then(|| value.get("result"))
        .flatten()
}

fn exact_names<'a>(items: &'a [Value], member: &str) -> Option<BTreeSet<&'a str>> {
    if items.len() > 256 {
        return None;
    }
    let names = items
        .iter()
        .map(|item| item.get(member).and_then(Value::as_str))
        .collect::<Option<BTreeSet<_>>>()?;
    (names.len() == items.len() && names.iter().all(|name| safe_registry_name(name)))
        .then_some(names)
}

fn observed_registry_digest(
    tools: &BTreeSet<&str>,
    resources: &BTreeSet<&str>,
    templates: &BTreeSet<&str>,
) -> String {
    let bytes = serde_json::to_vec(&(tools, resources, templates))
        .expect("bounded registry name sets are serializable");
    format!("{:x}", Sha256::digest(bytes))
}

fn read_mcp_frames(stdout: impl Read, sender: mpsc::SyncSender<Result<Vec<u8>, ()>>) {
    let mut reader = BufReader::new(stdout);
    let mut total = 0_usize;
    for _ in 0..MAX_MCP_FRAMES {
        let mut frame = Vec::new();
        let Ok(bytes) = reader.read_until(b'\n', &mut frame) else {
            let _ = sender.send(Err(()));
            return;
        };
        if bytes == 0 {
            return;
        }
        total = total.saturating_add(bytes);
        if bytes > MAX_MCP_FRAME_BYTES || total > MAX_MCP_TOTAL_BYTES {
            let _ = sender.send(Err(()));
            return;
        }
        while frame
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            frame.pop();
        }
        if sender.send(Ok(frame)).is_err() {
            return;
        }
    }
    let _ = sender.send(Err(()));
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn run_bounded(
    executable: &Path,
    arguments: impl IntoIterator<Item = OsString>,
    cwd: Option<&Path>,
    timeout: Duration,
) -> Option<BoundedOutput> {
    let executable = safe_executable(executable)?;
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    isolate_child_process_group(&mut command);
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let stdout_thread = thread::spawn(move || read_limited(stdout, MAX_COMMAND_OUTPUT_BYTES));
    let stderr_thread = thread::spawn(move || read_limited(stderr, MAX_COMMAND_OUTPUT_BYTES));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < timeout => thread::sleep(COMMAND_POLL_INTERVAL),
            Ok(None) | Err(_) => {
                kill_child_process_group(&mut child);
                let _ = child.wait();
                break None;
            }
        }
    };
    // Diagnostic adapters are not allowed to retain descendants after their
    // direct child exits successfully. The isolated process group remains the
    // bounded cleanup authority even after the leader has been reaped.
    kill_child_process_group(&mut child);
    let stdout = stdout_thread.join().ok().flatten();
    let _ = stderr_thread.join();
    Some(BoundedOutput {
        status: status?,
        stdout: stdout?,
    })
}

fn read_limited(mut reader: impl Read, max: usize) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    reader
        .by_ref()
        .take((max + 1) as u64)
        .read_to_end(&mut output)
        .ok()?;
    (output.len() <= max).then_some(output)
}

fn stop_child(child: &mut Child, started: Instant, timeout: Duration) {
    if child.try_wait().ok().flatten().is_some() {
        kill_child_process_group(child);
        return;
    }
    let grace = timeout
        .checked_sub(started.elapsed())
        .unwrap_or_default()
        .min(Duration::from_millis(100));
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(COMMAND_POLL_INTERVAL);
    }
    kill_child_process_group(child);
    let _ = child.wait();
}

#[cfg(unix)]
fn isolate_child_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

#[cfg(not(unix))]
fn isolate_child_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn kill_child_process_group(child: &mut Child) {
    let Some(raw_pid) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        let _ = child.kill();
        return;
    };
    if rustix::process::kill_process_group(raw_pid, rustix::process::Signal::KILL).is_err() {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn kill_child_process_group(child: &mut Child) {
    let _ = child.kill();
}

fn safe_executable(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
        return None;
    }
    let canonical = fs::canonicalize(path).ok()?;
    let metadata = fs::metadata(&canonical).ok()?;
    (metadata.is_file() && is_executable(&canonical)).then_some(canonical)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .take(MAX_PATH_ENTRIES)
        .map(|directory| directory.join(name))
        .find_map(|candidate| safe_executable(&candidate))
}

fn detect_auto_surface() -> Option<SurfaceKind> {
    let bundle = std::env::var("__CFBundleIdentifier").ok();
    let origin = std::env::var("CODEX_INTERNAL_ORIGINATOR_OVERRIDE").ok();
    if bundle.as_deref() == Some(APP_BUNDLE_ID) && origin.as_deref() == Some("Codex Desktop") {
        Some(SurfaceKind::App)
    } else {
        // A shell invocation by itself does not identify whether the user
        // intends to diagnose CLI, App, or IDE. Do not guess CLI.
        None
    }
}

fn strict_utf8(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.contains('\0')).then_some(text)
}

fn safe_coordinate(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'_' | b'-' | b'+')
        })
}

fn safe_registry_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::io::{Read as _, Write as _};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::os::unix::net::{UnixListener, UnixStream};
    #[cfg(unix)]
    use std::thread::JoinHandle;

    #[cfg(unix)]
    use base64::Engine as _;
    #[cfg(unix)]
    use hmac::{Hmac, Mac};
    use tempfile::TempDir;

    use super::*;

    const TEST_LAUNCHER: &str = "/package/current/bin/godot-codex-mcp";

    #[cfg(unix)]
    type HmacSha256 = Hmac<Sha256>;

    #[cfg(unix)]
    #[derive(Clone, Copy)]
    enum TestBridgeMode {
        Ready {
            omitted_capability: Option<&'static str>,
        },
        WrongToken,
        ProjectMismatch,
        ProtocolMismatch,
        Hang,
    }

    #[cfg(unix)]
    struct TestBridge {
        project: TempDir,
        listener: Option<UnixListener>,
        file_token: [u8; 32],
        project_id: String,
        editor_session_id: String,
    }

    #[cfg(unix)]
    impl TestBridge {
        fn new() -> Self {
            let project = TempDir::new().unwrap();
            fs::write(project.path().join("project.godot"), "[application]\n").unwrap();
            let codex = project.path().join(".godot/codex");
            let run = codex.join("run");
            fs::create_dir_all(&run).unwrap();
            fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
            let endpoint_relative = ".godot/codex/run/b.sock";
            let endpoint = project.path().join(endpoint_relative);
            let listener = UnixListener::bind(&endpoint).unwrap();
            fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
            let file_token = [0x31_u8; 32];
            let project_id =
                godot_codex_bridge_client::project_id_for_path(project.path()).unwrap();
            let editor_session_id = format!("editor:{}", "a".repeat(32));
            let record = json!({
                "discovery_schema": 1,
                "created_at": "2026-07-25T00:00:00Z",
                "transport": "uds",
                "endpoint": endpoint_relative,
                "token_file": ".godot/codex/session.token",
                "project_id": project_id,
                "editor_session_id": editor_session_id,
                "pid": std::process::id(),
                "protocol_versions": ["1.8"],
            });
            fs::write(
                codex.join("bridge.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            fs::write(codex.join("session.token"), file_token).unwrap();
            fs::write(
                codex.join("bridge.lock"),
                serde_json::to_vec(&json!({
                    "editor_session_id": editor_session_id,
                    "pid": std::process::id(),
                }))
                .unwrap(),
            )
            .unwrap();
            for name in ["bridge.json", "session.token", "bridge.lock"] {
                fs::set_permissions(codex.join(name), fs::Permissions::from_mode(0o600)).unwrap();
            }
            Self {
                project,
                listener: Some(listener),
                file_token,
                project_id,
                editor_session_id,
            }
        }

        fn spawn(&mut self, mode: TestBridgeMode) -> JoinHandle<()> {
            let listener = self.listener.take().unwrap();
            let file_token = self.file_token;
            let project_id = self.project_id.clone();
            let editor_session_id = self.editor_session_id.clone();
            std::thread::spawn(move || {
                serve_test_bridge(listener, mode, file_token, &project_id, &editor_session_id);
            })
        }

        fn drop_listener(&mut self) {
            drop(self.listener.take());
        }

        fn retained_files(&self) -> BTreeMap<&'static str, Vec<u8>> {
            let codex = self.project.path().join(".godot/codex");
            [
                ("project.godot", self.project.path().join("project.godot")),
                ("bridge.json", codex.join("bridge.json")),
                ("session.token", codex.join("session.token")),
                ("bridge.lock", codex.join("bridge.lock")),
            ]
            .into_iter()
            .map(|(name, path)| (name, fs::read(path).unwrap()))
            .collect()
        }
    }

    #[cfg(unix)]
    fn serve_test_bridge(
        listener: UnixListener,
        mode: TestBridgeMode,
        file_token: [u8; 32],
        project_id: &str,
        editor_session_id: &str,
    ) {
        if listener.set_nonblocking(true).is_err() {
            return;
        }
        let accept_deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < accept_deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        };
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        if matches!(mode, TestBridgeMode::Hang) {
            std::thread::sleep(Duration::from_millis(250));
            return;
        }
        let Some(hello) = read_bridge_frame(&mut stream) else {
            return;
        };
        let Some(client_nonce) = hello
            .get("client_nonce")
            .and_then(Value::as_str)
            .and_then(decode_bridge_nonce)
        else {
            return;
        };
        let offered = hello
            .get("supported_protocol_versions")
            .and_then(Value::as_array)
            .and_then(|values| {
                values
                    .iter()
                    .map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .unwrap_or_default();
        let selected = if matches!(mode, TestBridgeMode::ProtocolMismatch) {
            "2.0"
        } else {
            "1.8"
        };
        let challenge_project = if matches!(mode, TestBridgeMode::ProjectMismatch) {
            "project:sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        } else {
            project_id
        };
        let server_nonce = [0x52_u8; 32];
        let server_token = if matches!(mode, TestBridgeMode::WrongToken) {
            [0x93_u8; 32]
        } else {
            file_token
        };
        let transcript = bridge_handshake_transcript(
            &offered,
            selected,
            project_id,
            editor_session_id,
            &client_nonce,
            &server_nonce,
        );
        let challenge = json!({
            "handshake_version": "1.0",
            "kind": "handshake.server_challenge",
            "selected_protocol_version": selected,
            "project_id": challenge_project,
            "editor_session_id": editor_session_id,
            "server_nonce": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(server_nonce),
            "server_proof": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                bridge_proof(true, &server_token, &transcript)
            ),
        });
        if write_bridge_frame(&mut stream, &challenge).is_none()
            || !matches!(
                mode,
                TestBridgeMode::Ready { .. }
                    | TestBridgeMode::WrongToken
                    | TestBridgeMode::ProjectMismatch
            )
        {
            return;
        }
        let Some(authenticate) = read_bridge_frame(&mut stream) else {
            return;
        };
        let Some(client_proof) = authenticate
            .get("client_proof")
            .and_then(Value::as_str)
            .and_then(decode_bridge_nonce)
        else {
            return;
        };
        if client_proof != bridge_proof(false, &file_token, &transcript) {
            return;
        }
        let ready = json!({
            "handshake_version": "1.0",
            "kind": "handshake.server_ready",
            "selected_protocol_version": selected,
            "project_id": project_id,
            "editor_session_id": editor_session_id,
        });
        if write_bridge_frame(&mut stream, &ready).is_none() {
            return;
        }
        let Some(initialize) = read_bridge_frame(&mut stream) else {
            return;
        };
        let omitted = match mode {
            TestBridgeMode::Ready { omitted_capability } => omitted_capability,
            _ => None,
        };
        let capabilities = initialize
            .pointer("/params/requested_capabilities")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|name| Some(*name) != omitted)
                    .map(|name| json!({"name": name, "readiness": "ready"}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let response = json!({
            "protocol_version": selected,
            "kind": "response",
            "request_id": initialize.get("request_id").cloned().unwrap_or(Value::Null),
            "result": {"capabilities": capabilities},
            "context": {
                "project_id": project_id,
                "editor_session_id": editor_session_id,
            },
        });
        let _ = write_bridge_frame(&mut stream, &response);
    }

    #[cfg(unix)]
    fn read_bridge_frame(stream: &mut UnixStream) -> Option<Value> {
        let mut prefix = [0_u8; 4];
        stream.read_exact(&mut prefix).ok()?;
        let length = usize::try_from(u32::from_be_bytes(prefix)).ok()?;
        if !(1..=1_048_576).contains(&length) {
            return None;
        }
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload).ok()?;
        serde_json::from_slice(&payload).ok()
    }

    #[cfg(unix)]
    fn write_bridge_frame(stream: &mut UnixStream, value: &Value) -> Option<()> {
        let payload = serde_json::to_vec(value).ok()?;
        let length = u32::try_from(payload.len()).ok()?;
        stream.write_all(&length.to_be_bytes()).ok()?;
        stream.write_all(&payload).ok()?;
        stream.flush().ok()
    }

    #[cfg(unix)]
    fn decode_bridge_nonce(value: &str) -> Option<[u8; 32]> {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .ok()?
            .try_into()
            .ok()
    }

    #[cfg(unix)]
    fn append_bridge_field(output: &mut Vec<u8>, value: &[u8]) {
        output.extend_from_slice(&u32::try_from(value.len()).unwrap().to_be_bytes());
        output.extend_from_slice(value);
    }

    #[cfg(unix)]
    fn bridge_handshake_transcript(
        offered: &[String],
        selected: &str,
        project_id: &str,
        editor_session_id: &str,
        client_nonce: &[u8; 32],
        server_nonce: &[u8; 32],
    ) -> Vec<u8> {
        let mut output = b"godot-codex-bridge/handshake-transcript/v1\0".to_vec();
        append_bridge_field(&mut output, b"1.0");
        output.extend_from_slice(&u32::try_from(offered.len()).unwrap().to_be_bytes());
        for version in offered {
            append_bridge_field(&mut output, version.as_bytes());
        }
        append_bridge_field(&mut output, selected.as_bytes());
        append_bridge_field(&mut output, project_id.as_bytes());
        append_bridge_field(&mut output, editor_session_id.as_bytes());
        append_bridge_field(&mut output, client_nonce);
        append_bridge_field(&mut output, server_nonce);
        output
    }

    #[cfg(unix)]
    fn bridge_proof(server: bool, token: &[u8; 32], transcript: &[u8]) -> [u8; 32] {
        let domain = if server {
            b"godot-codex-bridge/server-proof/v1\0".as_slice()
        } else {
            b"godot-codex-bridge/client-proof/v1\0".as_slice()
        };
        let mut hmac = HmacSha256::new_from_slice(token).unwrap();
        hmac.update(domain);
        hmac.update(transcript);
        hmac.finalize().into_bytes().into()
    }

    #[cfg(unix)]
    fn executable(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cli_adapter_uses_real_local_commands_and_exact_effective_projection() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("project.godot"), "[application]\n").unwrap();
        let tools = READ_ONLY_TOOLS
            .iter()
            .map(|tool| Value::String((*tool).to_owned()))
            .collect::<Vec<_>>();
        let config = serde_json::to_string(&json!({
            "name": "godot_editor",
            "enabled": true,
            "transport": {
                "type": "stdio",
                "command": TEST_LAUNCHER,
                "args": ["--project-root", "."],
                "cwd": temp.path()
            },
            "enabled_tools": tools,
            "startup_timeout_sec": 10.0,
            "tool_timeout_sec": 60.0
        }))
        .unwrap();
        let codex = temp.path().join("codex");
        executable(
            &codex,
            &format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                   --version) printf '%s\\n' 'codex-cli 0.145.0-alpha.30' ;;\n\
                   features) printf '%s\\n' 'tool_call_mcp_elicitation stable true' ;;\n\
                   mcp) printf '%s\\n' '{config}' ;;\n\
                   *) exit 2 ;;\n\
                 esac\n"
            ),
        );
        let programs = LocalProbePrograms {
            auto_surface: None,
            cli_codex: Some(codex),
            app_plutil: PathBuf::from("/usr/bin/false"),
            app_info_plist: temp.path().join("Info.plist"),
            app_codex: PathBuf::from("/usr/bin/false"),
            ide_code: PathBuf::from("/usr/bin/false"),
            sidecar: PathBuf::from("/usr/bin/false"),
            command_timeout: LOCAL_COMMAND_TIMEOUT,
            mcp_timeout: Duration::from_secs(1),
            total_timeout: Duration::from_secs(3),
        };
        let observed = probe_host(
            temp.path(),
            SurfaceKind::Cli,
            &programs,
            Path::new(TEST_LAUNCHER),
            ProbeDeadline::new(programs.total_timeout),
        )
        .unwrap();
        assert_eq!(observed.host_version, "0.145.0-alpha.30");
        assert_eq!(observed.effective_project_config, Some(true));
        assert_eq!(observed.restart_required, Some(false));
        assert_eq!(observed.supports_form_elicitation, Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn ide_adapter_binds_vscode_and_official_extension_coordinates() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("project.godot"), "[application]\n").unwrap();
        let extension_root = temp.path().join("openai.chatgpt");
        let embedded_codex = extension_codex_path(&extension_root).unwrap();
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(
            embedded_codex,
            extension_root.join("bin/macos-aarch64/codex")
        );
        fs::create_dir_all(embedded_codex.parent().unwrap()).unwrap();
        let tools = READ_ONLY_TOOLS
            .iter()
            .map(|tool| Value::String((*tool).to_owned()))
            .collect::<Vec<_>>();
        let config = serde_json::to_string(&json!({
            "name": "godot_editor",
            "enabled": true,
            "transport": {
                "type": "stdio",
                "command": TEST_LAUNCHER,
                "args": ["--project-root", "."],
                "cwd": temp.path()
            },
            "enabled_tools": tools,
            "startup_timeout_sec": 10.0,
            "tool_timeout_sec": 60.0
        }))
        .unwrap();
        executable(
            &embedded_codex,
            &format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                   features) printf '%s\\n' 'tool_call_mcp_elicitation stable true' ;;\n\
                   mcp) printf '%s\\n' '{config}' ;;\n\
                   *) exit 2 ;;\n\
                 esac\n"
            ),
        );
        let code = temp.path().join("code");
        executable(
            &code,
            &format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                   --version) printf '%s\\n' '1.127.0' 'commit' 'arm64' ;;\n\
                   --list-extensions) printf '%s\\n' 'openai.chatgpt@26.721.30844' ;;\n\
                   --locate-extension) printf '%s\\n' '{}' ;;\n\
                   *) exit 2 ;;\n\
                 esac\n",
                extension_root.display()
            ),
        );
        let programs = LocalProbePrograms {
            auto_surface: None,
            cli_codex: None,
            app_plutil: PathBuf::from("/usr/bin/false"),
            app_info_plist: temp.path().join("Info.plist"),
            app_codex: PathBuf::from("/usr/bin/false"),
            ide_code: code,
            sidecar: PathBuf::from("/usr/bin/false"),
            command_timeout: Duration::from_secs(3),
            mcp_timeout: Duration::from_secs(3),
            total_timeout: Duration::from_secs(15),
        };
        let observed = probe_host(
            temp.path(),
            SurfaceKind::Ide,
            &programs,
            Path::new(TEST_LAUNCHER),
            ProbeDeadline::new(programs.total_timeout),
        )
        .unwrap();
        assert_eq!(observed.host_version, "26.721.30844");
        assert_eq!(observed.ide_host_version.as_deref(), Some("1.127.0"));
        assert_eq!(observed.effective_project_config, Some(true));
        assert_eq!(observed.restart_required, None);
        assert_eq!(observed.supports_form_elicitation, Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn app_adapter_reads_bundle_coordinate_without_claiming_live_restart_state() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("project.godot"), "[application]\n").unwrap();
        let tools = READ_ONLY_TOOLS
            .iter()
            .map(|tool| Value::String((*tool).to_owned()))
            .collect::<Vec<_>>();
        let config = serde_json::to_string(&json!({
            "name": "godot_editor",
            "enabled": true,
            "transport": {
                "type": "stdio",
                "command": TEST_LAUNCHER,
                "args": ["--project-root", "."],
                "cwd": temp.path()
            },
            "enabled_tools": tools,
            "startup_timeout_sec": 10.0,
            "tool_timeout_sec": 60.0
        }))
        .unwrap();
        let codex = temp.path().join("app-codex");
        executable(
            &codex,
            &format!(
                "#!/bin/sh\n\
                 case \"$1\" in\n\
                   features) printf '%s\\n' 'tool_call_mcp_elicitation stable true' ;;\n\
                   mcp) printf '%s\\n' '{config}' ;;\n\
                   *) exit 2 ;;\n\
                 esac\n"
            ),
        );
        let plutil = temp.path().join("plutil");
        executable(
            &plutil,
            "#!/bin/sh\n\
             case \"$2\" in\n\
               CFBundleIdentifier) printf '%s\\n' 'com.openai.codex' ;;\n\
               CFBundleShortVersionString) printf '%s\\n' '26.721.41059' ;;\n\
               *) exit 2 ;;\n\
             esac\n",
        );
        let programs = LocalProbePrograms {
            auto_surface: Some(SurfaceKind::App),
            cli_codex: None,
            app_plutil: plutil,
            app_info_plist: temp.path().join("Info.plist"),
            app_codex: codex,
            ide_code: PathBuf::from("/usr/bin/false"),
            sidecar: PathBuf::from("/usr/bin/false"),
            command_timeout: LOCAL_COMMAND_TIMEOUT,
            mcp_timeout: Duration::from_secs(1),
            // Match the production aggregate budget. This test launches four
            // bounded child probes and must remain deterministic when the
            // full Rust suite runs them alongside other process tests.
            total_timeout: TOTAL_PROBE_TIMEOUT,
        };
        let observed = probe_host(
            temp.path(),
            SurfaceKind::App,
            &programs,
            Path::new(TEST_LAUNCHER),
            ProbeDeadline::new(programs.total_timeout),
        )
        .unwrap();
        assert_eq!(observed.host_version, "26.721.41059");
        assert_eq!(observed.effective_project_config, Some(true));
        assert_eq!(observed.restart_required, None);
        assert_eq!(observed.supports_form_elicitation, Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn local_command_deadline_kills_a_hung_probe_without_exposing_output() {
        let temp = TempDir::new().unwrap();
        let hung = temp.path().join("hung");
        executable(
            &hung,
            "#!/bin/sh\nprintf '%s\\n' '/private/secret' >&2\nsleep 5\n",
        );
        let started = Instant::now();
        assert!(
            run_bounded(
                &hung,
                std::iter::empty(),
                Some(temp.path()),
                Duration::from_millis(50)
            )
            .is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn authenticated_bridge_probe_distinguishes_ready_auth_binding_version_and_refused() {
        let mut ready = TestBridge::new();
        let before = ready.retained_files();
        let ready_server = ready.spawn(TestBridgeMode::Ready {
            omitted_capability: None,
        });
        let observation = probe_bridge(ready.project.path(), Duration::from_secs(1));
        ready_server.join().unwrap();
        assert_eq!(observation.status, crate::BridgeProbeStatus::Ready);
        assert_eq!(observation.negotiated_protocol.as_deref(), Some("1.8"));
        assert_eq!(ready.retained_files(), before);

        let mut auth = TestBridge::new();
        assert_eq!(
            fs::read(auth.project.path().join(".godot/codex/session.token"))
                .unwrap()
                .len(),
            32,
            "the negative must use a wrong exact-length token"
        );
        let auth_server = auth.spawn(TestBridgeMode::WrongToken);
        let observation = probe_bridge(auth.project.path(), Duration::from_secs(1));
        auth_server.join().unwrap();
        assert_eq!(
            observation.status,
            crate::BridgeProbeStatus::AuthenticationFailed
        );

        let mut binding = TestBridge::new();
        let binding_server = binding.spawn(TestBridgeMode::ProjectMismatch);
        let observation = probe_bridge(binding.project.path(), Duration::from_secs(1));
        binding_server.join().unwrap();
        assert_eq!(
            observation.status,
            crate::BridgeProbeStatus::ProjectBindingMismatch
        );

        let mut version = TestBridge::new();
        let version_server = version.spawn(TestBridgeMode::ProtocolMismatch);
        let observation = probe_bridge(version.project.path(), Duration::from_secs(1));
        version_server.join().unwrap();
        assert_eq!(
            observation.status,
            crate::BridgeProbeStatus::ProtocolVersionMismatch
        );

        let mut refused = TestBridge::new();
        refused.drop_listener();
        let observation = probe_bridge(refused.project.path(), Duration::from_millis(250));
        assert_eq!(observation.status, crate::BridgeProbeStatus::Unreachable);
    }

    #[cfg(unix)]
    #[test]
    fn bridge_probe_has_one_outer_deadline_for_a_hung_authenticated_endpoint() {
        let mut bridge = TestBridge::new();
        let server = bridge.spawn(TestBridgeMode::Hang);
        let started = Instant::now();
        let observation = probe_bridge(bridge.project.path(), Duration::from_millis(50));
        assert_eq!(observation.status, crate::BridgeProbeStatus::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn production_connect_rejects_missing_resource_script_and_runtime_read_capabilities() {
        for omitted in [
            "resource.uid_dependencies",
            "script.gdscript_semantics",
            "runtime.debugger",
        ] {
            let mut bridge = TestBridge::new();
            let server = bridge.spawn(TestBridgeMode::Ready {
                omitted_capability: Some(omitted),
            });
            let observation = probe_bridge(bridge.project.path(), Duration::from_secs(1));
            server.join().unwrap();
            assert_eq!(
                observation.status,
                crate::BridgeProbeStatus::ProtocolVersionMismatch,
                "missing {omitted} did not fail the production handshake"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn bounded_command_timeout_terminates_descendants_in_the_isolated_process_group() {
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("descendant");
        let marker = temp.path().join("orphan-marker");
        executable(
            &script,
            "#!/bin/sh\n( sleep 0.3; printf child > \"$1\" ) &\nsleep 5\n",
        );
        assert!(
            run_bounded(
                &script,
                [marker.clone().into_os_string()],
                Some(temp.path()),
                Duration::from_millis(50),
            )
            .is_none()
        );
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            !marker.exists(),
            "a timed-out doctor adapter left a descendant alive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_command_cleans_descendants_even_after_the_group_leader_exits_successfully() {
        let temp = TempDir::new().unwrap();
        let script = temp.path().join("early-exit");
        let marker = temp.path().join("orphan-marker");
        executable(
            &script,
            "#!/bin/sh\n( sleep 0.3; printf child > \"$1\" ) &\nexit 0\n",
        );
        let output = run_bounded(
            &script,
            [marker.clone().into_os_string()],
            Some(temp.path()),
            Duration::from_secs(3),
        )
        .expect("the group leader itself exited successfully");
        assert!(output.status.success());
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            !marker.exists(),
            "a successful doctor adapter left a descendant alive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn aggregate_probe_budget_does_not_multiply_per_child() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("project.godot"), "[application]\n").unwrap();
        let hung = temp.path().join("codex");
        executable(&hung, "#!/bin/sh\nsleep 5\n");
        let programs = LocalProbePrograms {
            auto_surface: None,
            cli_codex: Some(hung),
            app_plutil: PathBuf::from("/usr/bin/false"),
            app_info_plist: temp.path().join("Info.plist"),
            app_codex: PathBuf::from("/usr/bin/false"),
            ide_code: PathBuf::from("/usr/bin/false"),
            sidecar: PathBuf::from("/usr/bin/false"),
            command_timeout: Duration::from_secs(5),
            mcp_timeout: Duration::from_secs(5),
            total_timeout: Duration::from_millis(100),
        };
        let registry = godot_codex_product::canonical_registry_profile();
        let started = Instant::now();
        let observed = collect_local_observations(
            temp.path(),
            SurfaceSelection::Cli,
            "2025-11-25",
            &registry,
            None,
            &programs,
            Path::new(TEST_LAUNCHER),
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(observed.host.is_none());
        assert_eq!(
            observed.mcp.as_ref().map(|probe| probe.initialized),
            Some(false)
        );
        assert_eq!(
            observed.bridge.as_ref().map(|probe| probe.status),
            Some(crate::BridgeProbeStatus::TimedOut)
        );
    }

    #[cfg(unix)]
    #[test]
    fn mcp_adapter_probes_initialize_registry_instructions_and_status() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("project.godot"), "[application]\n").unwrap();
        let registry = godot_codex_product::canonical_registry_profile();
        let script = temp.path().join("fake-sidecar");
        let tools = serde_json::to_string(
            &registry
                .tools
                .iter()
                .map(|name| json!({"name": name}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let resources = serde_json::to_string(
            &registry
                .fixed_resources
                .iter()
                .map(|uri| json!({"uri": uri}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let templates = serde_json::to_string(
            &registry
                .resource_templates
                .iter()
                .map(|uri| json!({"uriTemplate": uri}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        executable(
            &script,
            &format!(
                r#"#!/usr/bin/env python3
import json
import sys
TOOLS = {tools}
RESOURCES = {resources}
TEMPLATES = {templates}
for raw in sys.stdin:
    request = json.loads(raw)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {{
            "protocolVersion": "2025-11-25",
            "serverInfo": {{"name": "fake", "version": "1"}},
            "capabilities": {{"tools": {{}}, "resources": {{}}}},
            "instructions": "Call godot_get_connection_status first."
        }}
    elif method == "tools/list":
        result = {{"tools": TOOLS}}
    elif method == "resources/list":
        result = {{"resources": RESOURCES}}
    elif method == "resources/templates/list":
        result = {{"resourceTemplates": TEMPLATES}}
    elif method == "tools/call":
        result = {{
            "content": [],
            "structuredContent": {{
                "status": "offline_empty",
                "package_version": "{version}"
            }},
            "isError": False
        }}
    else:
        result = {{}}
    print(json.dumps({{"jsonrpc": "2.0", "id": request["id"], "result": result}}), flush=True)
"#,
                version = env!("CARGO_PKG_VERSION"),
            ),
        );
        let observed = probe_mcp(
            temp.path(),
            "2025-11-25",
            &registry,
            &script,
            MCP_PROBE_TIMEOUT,
        )
        .unwrap();
        assert!(observed.initialized);
        assert!(observed.status_tool_available);
        assert!(observed.instructions_present);
        assert_eq!(observed.registry_digest, registry.digest);
        assert_eq!(observed.tool_count, 41);
        assert_eq!(observed.fixed_resource_count, 4);
        assert_eq!(observed.resource_template_count, 1);
        assert!(
            !temp.path().join(".godot/codex/bridge.json").exists(),
            "the stdio probe must not synthesize Bridge discovery"
        );
    }

    #[test]
    fn effective_config_requires_the_exact_owned_projection() {
        let tools = READ_ONLY_TOOLS
            .iter()
            .map(|tool| Value::String((*tool).to_owned()))
            .collect::<Vec<_>>();
        let value = json!({
            "name": "godot_editor",
            "enabled": true,
            "transport": {
                "type": "stdio",
                "command": TEST_LAUNCHER,
                "args": ["--project-root", "."],
                "cwd": "/project/root"
            },
            "enabled_tools": tools,
            "startup_timeout_sec": 10.0,
            "tool_timeout_sec": 60.0
        });
        assert!(effective_config_value(
            &value,
            Path::new(TEST_LAUNCHER),
            Path::new("/project/root")
        ));
        let mut wrong = value;
        wrong["transport"]["cwd"] = Value::String(".".to_owned());
        assert!(!effective_config_value(
            &wrong,
            Path::new(TEST_LAUNCHER),
            Path::new("/project/root")
        ));
        let basename = json!({
            "name": "godot_editor",
            "enabled": true,
            "transport": {
                "type": "stdio",
                "command": "godot-codex-mcp",
                "args": ["--project-root", "."],
                "cwd": "/project/root"
            },
            "enabled_tools": READ_ONLY_TOOLS,
            "startup_timeout_sec": 10.0,
            "tool_timeout_sec": 60.0
        });
        assert!(!effective_config_value(
            &basename,
            Path::new(TEST_LAUNCHER),
            Path::new("/project/root")
        ));
    }

    #[test]
    fn auto_surface_never_defaults_to_cli() {
        assert!(detect_auto_surface().is_none() || detect_auto_surface() == Some(SurfaceKind::App));
    }

    #[test]
    fn extension_coordinate_is_exact_and_unique() {
        assert_eq!(
            extension_version(
                "publisher.other@1.0\nopenai.chatgpt@26.721.30844\n",
                "openai.chatgpt"
            )
            .as_deref(),
            Some("26.721.30844")
        );
        assert!(
            extension_version("openai.chatgpt@1.0\nopenai.chatgpt@2.0\n", "openai.chatgpt")
                .is_none()
        );
    }

    #[test]
    fn registry_mismatch_digest_cannot_equal_the_canonical_digest() {
        let registry = godot_codex_product::canonical_registry_profile();
        let tools = ["unexpected"].into_iter().collect();
        let resources = BTreeSet::new();
        let templates = BTreeSet::new();
        assert_ne!(
            observed_registry_digest(&tools, &resources, &templates),
            registry.digest
        );
    }
}
