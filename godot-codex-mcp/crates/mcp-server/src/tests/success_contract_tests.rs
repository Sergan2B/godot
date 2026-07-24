#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use base64::Engine as _;
use godot_codex_bridge_client::{
    AffectedEntity, AffectedEntityRole, ApprovalBinding, ApprovalScope, BridgeError, DirtyEffect,
    PrepareResult, Risk, SaveEffect, TransactionCoordinates, TransactionLimits,
    TransactionOperation, TransactionOutcome, TransactionPreview, TransactionState,
    TransactionStatus, UndoEligibility, UndoEligibilityReason,
};
use godot_codex_transactions::{
    ApplyCommand, BridgeApplyFailure, BridgeApplyOutcome, BridgeFuture, CheckAuthority,
    CheckOutcome, ObservedPrepare, ObservedStatus, PrepareCommand, TransactionBridge,
    TransactionClock, TransactionCoordinator, UndoCommand, ValidationCheck, ValidationCoordinator,
    ValidationPolicy,
};
use hmac::{Hmac, Mac};
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, ElicitRequestParams, ElicitResult,
    ElicitationAction, ElicitationCapability, FormElicitationCapability, Implementation,
};
use rmcp::service::RequestContext;
use rmcp::{ClientHandler, ErrorData as McpError, RoleClient, ServiceExt};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::*;

const EDITOR_SESSION_ID: &str = "editor:0123456789abcdef0123456789abcdef";
const SCENE_ID: &str = "scene:0123456789abcdef0123456789abcdef";
const HISTORY_ID: &str = "history:11111111111111111111111111111111";
const NODE_ID: &str = "node:22222222222222222222222222222222";
const OTHER_NODE_ID: &str = "node:33333333333333333333333333333333";
const RUNTIME_SESSION_ID: &str = "runtime:22222222222222222222222222222222";
const RUNTIME_OBJECT_ID: &str = "runtime-object:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const RUNTIME_STACK_ID: &str = "runtime-stack:CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC";

type HmacSha256 = Hmac<Sha256>;

struct FixedClock;

impl TransactionClock for FixedClock {
    fn now_ms(&self) -> u64 {
        2_000
    }
}

struct TransactionFixtureBridge {
    project_id: String,
    editor_session_id: String,
    next_transaction: AtomicUsize,
    statuses: Mutex<BTreeMap<String, TransactionStatus>>,
}

impl TransactionFixtureBridge {
    fn new(project_id: String, editor_session_id: String) -> Self {
        Self {
            project_id,
            editor_session_id,
            next_transaction: AtomicUsize::new(1),
            statuses: Mutex::new(BTreeMap::new()),
        }
    }
}

