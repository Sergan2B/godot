mod support;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use godot_codex_bridge_client::{
    AffectedEntity, AffectedEntityRole, ApprovalBinding, ApprovalScope, BridgeError, DirtyEffect,
    OperationKind, PrepareResult, Risk, SaveEffect, TransactionCoordinates, TransactionLimits,
    TransactionOutcome, TransactionPreview, TransactionState, TransactionStatus, UndoEligibility,
    UndoEligibilityReason,
};
use godot_codex_mcp_server::GodotMcpServer;
use godot_codex_resource_indexer::{ResourceIndexReader, SceneIndexReader, ScriptIndexReader};
use godot_codex_semantic_model::SnapshotReplicator;
use godot_codex_transactions::{
    ApplyCommand, BridgeApplyFailure, BridgeApplyOutcome, BridgeFuture, ObservedPrepare,
    ObservedStatus, PrepareCommand, TransactionBridge, TransactionClock, TransactionCoordinator,
    UndoCommand,
};
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, ElicitRequestParams, ElicitResult,
    ElicitationAction, ElicitationCapability, FormElicitationCapability, Implementation,
};
use rmcp::service::RequestContext;
use rmcp::{ClientHandler, ErrorData as McpError, RoleClient, ServiceExt};
use serde_json::{Value, json};
use support::{APPROVAL_PROBE_TOOL, ApprovalProbeServer, approval_schema_json, approval_tool};

struct FixedClock;

impl TransactionClock for FixedClock {
    fn now_ms(&self) -> u64 {
        2_000
    }
}

struct TransactionFakeBridge {
    project_id: String,
    editor_session_id: String,
    prepare: PrepareResult,
    status: Mutex<TransactionStatus>,
    apply_calls: AtomicUsize,
    undo_calls: AtomicUsize,
}

