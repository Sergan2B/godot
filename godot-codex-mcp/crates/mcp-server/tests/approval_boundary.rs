mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, ElicitRequestParams, ElicitResult,
    ElicitationAction, ElicitationCapability, FormElicitationCapability, Implementation,
};
use rmcp::service::RequestContext;
use rmcp::{ClientHandler, ErrorData as McpError, RoleClient, ServiceExt};
use serde_json::{Value, json};
use support::{APPROVAL_PROBE_TOOL, ApprovalProbeServer, approval_schema_json, approval_tool};

#[derive(Clone, Copy, Debug)]
enum MockResponse {
    AcceptTrue,
    AcceptFalse,
    AcceptMissing,
    AcceptExtra,
    Decline,
    Cancel,
    Timeout,
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
        assert_eq!(
            serialized["requestedSchema"]["required"],
            json!(["confirm"])
        );
        assert_eq!(
            serialized["requestedSchema"]["properties"]["confirm"]["type"],
            "boolean"
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