impl TransactionBridge for TransactionFixtureBridge {
    fn prepare<'a>(
        &'a self,
        command: &'a PrepareCommand,
    ) -> BridgeFuture<'a, Result<ObservedPrepare, BridgeError>> {
        Box::pin(async move {
            let transaction_seq = self.next_transaction.fetch_add(1, Ordering::SeqCst);
            let transaction_id = format!("transaction:{transaction_seq:032x}");
            let operation_kind = command.operation.kind();
            let (risk, scope, affected_entity) = operation_profile(&command.operation);
            let summary = format!("Prepare {operation_kind:?}");
            let preview_payload_json = json!({
                "operation_kind": operation_kind,
                "summary": summary,
            })
            .to_string();
            let preview_digest = format!(
                "sha256:{:x}",
                Sha256::digest(preview_payload_json.as_bytes())
            );
            let coordinates = TransactionCoordinates {
                transaction_id: transaction_id.clone(),
                scene_id: command.coordinates.scene_id.clone(),
                history_id: command.coordinates.history_id.clone(),
                scene_revision: command.coordinates.scene_revision,
                operation_seq: command.coordinates.operation_seq,
                transaction_seq: transaction_seq as u64,
            };
            let limits = transaction_limits();
            let prepare = PrepareResult {
                schema_version: "transaction/1.0".to_owned(),
                coordinates: coordinates.clone(),
                state: TransactionState::Previewed,
                operation_kind,
                risk,
                scope,
                affected_entities: vec![affected_entity],
                preview: TransactionPreview {
                    operation_kind,
                    summary,
                    dirty_effect: DirtyEffect::MarksSceneDirty,
                    save_effect: SaveEffect::NotSaved,
                    preconditions: vec!["The exact scene revision is still current.".to_owned()],
                    truncated: false,
                },
                preview_payload_json,
                preview_digest: preview_digest.clone(),
                created_at_ms: 1_000,
                expires_at_ms: 301_000,
                limits_applied: limits.clone(),
            };
            self.statuses.lock().unwrap().insert(
                transaction_id,
                TransactionStatus {
                    schema_version: "transaction/1.0".to_owned(),
                    coordinates,
                    state: TransactionState::Previewed,
                    operation_kind,
                    risk,
                    scope,
                    preview_digest,
                    current_scene_revision: command.coordinates.scene_revision,
                    current_operation_seq: command.coordinates.operation_seq,
                    outcome: Some(TransactionOutcome::None),
                    error: None,
                    undo_eligibility: UndoEligibility {
                        eligible: false,
                        reason: UndoEligibilityReason::NotCommitted,
                    },
                    committed_entities: None,
                    updated_at_ms: 1_000,
                    limits_applied: limits,
                    truncated: false,
                },
            );
            Ok(ObservedPrepare {
                project_id: self.project_id.clone(),
                editor_session_id: self.editor_session_id.clone(),
                result: prepare,
            })
        })
    }

    fn status<'a>(
        &'a self,
        transaction_id: &'a str,
    ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>> {
        Box::pin(async move {
            let status = self
                .statuses
                .lock()
                .unwrap()
                .get(transaction_id)
                .cloned()
                .ok_or_else(|| BridgeError::Rpc {
                    code: "transaction_not_found".to_owned(),
                    message: "The transaction fixture is not retained.".to_owned(),
                    retryable: false,
                    data: json!({}),
                })?;
            Ok(ObservedStatus {
                project_id: self.project_id.clone(),
                editor_session_id: self.editor_session_id.clone(),
                result: status,
            })
        })
    }

    fn apply_approved<'a>(
        &'a self,
        command: &'a ApplyCommand,
        _binding: &'a ApprovalBinding,
        _issued_at_ms: u64,
    ) -> BridgeFuture<'a, Result<BridgeApplyOutcome, BridgeApplyFailure>> {
        Box::pin(async move {
            let mut statuses = self.statuses.lock().unwrap();
            let status = statuses
                .get_mut(&command.transaction_id)
                .expect("prepared transaction");
            status.state = TransactionState::Committed;
            status.outcome = Some(TransactionOutcome::Committed);
            status.undo_eligibility = UndoEligibility {
                eligible: true,
                reason: UndoEligibilityReason::Eligible,
            };
            status.updated_at_ms = 2_000;
            Ok(BridgeApplyOutcome {
                observed: ObservedStatus {
                    project_id: self.project_id.clone(),
                    editor_session_id: self.editor_session_id.clone(),
                    result: status.clone(),
                },
                receipt_hash: format!("sha256:{}", "9".repeat(64)),
            })
        })
    }

    fn undo<'a>(
        &'a self,
        command: &'a UndoCommand,
    ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>> {
        Box::pin(async move {
            let mut statuses = self.statuses.lock().unwrap();
            let status = statuses
                .get_mut(&command.transaction_id)
                .expect("committed transaction");
            status.state = TransactionState::Undone;
            status.outcome = Some(TransactionOutcome::Undone);
            status.undo_eligibility = UndoEligibility {
                eligible: false,
                reason: UndoEligibilityReason::NotCommitted,
            };
            status.updated_at_ms = 3_000;
            Ok(ObservedStatus {
                project_id: self.project_id.clone(),
                editor_session_id: self.editor_session_id.clone(),
                result: status.clone(),
            })
        })
    }
}

fn operation_profile(operation: &TransactionOperation) -> (Risk, ApprovalScope, AffectedEntity) {
    let (risk, scope, node_id, role) = match operation {
        TransactionOperation::CreateNode { parent_node_id, .. } => (
            Risk::Write,
            ApprovalScope::SceneNodeCreate,
            parent_node_id,
            AffectedEntityRole::Parent,
        ),
        TransactionOperation::DeleteNode { node_id } => (
            Risk::Destructive,
            ApprovalScope::SceneNodeDelete,
            node_id,
            AffectedEntityRole::Target,
        ),
        TransactionOperation::ReparentNode { node_id, .. } => (
            Risk::Destructive,
            ApprovalScope::SceneNodeReparent,
            node_id,
            AffectedEntityRole::Target,
        ),
        TransactionOperation::SetProperty { node_id, .. } => (
            Risk::Destructive,
            ApprovalScope::ScenePropertySet,
            node_id,
            AffectedEntityRole::Target,
        ),
        TransactionOperation::AttachScript { node_id, .. } => (
            Risk::Write,
            ApprovalScope::SceneScriptAttach,
            node_id,
            AffectedEntityRole::Target,
        ),
        TransactionOperation::DetachScript { node_id } => (
            Risk::Destructive,
            ApprovalScope::SceneScriptDetach,
            node_id,
            AffectedEntityRole::Target,
        ),
        TransactionOperation::ConnectSignal {
            emitter_node_id, ..
        } => (
            Risk::Write,
            ApprovalScope::SceneSignalConnect,
            emitter_node_id,
            AffectedEntityRole::Emitter,
        ),
        TransactionOperation::DisconnectSignal {
            emitter_node_id, ..
        } => (
            Risk::Destructive,
            ApprovalScope::SceneSignalDisconnect,
            emitter_node_id,
            AffectedEntityRole::Emitter,
        ),
    };
    (
        risk,
        scope,
        AffectedEntity {
            node_id: node_id.clone(),
            role,
        },
    )
}