impl TransactionBridge for TransactionFakeBridge {
    fn prepare<'a>(
        &'a self,
        _command: &'a PrepareCommand,
    ) -> BridgeFuture<'a, Result<ObservedPrepare, BridgeError>> {
        Box::pin(async move {
            Ok(ObservedPrepare {
                project_id: self.project_id.clone(),
                editor_session_id: self.editor_session_id.clone(),
                result: self.prepare.clone(),
            })
        })
    }

    fn status<'a>(
        &'a self,
        _transaction_id: &'a str,
    ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>> {
        Box::pin(async move {
            Ok(ObservedStatus {
                project_id: self.project_id.clone(),
                editor_session_id: self.editor_session_id.clone(),
                result: self.status.lock().unwrap().clone(),
            })
        })
    }

    fn apply_approved<'a>(
        &'a self,
        _command: &'a ApplyCommand,
        _binding: &'a ApprovalBinding,
        _issued_at_ms: u64,
    ) -> BridgeFuture<'a, Result<BridgeApplyOutcome, BridgeApplyFailure>> {
        Box::pin(async move {
            self.apply_calls.fetch_add(1, Ordering::SeqCst);
            let mut status = self.status.lock().unwrap().clone();
            status.state = TransactionState::Committed;
            status.outcome = Some(TransactionOutcome::Committed);
            status.undo_eligibility = UndoEligibility {
                eligible: true,
                reason: UndoEligibilityReason::Eligible,
            };
            *self.status.lock().unwrap() = status.clone();
            Ok(BridgeApplyOutcome {
                observed: ObservedStatus {
                    project_id: self.project_id.clone(),
                    editor_session_id: self.editor_session_id.clone(),
                    result: status,
                },
                receipt_hash: format!("sha256:{}", "b".repeat(64)),
            })
        })
    }

    fn undo<'a>(
        &'a self,
        _command: &'a UndoCommand,
    ) -> BridgeFuture<'a, Result<ObservedStatus, BridgeError>> {
        Box::pin(async move {
            self.undo_calls.fetch_add(1, Ordering::SeqCst);
            let mut status = self.status.lock().unwrap().clone();
            status.state = TransactionState::Undone;
            status.outcome = Some(TransactionOutcome::Undone);
            status.undo_eligibility = UndoEligibility {
                eligible: false,
                reason: UndoEligibilityReason::NotCommitted,
            };
            *self.status.lock().unwrap() = status.clone();
            Ok(ObservedStatus {
                project_id: self.project_id.clone(),
                editor_session_id: self.editor_session_id.clone(),
                result: status,
            })
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum MockResponse {
    AcceptTrue,
    AcceptFalse,
    AcceptMissing,
    AcceptExtra,
    Decline,
    Cancel,
    Timeout,
    LongTimeout,
}

#[derive(Clone, Debug)]
struct MockClient {
    supports_form: bool,
    response: MockResponse,
    calls: Arc<AtomicUsize>,
}

impl MockClient {
    fn new(supports_form: bool, response: MockResponse) -> Self {
        Self {
            supports_form,
            response,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl ClientHandler for MockClient {
    fn get_info(&self) -> ClientInfo {
        let capabilities = if self.supports_form {
            ClientCapabilities::builder()
                .enable_elicitation_with(
                    ElicitationCapability::new()
                        .with_form(FormElicitationCapability::new().with_schema_validation(true)),
                )
                .build()
        } else {
            ClientCapabilities::default()
        };
        ClientInfo::new(capabilities, Implementation::new("s9-mock-client", "1.0.0"))
    }

    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, McpError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let serialized = serde_json::to_value(request).expect("serialize request");
        assert_eq!(serialized["mode"], "form");
        let message = serialized["message"].as_str().unwrap();
        assert!(message.len() <= 8_192);
        if message.contains("Preview JSON:") {
            assert!(message.contains("\"operation_kind\":\"create_node\""));
        }
        assert_eq!(
            serialized["requestedSchema"]["required"],
            json!(["confirm"])
        );
        assert_eq!(
            serialized["requestedSchema"]["properties"]["confirm"]["type"],
            "boolean"
        );
        assert_eq!(
            serialized["requestedSchema"]["properties"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert!(
            serialized["requestedSchema"]["properties"]["confirm"]
                .get("default")
                .is_none()
        );
        match self.response {
            MockResponse::AcceptTrue => {
                Ok(ElicitResult::new(ElicitationAction::Accept)
                    .with_content(json!({"confirm": true})))
            }
            MockResponse::AcceptFalse => Ok(ElicitResult::new(ElicitationAction::Accept)
                .with_content(json!({"confirm": false}))),
            MockResponse::AcceptMissing => {
                Ok(ElicitResult::new(ElicitationAction::Accept).with_content(json!({})))
            }
            MockResponse::AcceptExtra => Ok(ElicitResult::new(ElicitationAction::Accept)
                .with_content(json!({"confirm": true, "approved": true}))),
            MockResponse::Decline => Ok(ElicitResult::new(ElicitationAction::Decline)),
            MockResponse::Cancel => Ok(ElicitResult::new(ElicitationAction::Cancel)),
            MockResponse::Timeout => {
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok(ElicitResult::new(ElicitationAction::Accept)
                    .with_content(json!({"confirm": true})))
            }
            MockResponse::LongTimeout => {
                tokio::time::sleep(Duration::from_secs(121)).await;
                Ok(ElicitResult::new(ElicitationAction::Accept)
                    .with_content(json!({"confirm": true})))
            }
        }
    }
}

async fn call_probe(client_handler: MockClient, timeout: Duration) -> (Value, bool, usize) {
    let calls = client_handler.calls.clone();
    let (server_transport, client_transport) = tokio::io::duplex(65_536);
    let server_task = tokio::spawn(async move {
        ApprovalProbeServer::new(timeout, None)
            .serve(server_transport)
            .await
            .expect("serve probe")
            .waiting()
            .await
            .expect("wait probe");
    });
    let client = client_handler
        .serve(client_transport)
        .await
        .expect("serve client");
    let result = client
        .call_tool(CallToolRequestParams::new(APPROVAL_PROBE_TOOL))
        .await
        .expect("call probe");
    let is_error = result.is_error == Some(true);
    let value = result.structured_content.expect("structured result");
    client.cancel().await.expect("cancel client");
    server_task.await.expect("join probe");
    (value, is_error, calls.load(Ordering::SeqCst))
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

async fn call_transaction_apply(client_handler: MockClient) -> (Value, bool, usize, usize) {
    use sha2::{Digest, Sha256};

    let elicitation_calls = client_handler.calls.clone();
    let exercise_complete_cycle =
        client_handler.supports_form && matches!(client_handler.response, MockResponse::AcceptTrue);
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("project.godot"), b"[application]\n").unwrap();
    let project_id = format!("project:sha256:{}", "1".repeat(64));
    let editor_session_id = format!("editor:{}", "2".repeat(32));
    let transaction_id = format!("transaction:{}", "3".repeat(32));
    let scene_id = format!("scene:{}", "4".repeat(32));
    let history_id = format!("history:{}", "5".repeat(32));
    let node_id = format!("node:{}", "6".repeat(32));
    let preview_payload_json = r#"{"summary":"Create Node2D"}"#.to_owned();
    let preview_digest = format!(
        "sha256:{:x}",
        Sha256::digest(preview_payload_json.as_bytes())
    );
    let coordinates = TransactionCoordinates {
        transaction_id: transaction_id.clone(),
        scene_id: scene_id.clone(),
        history_id: history_id.clone(),
        scene_revision: 7,
        operation_seq: 8,
        transaction_seq: 1,
    };
    let preview = TransactionPreview {
        operation_kind: OperationKind::CreateNode,
        summary: "Create Node2D named Created".to_owned(),
        dirty_effect: DirtyEffect::MarksSceneDirty,
        save_effect: SaveEffect::NotSaved,
        preconditions: vec!["Scene is open".to_owned()],
        truncated: false,
    };
    let prepare = PrepareResult {
        schema_version: "transaction/1.0".to_owned(),
        coordinates: coordinates.clone(),
        state: TransactionState::Previewed,
        operation_kind: OperationKind::CreateNode,
        risk: Risk::Write,
        scope: ApprovalScope::SceneNodeCreate,
        affected_entities: vec![AffectedEntity {
            node_id: node_id.clone(),
            role: AffectedEntityRole::Parent,
        }],
        preview,
        preview_payload_json,
        preview_digest: preview_digest.clone(),
        created_at_ms: 1_000,
        expires_at_ms: 301_000,
        limits_applied: transaction_limits(),
    };
    let status = TransactionStatus {
        schema_version: "transaction/1.0".to_owned(),
        coordinates,
        state: TransactionState::Previewed,
        operation_kind: OperationKind::CreateNode,
        risk: Risk::Write,
        scope: ApprovalScope::SceneNodeCreate,
        preview_digest: preview_digest.clone(),
        current_scene_revision: 7,
        current_operation_seq: 8,
        outcome: Some(TransactionOutcome::None),
        error: None,
        undo_eligibility: UndoEligibility {
            eligible: false,
            reason: UndoEligibilityReason::NotCommitted,
        },
        committed_entities: None,
        updated_at_ms: 1_000,
        limits_applied: transaction_limits(),
        truncated: false,
    };
    let bridge = Arc::new(TransactionFakeBridge {
        project_id: project_id.clone(),
        editor_session_id: editor_session_id.clone(),
        prepare,
        status: Mutex::new(status),
        apply_calls: AtomicUsize::new(0),
        undo_calls: AtomicUsize::new(0),
    });
    let coordinator = TransactionCoordinator::open_with(
        project.path(),
        project_id.clone(),
        bridge.clone(),
        Arc::new(FixedClock),
    )
    .unwrap();
    let server = GodotMcpServer::with_all_indexes_and_project_services(
        SnapshotReplicator::new(),
        ResourceIndexReader::new(),
        SceneIndexReader::new(),
        ScriptIndexReader::new(),
        project.path().to_path_buf(),
        coordinator,
    );
    let (server_transport, client_transport) = tokio::io::duplex(65_536);
    let server_task = tokio::spawn(async move {
        server
            .serve(server_transport)
            .await
            .expect("serve transaction server")
            .waiting()
            .await
            .expect("wait transaction server");
    });
    let client = client_handler
        .serve(client_transport)
        .await
        .expect("serve client");
    let prepare_arguments = json!({
        "project_id": project_id,
        "editor_session_id": editor_session_id,
        "scene_id": scene_id,
        "history_id": history_id,
        "scene_revision": 7,
        "operation_seq": 8,
        "idempotency_key": format!("idempotency:{}", "7".repeat(32)),
        "parent_node_id": node_id,
        "godot_type": "Node2D",
        "name": "Created"
    })
    .as_object()
    .unwrap()
    .clone();
    let prepared = client
        .call_tool(
            CallToolRequestParams::new("godot_prepare_create_node")
                .with_arguments(prepare_arguments),
        )
        .await
        .expect("call transaction prepare");
    assert_eq!(prepared.is_error, Some(false));
    let prepared = prepared
        .structured_content
        .expect("structured prepare result");
    let arguments = json!({
        "transaction_id": prepared["transaction_id"],
        "preview_digest": prepared["preview_digest"],
        "expected_scene_revision": prepared["scene_revision"],
        "expected_operation_seq": prepared["operation_seq"],
    })
    .as_object()
    .unwrap()
    .clone();
    let result = client
        .call_tool(CallToolRequestParams::new("godot_apply_transaction").with_arguments(arguments))
        .await
        .expect("call transaction apply");
    let is_error = result.is_error == Some(true);
    let value = result.structured_content.expect("structured result");
    if exercise_complete_cycle {
        assert!(!is_error);
        let status_arguments = json!({
            "transaction_id": prepared["transaction_id"]
        })
        .as_object()
        .unwrap()
        .clone();
        let status = client
            .call_tool(
                CallToolRequestParams::new("godot_get_transaction_status")
                    .with_arguments(status_arguments),
            )
            .await
            .expect("call transaction status");
        assert_eq!(status.is_error, Some(false));
        let status = status.structured_content.expect("structured status result");
        assert_eq!(status["state"], "committed");
        let undo_arguments = json!({
            "transaction_id": status["transaction_id"],
            "expected_transaction_seq": status["transaction_seq"],
            "expected_scene_revision": status["current_scene_revision"],
            "expected_operation_seq": status["current_operation_seq"]
        })
        .as_object()
        .unwrap()
        .clone();
        for _ in 0..2 {
            let undone = client
                .call_tool(
                    CallToolRequestParams::new("godot_undo_transaction")
                        .with_arguments(undo_arguments.clone()),
                )
                .await
                .expect("call transaction Undo");
            assert_eq!(undone.is_error, Some(false));
            assert_eq!(
                undone.structured_content.expect("structured Undo result")["state"],
                "undone"
            );
        }
        assert_eq!(bridge.undo_calls.load(Ordering::SeqCst), 1);
    }
    client.cancel().await.expect("cancel client");
    server_task.await.expect("join transaction server");
    (
        value,
        is_error,
        elicitation_calls.load(Ordering::SeqCst),
        bridge.apply_calls.load(Ordering::SeqCst),
    )
}

#[test]
fn approval_boundary_schema_and_annotations_are_closed() {
    let schema = approval_schema_json();
    assert_eq!(schema["mode"], "form");
    assert_eq!(schema["requestedSchema"]["required"], json!(["confirm"]));
    assert_eq!(
        schema["requestedSchema"]["properties"]["confirm"]["type"],
        "boolean"
    );
    let tool = approval_tool();
    assert_eq!(tool.name, APPROVAL_PROBE_TOOL);
    assert_eq!(tool.input_schema["additionalProperties"], false);
    let annotations = tool.annotations.expect("annotations");
    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(annotations.destructive_hint, Some(true));
    assert_eq!(annotations.idempotent_hint, Some(false));
    assert_eq!(annotations.open_world_hint, Some(false));
}

#[tokio::test]
async fn approval_boundary_accept_true_is_receipt_eligible() {
    let (value, is_error, calls) = call_probe(
        MockClient::new(true, MockResponse::AcceptTrue),
        Duration::from_secs(1),
    )
    .await;
    assert!(!is_error);
    assert_eq!(value["code"], "approval_accepted");
    assert_eq!(value["confirmed"], true);
    assert_eq!(value["receipt_eligible"], true);
    assert_eq!(value["project_mutated"], false);
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn approval_boundary_accept_without_true_is_rejected() {
    for response in [
        MockResponse::AcceptFalse,
        MockResponse::AcceptMissing,
        MockResponse::AcceptExtra,
    ] {
        let (value, is_error, calls) =
            call_probe(MockClient::new(true, response), Duration::from_secs(1)).await;
        assert!(is_error);
        assert_eq!(value["code"], "approval_invalid");
        assert_eq!(value["receipt_eligible"], false);
        assert_eq!(calls, 1);
    }
}

#[tokio::test]
async fn approval_boundary_decline_and_cancel_are_distinct() {
    for (response, code) in [
        (MockResponse::Decline, "approval_declined"),
        (MockResponse::Cancel, "approval_cancelled"),
    ] {
        let (value, is_error, calls) =
            call_probe(MockClient::new(true, response), Duration::from_secs(1)).await;
        assert!(is_error);
        assert_eq!(value["code"], code);
        assert_eq!(value["receipt_eligible"], false);
        assert_eq!(calls, 1);
    }
}

#[tokio::test]
async fn approval_boundary_timeout_fails_closed() {
    let (value, is_error, calls) = call_probe(
        MockClient::new(true, MockResponse::Timeout),
        Duration::from_millis(20),
    )
    .await;
    assert!(is_error);
    assert_eq!(value["code"], "approval_timeout");
    assert_eq!(value["receipt_eligible"], false);
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn approval_boundary_unsupported_host_is_not_elicited() {
    let (value, is_error, calls) = call_probe(
        MockClient::new(false, MockResponse::AcceptTrue),
        Duration::from_secs(1),
    )
    .await;
    assert!(is_error);
    assert_eq!(value["code"], "approval_host_unsupported");
    assert_eq!(value["receipt_eligible"], false);
    assert_eq!(calls, 0);
}

#[tokio::test]
async fn production_apply_is_unavailable_without_form_binding() {
    let (value, is_error, elicitation_calls, apply_calls) =
        call_transaction_apply(MockClient::new(false, MockResponse::AcceptTrue)).await;
    assert!(is_error);
    assert_eq!(value["error"]["code"], "approval_host_unsupported");
    assert_eq!(elicitation_calls, 0);
    assert_eq!(apply_calls, 0);
}

#[tokio::test]
async fn production_apply_accepts_exact_form_and_dispatches_once() {
    let (value, is_error, elicitation_calls, apply_calls) =
        call_transaction_apply(MockClient::new(true, MockResponse::AcceptTrue)).await;
    assert!(!is_error);
    assert_eq!(value["state"], "committed");
    assert_eq!(elicitation_calls, 1);
    assert_eq!(apply_calls, 1);
    let serialized = value.to_string();
    assert!(!serialized.contains("history:"));
    assert!(value.get("receipt").is_none());
    assert!(value.get("receipt_hash").is_none());
    assert!(value.get("nonce").is_none());
    assert!(value.get("mac").is_none());
}

#[tokio::test]
async fn production_apply_rejects_non_exact_or_negative_approval_without_dispatch() {
    for (response, code) in [
        (MockResponse::AcceptFalse, "approval_invalid"),
        (MockResponse::AcceptMissing, "approval_invalid"),
        (MockResponse::AcceptExtra, "approval_invalid"),
        (MockResponse::Decline, "approval_declined"),
        (MockResponse::Cancel, "approval_cancelled"),
    ] {
        let (value, is_error, elicitation_calls, apply_calls) =
            call_transaction_apply(MockClient::new(true, response)).await;
        assert!(is_error);
        assert_eq!(value["error"]["code"], code);
        assert_eq!(elicitation_calls, 1);
        assert_eq!(apply_calls, 0);
    }
}

#[tokio::test(start_paused = true)]
async fn production_apply_timeout_fails_closed_without_dispatch() {
    let (value, is_error, elicitation_calls, apply_calls) =
        call_transaction_apply(MockClient::new(true, MockResponse::LongTimeout)).await;
    assert!(is_error);
    assert_eq!(value["error"]["code"], "approval_timeout");
    assert_eq!(elicitation_calls, 1);
    assert_eq!(apply_calls, 0);
}