fn transaction_limits() -> TransactionLimits {
    TransactionLimits {
        operations: 1,
        prepared_records: 64,
        applying_per_history: 1,
        prepared_ttl_ms: 300_000,
        approval_timeout_ms: 120_000,
        receipt_ttl_ms: 30_000,
        clock_skew_ms: 2_000,
        approval_message_bytes: 8_192,
        preview_bytes: 65_536,
        operation_bytes: 65_536,
        variant_depth: 8,
        container_items: 1_000,
        string_characters: 16_384,
        structural_nodes: 1_000,
        journal_records: 1_024,
        journal_bytes: 8_388_608,
        status_bytes: 65_536,
        request_deadline_ms: 5_000,
    }
}

#[derive(Clone, Debug)]
struct AcceptingClient;

impl ClientHandler for AcceptingClient {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::new(
            ClientCapabilities::builder()
                .enable_elicitation_with(
                    ElicitationCapability::new()
                        .with_form(FormElicitationCapability::new().with_schema_validation(true)),
                )
                .build(),
            Implementation::new("success-contract-client", "1.0.0"),
        )
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        let request = serde_json::to_value(request).expect("serialize elicitation");
        assert_eq!(request["mode"], "form");
        assert_eq!(request["requestedSchema"]["type"], "object");
        assert_eq!(request["requestedSchema"]["properties"], json!({}));
        assert!(request["requestedSchema"].get("required").is_none());
        Ok(ElicitResult::new(ElicitationAction::Accept))
    }
}

struct RuntimeBridgeFixture {
    join: JoinHandle<()>,
    methods: Arc<Mutex<BTreeMap<String, usize>>>,
}

impl RuntimeBridgeFixture {
    fn install(
        project_root: &Path,
        project_id: &str,
        editor_session_id: &str,
        expected_connections: usize,
    ) -> Self {
        let codex = project_root.join(".godot/codex");
        let run = codex.join("run");
        fs::create_dir_all(&run).unwrap();
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
        let endpoint_relative = ".godot/codex/run/b.sock";
        let endpoint = project_root.join(endpoint_relative);
        let listener = UnixListener::bind(&endpoint).unwrap();
        fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600)).unwrap();
        let token = [0x31_u8; 32];
        fs::write(codex.join("session.token"), token).unwrap();
        fs::write(codex.join("bridge.lock"), b"{}").unwrap();
        fs::write(
            codex.join("bridge.json"),
            serde_json::to_vec(&json!({
                "discovery_schema": 1,
                "transport": "uds",
                "endpoint": endpoint_relative,
                "token_file": ".godot/codex/session.token",
                "project_id": project_id,
                "editor_session_id": editor_session_id,
                "protocol_versions": ["1.8"],
            }))
            .unwrap(),
        )
        .unwrap();
        for name in ["session.token", "bridge.lock", "bridge.json"] {
            fs::set_permissions(codex.join(name), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let project_id = project_id.to_owned();
        let editor_session_id = editor_session_id.to_owned();
        let methods = Arc::new(Mutex::new(BTreeMap::new()));
        let observed_methods = Arc::clone(&methods);
        let join = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut served = 0;
            while served < expected_connections {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!(
                        "runtime Bridge accepted {served}/{expected_connections} connections: {error}"
                    ),
                };
                stream.set_nonblocking(false).unwrap();
                serve_authenticated_connection(
                    &mut stream,
                    &token,
                    &project_id,
                    &editor_session_id,
                    &observed_methods,
                );
                served += 1;
            }
        });
        Self { join, methods }
    }

    fn finish(self) {
        self.join.join().expect("join runtime Bridge fixture");
        assert_eq!(
            *self.methods.lock().unwrap(),
            BTreeMap::from([
                ("runtime.continue".to_owned(), 1),
                ("runtime.object.inspect".to_owned(), 1),
                ("runtime.pause".to_owned(), 1),
                ("runtime.run".to_owned(), 2),
                ("runtime.snapshot.get".to_owned(), 1),
                ("runtime.stack.get".to_owned(), 1),
                ("runtime.stop".to_owned(), 1),
                ("runtime.viewport.capture".to_owned(), 1),
                ("transaction.prepare_change_set".to_owned(), 1),
            ])
        );
    }
}

fn serve_authenticated_connection(
    stream: &mut UnixStream,
    token: &[u8; 32],
    project_id: &str,
    editor_session_id: &str,
    methods: &Mutex<BTreeMap<String, usize>>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let hello = read_bridge_frame(stream).expect("client hello");
    assert_eq!(hello["project_id"], project_id);
    assert_eq!(hello["editor_session_id"], editor_session_id);
    assert_eq!(hello["handshake_version"], "1.0");
    assert_eq!(hello["kind"], "handshake.client_hello");
    let client_nonce = decode_bridge_nonce(hello["client_nonce"].as_str().unwrap()).unwrap();
    let offered = hello["supported_protocol_versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let server_nonce = [0x52_u8; 32];
    let transcript = bridge_handshake_transcript(
        &offered,
        "1.8",
        project_id,
        editor_session_id,
        &client_nonce,
        &server_nonce,
    );
    write_bridge_frame(
        stream,
        &json!({
            "handshake_version": "1.0",
            "kind": "handshake.server_challenge",
            "selected_protocol_version": "1.8",
            "project_id": project_id,
            "editor_session_id": editor_session_id,
            "server_nonce": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(server_nonce),
            "server_proof": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                bridge_proof(true, token, &transcript)
            ),
        }),
    );
    let authenticate = read_bridge_frame(stream).expect("client authentication");
    assert_eq!(
        decode_bridge_nonce(authenticate["client_proof"].as_str().unwrap()).unwrap(),
        bridge_proof(false, token, &transcript)
    );
    write_bridge_frame(
        stream,
        &json!({
            "handshake_version": "1.0",
            "kind": "handshake.server_ready",
            "selected_protocol_version": "1.8",
            "project_id": project_id,
            "editor_session_id": editor_session_id,
        }),
    );
    let initialize = read_bridge_frame(stream).expect("bridge.initialize");
    let capabilities = initialize
        .pointer("/params/requested_capabilities")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|name| {
            json!({
                "name": name.as_str().unwrap(),
                "readiness": "ready",
            })
        })
        .collect::<Vec<_>>();
    write_bridge_frame(
        stream,
        &bridge_response(
            &initialize,
            json!({"capabilities": capabilities}),
            project_id,
            editor_session_id,
        ),
    );
    let Some(request) = read_bridge_frame(stream) else {
        return;
    };
    let method = request["method"].as_str().unwrap().to_owned();
    *methods.lock().unwrap().entry(method.clone()).or_insert(0) += 1;
    match method.as_str() {
        "runtime.run" | "runtime.stop" | "runtime.pause" | "runtime.continue" => {
            let target = request
                .pointer("/params/target")
                .cloned()
                .unwrap_or_else(|| json!("project"));
            let state = match method.as_str() {
                "runtime.stop" => "stopped",
                "runtime.pause" => "paused",
                _ => "running",
            };
            write_bridge_frame(
                stream,
                &bridge_response(
                    &request,
                    json!({
                        "schema_version": "runtime/1.0",
                        "project_id": project_id,
                        "editor_session_id": editor_session_id,
                        "runtime_session_id": RUNTIME_SESSION_ID,
                        "runtime_event_seq": 2,
                        "state": state,
                        "origin": "mcp",
                        "target": target,
                        "scene_path": "res://main.tscn",
                    }),
                    project_id,
                    editor_session_id,
                ),
            );
        }
        "runtime.snapshot.get" => {
            let mut accepted: Value = serde_json::from_str(include_str!(
                "../../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-snapshot-response.json"
            ))
            .unwrap();
            adapt_bridge_message(
                &mut accepted,
                project_id,
                editor_session_id,
                Some(request["request_id"].clone()),
            );
            write_bridge_frame(stream, &accepted);

            let mut begin: Value = serde_json::from_str(include_str!(
                "../../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-snapshot-begin.json"
            ))
            .unwrap();
            adapt_bridge_message(&mut begin, project_id, editor_session_id, None);
            write_bridge_frame(stream, &begin);

            let mut chunk: Value = serde_json::from_str(include_str!(
                "../../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-snapshot-chunk.json"
            ))
            .unwrap();
            adapt_bridge_message(&mut chunk, project_id, editor_session_id, None);
            chunk["domain"] = json!("runtime");
            write_bridge_frame(stream, &chunk);
            let ack = read_bridge_frame(stream).expect("runtime snapshot acknowledgement");
            assert_eq!(ack["kind"], "ack");
            assert_eq!(ack["params"]["snapshot_id"], chunk["snapshot_id"]);
            assert_eq!(ack["params"]["domain"], "runtime");
            assert_eq!(ack["params"]["through_chunk"], 0);

            let mut end: Value = serde_json::from_str(include_str!(
                "../../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-snapshot-end.json"
            ))
            .unwrap();
            adapt_bridge_message(&mut end, project_id, editor_session_id, None);
            write_bridge_frame(stream, &end);
        }
        "runtime.object.inspect" => write_fixture_response(
            stream,
            &request,
            include_str!(
                "../../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-object-inspect-response.json"
            ),
            project_id,
            editor_session_id,
        ),
        "runtime.stack.get" => write_fixture_response(
            stream,
            &request,
            include_str!(
                "../../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-stack-response.json"
            ),
            project_id,
            editor_session_id,
        ),
        "runtime.viewport.capture" => write_fixture_response(
            stream,
            &request,
            include_str!(
                "../../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-viewport-capture-response.json"
            ),
            project_id,
            editor_session_id,
        ),
        "transaction.prepare_change_set" => {
            let coordinates = request["params"]["coordinates"].clone();
            write_bridge_frame(
                stream,
                &bridge_response(
                    &request,
                    json!({
                        "schema_version": "change-set/1.0",
                        "change_set_id": format!("change-set:{}", "e".repeat(32)),
                        "state": "previewed",
                        "transaction_seq": 1,
                        "preview_digest": format!("sha256:{}", "a".repeat(64)),
                        "request_digest": format!("sha256:{}", "b".repeat(64)),
                        "preview": {
                            "schema_version": "canonical-change-set-preview/1.0",
                            "risk": "low",
                            "operations": [{"kind": "create_node"}],
                            "save_scope": [],
                            "validation_policy": {
                                "rollback": "never",
                                "warnings": "allow",
                                "runtime": "skip",
                            },
                        },
                        "risk": "low",
                        "scope": "change_set.atomic",
                        "operation_count": 1,
                        "created_at_ms": 1_000,
                        "expires_at_ms": 301_000,
                        "limits_applied": {"operations": 16},
                        "coordinates": coordinates,
                    }),
                    project_id,
                    editor_session_id,
                ),
            );
        }
        _ => panic!("unexpected production Bridge method {method}"),
    }
}

fn write_fixture_response(
    stream: &mut UnixStream,
    request: &Value,
    fixture: &str,
    project_id: &str,
    editor_session_id: &str,
) {
    let mut response: Value = serde_json::from_str(fixture).unwrap();
    adapt_bridge_message(
        &mut response,
        project_id,
        editor_session_id,
        Some(request["request_id"].clone()),
    );
    write_bridge_frame(stream, &response);
}

fn bridge_response(
    request: &Value,
    result: Value,
    project_id: &str,
    editor_session_id: &str,
) -> Value {
    json!({
        "protocol_version": "1.8",
        "kind": "response",
        "request_id": request["request_id"],
        "result": result,
        "context": {
            "project_id": project_id,
            "editor_session_id": editor_session_id,
        },
    })
}

fn adapt_bridge_message(
    value: &mut Value,
    project_id: &str,
    editor_session_id: &str,
    request_id: Option<Value>,
) {
    value["protocol_version"] = json!("1.8");
    value["context"]["project_id"] = json!(project_id);
    value["context"]["editor_session_id"] = json!(editor_session_id);
    if let Some(request_id) = request_id {
        value["request_id"] = request_id;
    }
    for pointer in [
        "/result/revisions/editor_session_id",
        "/params/revisions/editor_session_id",
    ] {
        if let Some(field) = value.pointer_mut(pointer) {
            *field = json!(editor_session_id);
        }
    }
}

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

fn write_bridge_frame(stream: &mut UnixStream, value: &Value) {
    let payload = serde_json::to_vec(value).unwrap();
    stream
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .unwrap();
    stream.write_all(&payload).unwrap();
    stream.flush().unwrap();
}

fn decode_bridge_nonce(value: &str) -> Option<[u8; 32]> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .ok()?
        .try_into()
        .ok()
}

fn append_bridge_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&u32::try_from(value.len()).unwrap().to_be_bytes());
    output.extend_from_slice(value);
}

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

fn ready_replica_for_project(project_id: &str) -> SnapshotReplicator {
    let response: Value = serde_json::from_str(include_str!(
        "../../../../../schemas/codex_bridge/v1/fixtures/valid/rpc-snapshot-response.json"
    ))
    .unwrap();
    let begin: Value = serde_json::from_str(include_str!(
        "../../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-begin.json"
    ))
    .unwrap();
    let chunk: SnapshotChunk = serde_json::from_str(include_str!(
        "../../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-chunk.json"
    ))
    .unwrap();
    let end_message: Value = serde_json::from_str(include_str!(
        "../../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-end.json"
    ))
    .unwrap();
    let result = &response["result"];
    let metadata = SnapshotMetadata {
        snapshot_id: result["snapshot_id"].as_str().unwrap().to_owned(),
        project_id: project_id.to_owned(),
        editor_session_id: EDITOR_SESSION_ID.to_owned(),
        base_event_seq: result["base_event_seq"].as_u64().unwrap(),
        revisions: serde_json::from_value(result["revisions"].clone()).unwrap(),
        chunk_count: usize::try_from(begin["params"]["chunk_count"].as_u64().unwrap()).unwrap(),
        limits_applied: result["limits_applied"].clone(),
    };
    let end = SnapshotEnd {
        snapshot_id: end_message["params"]["snapshot_id"]
            .as_str()
            .unwrap()
            .to_owned(),
        chunk_count: usize::try_from(end_message["params"]["chunk_count"].as_u64().unwrap())
            .unwrap(),
        entity_count: usize::try_from(end_message["params"]["entity_count"].as_u64().unwrap())
            .unwrap(),
        checksum: end_message["params"]["checksum"]
            .as_str()
            .unwrap()
            .to_owned(),
        revisions: serde_json::from_value(end_message["params"]["revisions"].clone()).unwrap(),
    };
    let replicator = SnapshotReplicator::new();
    replicator.mark_bridge_negotiated(current_negotiated_bridge());
    replicator.begin(metadata).unwrap();
    replicator.push_chunk(chunk).unwrap();
    replicator.end(end).unwrap();
    replicator
}

fn retained_validation_report() -> (ValidationCoordinator, String) {
    let change_set_id = format!("change-set:{}", "d".repeat(32));
    let preview_digest = format!("sha256:{}", "c".repeat(64));
    let mut coordinator = ValidationCoordinator::default();
    coordinator
        .begin(
            &change_set_id,
            &preview_digest,
            ValidationPolicy {
                rollback: "on_required_failure".to_owned(),
                warnings: "fail_on_introduced".to_owned(),
                runtime: "skip".to_owned(),
            },
            10,
            100,
            &[(ValidationCheck::IndexConvergence, CheckAuthority::Required)],
        )
        .unwrap();
    coordinator
        .complete_check(
            &change_set_id,
            ValidationCheck::IndexConvergence,
            CheckOutcome::Passed,
            "index converged",
            Some(&json!({"index_revision": 1})),
            20,
        )
        .unwrap();
    let report = coordinator.finalize(&change_set_id, 30).unwrap();
    (coordinator, report.report_id)
}

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().expect("tool arguments object").clone()
}

fn assert_success_result(tool_name: &str, schema: &Value, result: &CallToolResult) {
    assert_eq!(
        result.is_error,
        Some(false),
        "{tool_name} did not reach its successful production handler: {:?}",
        result.structured_content
    );
    let structured = result
        .structured_content
        .as_ref()
        .unwrap_or_else(|| panic!("{tool_name} omitted structuredContent"));
    let validator = jsonschema::draft202012::options()
        .build(schema)
        .unwrap_or_else(|error| panic!("{tool_name} advertised invalid output schema: {error}"));
    assert!(
        validator.is_valid(structured),
        "{tool_name} successful result violates its advertised output schema: {structured}"
    );
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("{tool_name} did not begin with canonical JSON text");
    };
    assert_eq!(
        text.text,
        structured.to_string(),
        "{tool_name} text is not the exact canonical structuredContent serialization"
    );
}

#[test]
fn every_canonical_tool_success_is_normalized_by_the_production_router() {
    let (offline_server, project) = offline_indexed_server_fixture();
    drop(offline_server);
    let project_id =
        godot_codex_bridge_client::project_id_for_path(project.path()).expect("project identity");
    let store = SegmentStore::open(project.path(), &project_id).unwrap();
    let resource_index =
        ResourceIndexReader::from_validated_store(&store, EDITOR_SESSION_ID, 1).unwrap();
    let scene_index = SceneIndexReader::from_validated_store(&store, EDITOR_SESSION_ID, 3).unwrap();
    let script_index =
        ScriptIndexReader::from_validated_store(&store, EDITOR_SESSION_ID, 1, 3, 5).unwrap();
    let transaction_bridge = Arc::new(TransactionFixtureBridge::new(
        project_id.clone(),
        EDITOR_SESSION_ID.to_owned(),
    ));
    fs::set_permissions(
        project.path().join(".godot/codex"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let transaction_coordinator = TransactionCoordinator::open_with(
        project.path(),
        project_id.clone(),
        transaction_bridge,
        Arc::new(FixedClock),
    )
    .unwrap();
    let (validation_coordinator, validation_report_id) = retained_validation_report();
    let mut server = GodotMcpServer::with_all_indexes_and_project_services(
        ready_replica_for_project(&project_id),
        resource_index,
        scene_index,
        script_index,
        project.path().to_owned(),
        transaction_coordinator,
    )
    .with_product_startup_observation(ready_product_startup());
    server.validation_coordinator = Arc::new(tokio::sync::Mutex::new(validation_coordinator));
    drop(store);

    let static_server = canonical_ready_server();
    let bridge = RuntimeBridgeFixture::install(project.path(), &project_id, EDITOR_SESSION_ID, 10);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let (static_server_transport, static_client_transport) = tokio::io::duplex(1_048_576);
        let static_server_task = tokio::spawn(async move {
            static_server
                .serve(static_server_transport)
                .await
                .expect("serve static production MCP router")
                .waiting()
                .await
                .expect("wait for static production MCP router");
        });
        let static_client = AcceptingClient
            .serve(static_client_transport)
            .await
            .expect("serve static success-contract MCP client");
        let schemas = static_client
            .list_all_tools()
            .await
            .expect("list canonical MCP tools")
            .into_iter()
            .map(|tool| {
                (
                    tool.name.to_string(),
                    Value::Object(
                        tool.output_schema
                            .unwrap_or_else(|| panic!("{} has no output schema", tool.name))
                            .as_ref()
                            .clone(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut covered = BTreeSet::new();

        macro_rules! call_static_success {
            ($name:literal, $arguments:expr) => {{
                let result = static_client
                    .call_tool(
                        CallToolRequestParams::new($name).with_arguments(arguments($arguments)),
                    )
                    .await
                    .unwrap_or_else(|error| panic!("{} call failed: {error}", $name));
                assert_success_result(
                    $name,
                    schemas
                        .get($name)
                        .unwrap_or_else(|| panic!("{} missing advertised schema", $name)),
                    &result,
                );
                assert!(covered.insert($name), "{} covered twice", $name);
                result
                    .structured_content
                    .expect("successful structured result")
            }};
        }

        call_static_success!("godot_get_connection_status", json!({}));
        call_static_success!("godot_get_editor_state", json!({}));
        call_static_success!("godot_get_current_scene", json!({}));
        call_static_success!("godot_get_selected_nodes", json!({}));
        call_static_success!("godot_get_open_scenes", json!({"limit": 50}));
        call_static_success!("godot_get_inspector_state", json!({}));
        call_static_success!("godot_get_open_scripts", json!({"limit": 50}));
        call_static_success!("godot_get_editor_history", json!({"limit": 50}));
        call_static_success!(
            "godot_get_diagnostics",
            json!({"scope": "editor", "limit": 50})
        );
        call_static_success!("godot_get_viewport_state", json!({}));
        call_static_success!("godot_get_confirmation_policy", json!({}));
        call_static_success!("godot_reset_confirmation_policy", json!({}));
        call_static_success!(
            "godot_get_resource_dependencies",
            json!({"resource": "uid://a", "limit": 50})
        );
        call_static_success!(
            "godot_find_resource_owners",
            json!({"resource": "res://b.tres", "limit": 50})
        );
        call_static_success!(
            "godot_get_scene_graph",
            json!({"scene": "res://main.tscn", "limit": 50})
        );
        call_static_success!(
            "godot_inspect_node",
            json!({
                "node_id": "godot:node-occurrence:v1:player",
                "limit": 50,
            })
        );
        call_static_success!(
            "godot_search_symbols",
            json!({
                "query": "attack",
                "match": "prefix",
                "language": "gdscript",
                "limit": 50,
            })
        );
        call_static_success!(
            "godot_inspect_symbol",
            json!({"symbol_id": attack_symbol_id(), "limit": 50})
        );
        call_static_success!(
            "godot_find_usages",
            json!({
                "target": {"kind": "resource", "selector": "uid://b"},
                "source_kinds": [],
                "confidence": ["exact"],
                "scope": {"kind": "project"},
                "limit": 50,
            })
        );
        static_client
            .cancel()
            .await
            .expect("cancel static MCP success client");
        static_server_task
            .await
            .expect("join static MCP production router");

        let (server_transport, client_transport) = tokio::io::duplex(1_048_576);
        let server_task = tokio::spawn(async move {
            server
                .serve(server_transport)
                .await
                .expect("serve stateful production MCP router")
                .waiting()
                .await
                .expect("wait for stateful production MCP router");
        });
        let client = AcceptingClient
            .serve(client_transport)
            .await
            .expect("serve stateful success-contract MCP client");
        let dynamic_schemas = client
            .list_all_tools()
            .await
            .expect("list stateful canonical MCP tools")
            .into_iter()
            .map(|tool| {
                (
                    tool.name.to_string(),
                    Value::Object(
                        tool.output_schema
                            .unwrap_or_else(|| panic!("{} has no output schema", tool.name))
                            .as_ref()
                            .clone(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(dynamic_schemas, schemas);

        macro_rules! call_success {
            ($name:literal, $arguments:expr) => {{
                let result = client
                    .call_tool(
                        CallToolRequestParams::new($name).with_arguments(arguments($arguments)),
                    )
                    .await
                    .unwrap_or_else(|error| panic!("{} call failed: {error}", $name));
                assert_success_result(
                    $name,
                    schemas
                        .get($name)
                        .unwrap_or_else(|| panic!("{} missing advertised schema", $name)),
                    &result,
                );
                assert!(covered.insert($name), "{} covered twice", $name);
                result
                    .structured_content
                    .expect("successful structured result")
            }};
        }

        call_success!("godot_run_project", json!({}));
        call_success!("godot_run_current_scene", json!({}));
        call_success!(
            "godot_stop_project",
            json!({
                "runtime_session_id": RUNTIME_SESSION_ID,
                "expected_runtime_event_seq": 2,
            })
        );
        call_success!(
            "godot_pause_project",
            json!({
                "runtime_session_id": RUNTIME_SESSION_ID,
                "expected_runtime_event_seq": 2,
            })
        );
        call_success!(
            "godot_continue_project",
            json!({
                "runtime_session_id": RUNTIME_SESSION_ID,
                "expected_runtime_event_seq": 2,
            })
        );
        call_success!(
            "godot_get_runtime_tree",
            json!({
                "runtime_session_id": RUNTIME_SESSION_ID,
                "expected_runtime_event_seq": 3,
                "limit": 50,
            })
        );
        call_success!(
            "godot_inspect_runtime_object",
            json!({
                "runtime_session_id": RUNTIME_SESSION_ID,
                "runtime_object_id": RUNTIME_OBJECT_ID,
            })
        );
        call_success!(
            "godot_get_stack_trace",
            json!({
                "runtime_session_id": RUNTIME_SESSION_ID,
                "runtime_stack_id": RUNTIME_STACK_ID,
            })
        );
        call_success!(
            "godot_capture_viewport",
            json!({
                "runtime_session_id": RUNTIME_SESSION_ID,
                "max_width": 1280,
                "max_height": 720,
            })
        );

        let base_prepare = json!({
            "project_id": project_id,
            "editor_session_id": EDITOR_SESSION_ID,
            "scene_id": SCENE_ID,
            "history_id": HISTORY_ID,
            "scene_revision": 3,
            "operation_seq": 0,
        });
        let prepare_arguments = |suffix: char, operation: Value| {
            let mut value = base_prepare.clone();
            value["idempotency_key"] =
                json!(format!("idempotency:{}", suffix.to_string().repeat(32)));
            value
                .as_object_mut()
                .unwrap()
                .extend(operation.as_object().unwrap().clone());
            value
        };
        let prepared = call_success!(
            "godot_prepare_create_node",
            prepare_arguments(
                '1',
                json!({
                    "parent_node_id": NODE_ID,
                    "godot_type": "Node2D",
                    "name": "Created",
                    "insertion_index": null,
                })
            )
        );
        call_success!(
            "godot_prepare_delete_node",
            prepare_arguments('2', json!({"node_id": NODE_ID}))
        );
        call_success!(
            "godot_prepare_reparent_node",
            prepare_arguments(
                '3',
                json!({
                    "node_id": NODE_ID,
                    "new_parent_node_id": OTHER_NODE_ID,
                    "insertion_index": 0,
                    "keep_global_transform": true,
                })
            )
        );
        call_success!(
            "godot_prepare_set_property",
            prepare_arguments(
                '4',
                json!({
                    "node_id": NODE_ID,
                    "property": "visible",
                    "value": {"type": "bool", "value": true},
                })
            )
        );
        call_success!(
            "godot_prepare_attach_script",
            prepare_arguments(
                '5',
                json!({
                    "node_id": NODE_ID,
                    "script_ref": {
                        "uid_missing": true,
                        "path": "res://scripts/player.gd",
                    },
                })
            )
        );
        call_success!(
            "godot_prepare_detach_script",
            prepare_arguments('6', json!({"node_id": NODE_ID}))
        );
        call_success!(
            "godot_prepare_connect_signal",
            prepare_arguments(
                '7',
                json!({
                    "emitter_node_id": NODE_ID,
                    "signal": "ready",
                    "receiver_node_id": OTHER_NODE_ID,
                    "method": "_on_ready",
                    "flags": 2,
                    "unbinds": 0,
                    "binds": [],
                })
            )
        );
        call_success!(
            "godot_prepare_disconnect_signal",
            prepare_arguments(
                '8',
                json!({
                    "emitter_node_id": NODE_ID,
                    "signal": "ready",
                    "receiver_node_id": OTHER_NODE_ID,
                    "method": "_on_ready",
                    "flags": 2,
                    "unbinds": 0,
                    "binds": [],
                })
            )
        );
        call_success!(
            "godot_prepare_change_set",
            json!({
                "project_id": project_id,
                "idempotency_key": format!("idempotency:{}", "9".repeat(32)),
                "coordinates": {
                    "editor_session_id": EDITOR_SESSION_ID,
                    "scene_id": SCENE_ID,
                    "scene_revision": 3,
                    "operation_seq": 0,
                    "resource_revision": 1,
                    "script_graph_revision": 5,
                },
                "operations": [{
                    "kind": "create_node",
                    "parent_node_id": NODE_ID,
                    "godot_type": "Node2D",
                    "name": "CompoundCreated",
                    "insertion_index": null,
                }],
                "save_scope": {"paths": []},
                "validation_policy": {
                    "rollback": "never",
                    "warnings": "allow",
                    "runtime": "skip",
                },
            })
        );
        call_success!(
            "godot_get_validation_report",
            json!({"report_id": validation_report_id, "page": 0})
        );
        call_success!(
            "godot_get_transaction_status",
            json!({"transaction_id": prepared["transaction_id"]})
        );
        let applied = call_success!(
            "godot_apply_transaction",
            json!({
                "transaction_id": prepared["transaction_id"],
                "preview_digest": prepared["preview_digest"],
                "expected_scene_revision": prepared["scene_revision"],
                "expected_operation_seq": prepared["operation_seq"],
            })
        );
        call_success!(
            "godot_undo_transaction",
            json!({
                "transaction_id": applied["transaction_id"],
                "expected_transaction_seq": applied["transaction_seq"],
                "expected_scene_revision": applied["current_scene_revision"],
                "expected_operation_seq": applied["current_operation_seq"],
            })
        );

        let expected = FULL_BETA_TOOLS.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(covered, expected, "success-path coverage must remain exact");
        assert_eq!(
            schemas.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            expected
        );

        client.cancel().await.expect("cancel MCP success client");
        server_task.await.expect("join MCP production router");
    });
    bridge.finish();
}
