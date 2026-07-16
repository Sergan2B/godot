use std::collections::BTreeSet;
use std::path::Path;

#[cfg(any(unix, windows, test))]
use base64::Engine;
use godot_codex_semantic_model::SnapshotReplicator;
#[cfg(any(unix, windows))]
use godot_codex_semantic_model::{RevisionVector, SnapshotChunk, SnapshotEnd, SnapshotMetadata};
#[cfg(any(unix, windows, test))]
use hmac::{Hmac, Mac};
#[cfg(any(unix, windows, test))]
use serde_json::Value;
#[cfg(any(unix, windows, test))]
use serde_json::json;
#[cfg(any(unix, windows, test))]
use sha2::Sha256;
use thiserror::Error;

#[cfg(any(unix, windows, test))]
use crate::Discovery;

#[cfg(any(unix, windows))]
const MAX_FRAME_BYTES: usize = 1_048_576;
#[cfg(any(unix, windows))]
const IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(any(unix, windows, test))]
type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("bridge I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("bridge JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bridge message is invalid: {0}")]
    Invalid(String),
    #[error("bridge authentication failed")]
    Authentication,
    #[error("bridge snapshot failed: {0}")]
    Replica(#[from] godot_codex_semantic_model::ReplicaError),
    #[error("Godot bridge transport is unavailable on this platform")]
    Unsupported,
    #[error("bridge I/O timed out")]
    Timeout,
    #[error("bridge capability {capability} is unavailable after negotiating {negotiated_version}")]
    CapabilityUnavailable {
        capability: &'static str,
        negotiated_version: String,
    },
    #[error("bridge RPC failed with {code}: {message}")]
    Rpc {
        code: String,
        message: String,
        retryable: bool,
        data: Value,
    },
}

impl BridgeError {
    /// Returns a stable, user-facing description that cannot expose local paths,
    /// socket names, or parser details through the MCP error envelope.
    pub fn safe_summary(&self) -> &'static str {
        match self {
            Self::Io(_) => "Godot bridge is unavailable",
            Self::Json(_) | Self::Invalid(_) => "Godot bridge validation failed",
            Self::Authentication => "Godot bridge authentication failed",
            Self::Replica(_) => "Godot bridge snapshot validation failed",
            Self::Unsupported => "Godot bridge transport is unavailable on this platform",
            Self::Timeout => "Godot bridge timed out",
            Self::CapabilityUnavailable { .. } => "Godot bridge capability is unavailable",
            Self::Rpc { .. } => "Godot bridge request failed",
        }
    }
}

#[cfg(any(unix, windows, test))]
fn append_length_prefixed(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(
        &u32::try_from(value.len())
            .expect("bounded field")
            .to_be_bytes(),
    );
    output.extend_from_slice(value);
}

#[cfg(any(unix, windows, test))]
fn handshake_transcript(
    offered_versions: &[String],
    selected_version: &str,
    discovery: &Discovery,
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
) -> Vec<u8> {
    let mut transcript = b"godot-codex-bridge/handshake-transcript/v1\0".to_vec();
    append_length_prefixed(&mut transcript, b"1.0");
    transcript.extend_from_slice(
        &u32::try_from(offered_versions.len())
            .expect("bounded versions")
            .to_be_bytes(),
    );
    for version in offered_versions {
        append_length_prefixed(&mut transcript, version.as_bytes());
    }
    append_length_prefixed(&mut transcript, selected_version.as_bytes());
    append_length_prefixed(&mut transcript, discovery.project_id.as_bytes());
    append_length_prefixed(&mut transcript, discovery.editor_session_id.as_bytes());
    append_length_prefixed(&mut transcript, client_nonce);
    append_length_prefixed(&mut transcript, server_nonce);
    transcript
}

#[cfg(any(unix, windows, test))]
fn hmac_proof(server: bool, token: &[u8; 32], transcript: &[u8]) -> [u8; 32] {
    let domain = if server {
        b"godot-codex-bridge/server-proof/v1\0".as_slice()
    } else {
        b"godot-codex-bridge/client-proof/v1\0".as_slice()
    };
    let mut hmac = HmacSha256::new_from_slice(token).expect("HMAC accepts a 32-byte token");
    hmac.update(domain);
    hmac.update(transcript);
    hmac.finalize().into_bytes().into()
}

#[cfg(any(unix, windows))]
fn decode_base64url_32(value: &str) -> Result<[u8; 32], BridgeError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| BridgeError::Authentication)?
        .try_into()
        .map_err(|_| BridgeError::Authentication)
}

#[cfg(any(unix, windows))]
fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, BridgeError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::Invalid(format!("missing string field {key}")))
}

#[cfg(any(unix, windows))]
fn required_u64(value: &Value, key: &str) -> Result<u64, BridgeError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| BridgeError::Invalid(format!("missing integer field {key}")))
}

#[cfg(any(unix, windows, test))]
fn is_sync_invalidation(value: &Value) -> Result<bool, BridgeError> {
    match value.get("method").and_then(Value::as_str) {
        Some("sync.event") => {
            if value.get("kind").and_then(Value::as_str) != Some("notification")
                || value
                    .pointer("/params/event_seq")
                    .and_then(Value::as_u64)
                    .is_none()
            {
                return Err(BridgeError::Invalid("sync.event is invalid".to_owned()));
            }
            Ok(true)
        }
        Some("sync.invalidated") => {
            if value.get("kind").and_then(Value::as_str) != Some("notification") {
                return Err(BridgeError::Invalid(
                    "sync.invalidated is invalid".to_owned(),
                ));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(any(unix, windows, test))]
fn valid_snapshot_limits(value: &Value) -> bool {
    let bounded = |key: &str, maximum: u64| {
        value
            .get(key)
            .and_then(Value::as_u64)
            .is_some_and(|number| number > 0 && number <= maximum)
    };
    value.is_object()
        && bounded("snapshot_chunk_bytes", 524_288)
        && value
            .get("negotiated_snapshot_chunk_bytes")
            .and_then(Value::as_u64)
            == Some(524_288)
        && bounded("variant_depth", 8)
        && bounded("container_items", 1_000)
        && bounded("identity_characters", 1_024)
        && bounded("projected_value_bytes", 65_536)
        && bounded("inspector_bytes_per_node", 262_144)
        && bounded("total_inspector_bytes", 4_194_304)
        && value.get("truncated").and_then(Value::as_bool).is_some()
}

#[cfg(any(unix, windows))]
fn validate_context(
    value: &Value,
    discovery: &Discovery,
    protocol_version: &str,
) -> Result<(), BridgeError> {
    if value.pointer("/context/project_id").and_then(Value::as_str)
        != Some(discovery.project_id.as_str())
        || value
            .pointer("/context/editor_session_id")
            .and_then(Value::as_str)
            != Some(discovery.editor_session_id.as_str())
        || value.get("protocol_version").and_then(Value::as_str) != Some(protocol_version)
    {
        return Err(BridgeError::Invalid(
            "RPC context binding mismatch".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
type PlatformStream = tokio::net::UnixStream;
#[cfg(windows)]
type PlatformStream = tokio::net::TcpStream;

#[cfg(any(unix, windows))]
struct FrameStream {
    stream: PlatformStream,
}

#[cfg(any(unix, windows))]
impl FrameStream {
    async fn connect(endpoint: &crate::BridgeEndpoint) -> Result<Self, BridgeError> {
        #[cfg(unix)]
        let crate::BridgeEndpoint::Unix(endpoint) = endpoint else {
            return Err(BridgeError::Invalid(
                "unexpected bridge transport".to_owned(),
            ));
        };
        #[cfg(unix)]
        let connect = tokio::net::UnixStream::connect(endpoint);
        #[cfg(windows)]
        let crate::BridgeEndpoint::Tcp(endpoint) = endpoint else {
            return Err(BridgeError::Invalid(
                "unexpected bridge transport".to_owned(),
            ));
        };
        #[cfg(windows)]
        let connect = tokio::net::TcpStream::connect(endpoint);
        let stream = tokio::time::timeout(IO_TIMEOUT, connect)
            .await
            .map_err(|_| BridgeError::Timeout)??;
        #[cfg(windows)]
        stream.set_nodelay(true)?;
        Ok(Self { stream })
    }

    async fn send(&mut self, value: &Value) -> Result<(), BridgeError> {
        use tokio::io::AsyncWriteExt;
        let payload = serde_json::to_vec(value)?;
        if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
            return Err(BridgeError::Invalid(
                "outbound frame size is invalid".to_owned(),
            ));
        }
        self.stream
            .write_all(
                &u32::try_from(payload.len())
                    .expect("bounded frame")
                    .to_be_bytes(),
            )
            .await?;
        self.stream.write_all(&payload).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, BridgeError> {
        use tokio::io::AsyncReadExt;
        let mut prefix = [0_u8; 4];
        self.stream.read_exact(&mut prefix).await?;
        let length = usize::try_from(u32::from_be_bytes(prefix)).expect("u32 fits usize");
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(BridgeError::Invalid(
                "inbound frame size is invalid".to_owned(),
            ));
        }
        let mut payload = vec![0_u8; length];
        self.stream.read_exact(&mut payload).await?;
        let value: Value = serde_json::from_slice(&payload)?;
        if !value.is_object() {
            return Err(BridgeError::Invalid(
                "frame is not a JSON object".to_owned(),
            ));
        }
        Ok(value)
    }

    async fn receive_timed(&mut self) -> Result<Value, BridgeError> {
        self.receive_with_timeout(IO_TIMEOUT).await
    }

    async fn receive_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Value, BridgeError> {
        tokio::time::timeout(timeout, self.receive())
            .await
            .map_err(|_| BridgeError::Timeout)?
    }
}

#[cfg(any(unix, windows))]
pub(crate) struct Session {
    discovery: Discovery,
    stream: FrameStream,
    next_request: u64,
    resync_requested: bool,
    selected_protocol_version: String,
    capabilities: BTreeSet<String>,
}

#[cfg(any(unix, windows))]
impl Session {
    pub(crate) async fn connect(project_root: &Path) -> Result<Self, BridgeError> {
        let discovery = Discovery::load(project_root)?;
        let mut stream = FrameStream::connect(&discovery.endpoint).await?;
        let offered_versions = vec!["1.2".to_owned()];
        let mut client_nonce = [0_u8; 32];
        getrandom::fill(&mut client_nonce)
            .map_err(|error| BridgeError::Invalid(format!("client nonce failed: {error}")))?;
        let hello = json!({
            "handshake_version": "1.0",
            "kind": "handshake.client_hello",
            "supported_protocol_versions": offered_versions,
            "project_id": discovery.project_id,
            "editor_session_id": discovery.editor_session_id,
            "client_nonce": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(client_nonce),
        });
        stream.send(&hello).await?;
        let challenge = stream.receive_timed().await?;
        let selected_protocol_version =
            required_str(&challenge, "selected_protocol_version")?.to_owned();
        if required_str(&challenge, "kind")? != "handshake.server_challenge"
            || !matches!(selected_protocol_version.as_str(), "1.0" | "1.1" | "1.2")
            || required_str(&challenge, "project_id")? != discovery.project_id
            || required_str(&challenge, "editor_session_id")? != discovery.editor_session_id
        {
            return Err(BridgeError::Authentication);
        }
        let server_nonce = decode_base64url_32(required_str(&challenge, "server_nonce")?)?;
        let transcript = handshake_transcript(
            &offered_versions,
            &selected_protocol_version,
            &discovery,
            &client_nonce,
            &server_nonce,
        );
        let server_proof = decode_base64url_32(required_str(&challenge, "server_proof")?)?;
        let mut verifier =
            HmacSha256::new_from_slice(&discovery.token).expect("HMAC accepts a 32-byte token");
        verifier.update(b"godot-codex-bridge/server-proof/v1\0");
        verifier.update(&transcript);
        verifier
            .verify_slice(&server_proof)
            .map_err(|_| BridgeError::Authentication)?;
        let authenticate = json!({
            "handshake_version": "1.0",
            "kind": "handshake.client_authenticate",
            "selected_protocol_version": selected_protocol_version,
            "project_id": discovery.project_id,
            "editor_session_id": discovery.editor_session_id,
            "client_proof": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                hmac_proof(false, &discovery.token, &transcript)
            ),
        });
        stream.send(&authenticate).await?;
        let ready = stream.receive_timed().await?;
        if required_str(&ready, "kind")? != "handshake.server_ready"
            || required_str(&ready, "selected_protocol_version")? != selected_protocol_version
            || required_str(&ready, "project_id")? != discovery.project_id
            || required_str(&ready, "editor_session_id")? != discovery.editor_session_id
        {
            return Err(BridgeError::Authentication);
        }
        discovery.assert_unchanged()?;
        let transport_capability = discovery.transport_capability();
        let mut session = Self {
            discovery,
            stream,
            next_request: 1,
            resync_requested: false,
            selected_protocol_version,
            capabilities: BTreeSet::new(),
        };
        let mut requested_capabilities = vec!["bridge.lifecycle", transport_capability];
        if session.selected_protocol_version != "1.0" {
            requested_capabilities.extend([
                "editor.context",
                "editor.inspector",
                "sync.full_snapshot_v1",
                "sync.event_stream_v1",
            ]);
        }
        if session.selected_protocol_version == "1.2" {
            requested_capabilities
                .extend(["resource.uid_dependencies", "resource.incremental_index"]);
        }
        let initialize = session
            .request(
                "bridge.initialize",
                json!({
                    "client": {"name": "godot-codex-mcp", "version": env!("CARGO_PKG_VERSION")},
                    "requested_capabilities": requested_capabilities,
                }),
            )
            .await?;
        let capabilities = initialize
            .pointer("/result/capabilities")
            .and_then(Value::as_array)
            .ok_or_else(|| BridgeError::Invalid("capabilities are missing".to_owned()))?;
        for entry in capabilities {
            if entry.get("readiness").and_then(Value::as_str) == Some("ready")
                && let Some(name) = entry.get("name").and_then(Value::as_str)
            {
                session.capabilities.insert(name.to_owned());
            }
        }
        for capability in requested_capabilities {
            if !session.capabilities.contains(capability) {
                return Err(BridgeError::Invalid(format!(
                    "required capability is unavailable: {capability}"
                )));
            }
        }
        Ok(session)
    }

    pub(crate) fn context(&self) -> Value {
        json!({
            "project_id": self.discovery.project_id,
            "editor_session_id": self.discovery.editor_session_id,
        })
    }

    pub(crate) async fn receive_non_sync_timed(&mut self) -> Result<Value, BridgeError> {
        self.receive_non_sync_with_timeout(IO_TIMEOUT).await
    }

    pub(crate) async fn receive_non_sync_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Value, BridgeError> {
        loop {
            let message = self.stream.receive_with_timeout(timeout).await?;
            validate_context(&message, &self.discovery, &self.selected_protocol_version)?;
            if is_sync_invalidation(&message)? {
                self.resync_requested = true;
                continue;
            }
            return Ok(message);
        }
    }

    pub(crate) async fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, BridgeError> {
        self.request_with_timeout(method, params, IO_TIMEOUT).await
    }

    pub(crate) async fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value, BridgeError> {
        self.request_with_deadline_and_timeout(method, params, timeout, timeout)
            .await
    }

    pub(crate) async fn request_with_deadline_and_timeout(
        &mut self,
        method: &str,
        params: Value,
        deadline: std::time::Duration,
        receive_timeout: std::time::Duration,
    ) -> Result<Value, BridgeError> {
        let request_id = format!("req:mcp-{:016x}", self.next_request);
        self.next_request += 1;
        let request = json!({
            "protocol_version": self.selected_protocol_version,
            "kind": "request",
            "request_id": request_id,
            "method": method,
            "deadline_ms": u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
            "params": params,
            "context": self.context(),
        });
        self.stream.send(&request).await?;
        let response = self.receive_non_sync_with_timeout(receive_timeout).await?;
        if response.get("kind").and_then(Value::as_str) != Some("response")
            || response.get("request_id").and_then(Value::as_str) != Some(request_id.as_str())
        {
            return Err(BridgeError::Invalid("unexpected RPC response".to_owned()));
        }
        if let Some(error) = response.get("error") {
            return Err(BridgeError::Rpc {
                code: error
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid_error")
                    .to_owned(),
                message: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Bridge RPC failed")
                    .to_owned(),
                retryable: error
                    .get("retryable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                data: error.get("data").cloned().unwrap_or_else(|| json!({})),
            });
        }
        Ok(response)
    }

    pub(crate) fn protocol_version(&self) -> &str {
        &self.selected_protocol_version
    }

    pub(crate) fn capabilities(&self) -> &BTreeSet<String> {
        &self.capabilities
    }

    pub(crate) fn project_id(&self) -> &str {
        &self.discovery.project_id
    }

    pub(crate) fn editor_session_id(&self) -> &str {
        &self.discovery.editor_session_id
    }

    pub(crate) async fn send_ack(
        &mut self,
        snapshot_id: &str,
        domain: Option<&str>,
        through_chunk: usize,
    ) -> Result<(), BridgeError> {
        let mut params = json!({
            "snapshot_id": snapshot_id,
            "through_chunk": through_chunk,
        });
        if let Some(domain) = domain {
            params["domain"] = json!(domain);
        }
        let ack = json!({
            "protocol_version": self.selected_protocol_version,
            "kind": "ack",
            "ack_id": format!("ack:snapshot-{:016x}", self.next_request),
            "params": params,
            "context": self.context(),
        });
        self.next_request += 1;
        self.stream.send(&ack).await
    }

    pub(crate) async fn send_cancel(
        &mut self,
        request_id: &str,
        reason: &str,
    ) -> Result<(), BridgeError> {
        let cancel = json!({
            "protocol_version": self.selected_protocol_version,
            "kind": "cancel",
            "request_id": request_id,
            "reason": reason,
            "context": self.context(),
        });
        self.stream.send(&cancel).await
    }

    async fn snapshot(&mut self, replicator: &SnapshotReplicator) -> Result<bool, BridgeError> {
        if self.selected_protocol_version == "1.0" {
            return Err(BridgeError::CapabilityUnavailable {
                capability: "sync.full_snapshot_v1",
                negotiated_version: self.selected_protocol_version.clone(),
            });
        }
        let response = self
            .request(
                "editor.snapshot.get",
                json!({"domains": ["editor_context", "editor_inspector"]}),
            )
            .await?;
        let result = response
            .get("result")
            .ok_or_else(|| BridgeError::Invalid("snapshot result is missing".to_owned()))?;
        let snapshot_id = required_str(result, "snapshot_id")?.to_owned();
        let base_event_seq = required_u64(result, "base_event_seq")?;
        let accepted_revisions: RevisionVector =
            serde_json::from_value(result.get("revisions").cloned().ok_or_else(|| {
                BridgeError::Invalid("snapshot revisions are missing".to_owned())
            })?)?;

        let begin = self.receive_non_sync_timed().await?;
        if begin.get("kind").and_then(Value::as_str) != Some("notification")
            || begin.get("method").and_then(Value::as_str) != Some("snapshot.begin")
            || begin.pointer("/params/snapshot_id").and_then(Value::as_str)
                != Some(snapshot_id.as_str())
        {
            return Err(BridgeError::Invalid("snapshot.begin is invalid".to_owned()));
        }
        let chunk_count = usize::try_from(required_u64(&begin["params"], "chunk_count")?)
            .map_err(|_| BridgeError::Invalid("chunk count overflows".to_owned()))?;
        let begin_revisions: RevisionVector =
            serde_json::from_value(begin["params"]["revisions"].clone())?;
        if begin_revisions != accepted_revisions
            || required_u64(&begin["params"], "base_event_seq")? != base_event_seq
        {
            return Err(BridgeError::Invalid(
                "snapshot.begin revisions differ".to_owned(),
            ));
        }
        replicator.begin(SnapshotMetadata {
            snapshot_id: snapshot_id.clone(),
            project_id: self.discovery.project_id.clone(),
            editor_session_id: self.discovery.editor_session_id.clone(),
            base_event_seq,
            revisions: begin_revisions,
            chunk_count,
            limits_applied: result
                .get("limits_applied")
                .filter(|value| valid_snapshot_limits(value))
                .cloned()
                .ok_or_else(|| BridgeError::Invalid("snapshot limits are missing".to_owned()))?,
        })?;

        for expected_index in 0..chunk_count {
            let message = self.receive_non_sync_timed().await?;
            if message.get("kind").and_then(Value::as_str) != Some("chunk") {
                return Err(BridgeError::Invalid("expected snapshot chunk".to_owned()));
            }
            let chunk = SnapshotChunk {
                snapshot_id: required_str(&message, "snapshot_id")?.to_owned(),
                chunk_index: usize::try_from(required_u64(&message, "chunk_index")?)
                    .map_err(|_| BridgeError::Invalid("chunk index overflows".to_owned()))?,
                payload: message
                    .get("payload")
                    .cloned()
                    .ok_or_else(|| BridgeError::Invalid("chunk payload is missing".to_owned()))?,
                payload_json: required_str(&message, "payload_json")?.to_owned(),
                checksum: required_str(&message, "checksum")?.to_owned(),
            };
            if chunk.chunk_index != expected_index {
                return Err(BridgeError::Invalid("chunk order mismatch".to_owned()));
            }
            replicator.push_chunk(chunk)?;
            self.send_ack(&snapshot_id, None, expected_index).await?;
        }

        let end = self.receive_non_sync_timed().await?;
        if end.get("kind").and_then(Value::as_str) != Some("notification")
            || end.get("method").and_then(Value::as_str) != Some("snapshot.end")
        {
            return Err(BridgeError::Invalid("snapshot.end is invalid".to_owned()));
        }
        let end_params = &end["params"];
        replicator.end(SnapshotEnd {
            snapshot_id: required_str(end_params, "snapshot_id")?.to_owned(),
            chunk_count: usize::try_from(required_u64(end_params, "chunk_count")?)
                .map_err(|_| BridgeError::Invalid("end chunk count overflows".to_owned()))?,
            entity_count: usize::try_from(required_u64(end_params, "entity_count")?)
                .map_err(|_| BridgeError::Invalid("entity count overflows".to_owned()))?,
            checksum: required_str(end_params, "checksum")?.to_owned(),
            revisions: serde_json::from_value(end_params["revisions"].clone())?,
        })?;
        let needs_resync = self.resync_requested;
        if needs_resync {
            self.resync_requested = false;
            replicator.invalidate("editor changed during snapshot transfer");
        }
        Ok(needs_resync)
    }

    async fn wait_for_invalidation(
        &mut self,
        replicator: &SnapshotReplicator,
    ) -> Result<(), BridgeError> {
        let message = self.stream.receive().await?;
        validate_context(&message, &self.discovery, &self.selected_protocol_version)?;
        let method = message.get("method").and_then(Value::as_str);
        match method {
            Some("sync.event") => {
                let event_seq = message
                    .pointer("/params/event_seq")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| BridgeError::Invalid("event sequence is missing".to_owned()))?;
                let current = replicator.read().map_err(|error| {
                    BridgeError::Invalid(format!("event arrived without a ready replica: {error}"))
                })?;
                if event_seq != current.revisions.event_seq + 1 {
                    replicator.invalidate("event sequence gap");
                } else {
                    replicator.invalidate(format!("editor event {event_seq} requires resnapshot"));
                }
                Ok(())
            }
            Some("sync.invalidated") => {
                let reason = message
                    .pointer("/params/reason")
                    .and_then(Value::as_str)
                    .unwrap_or("sync invalidated");
                replicator.invalidate(reason);
                Ok(())
            }
            _ => Err(BridgeError::Invalid("unexpected server message".to_owned())),
        }
    }
}

#[cfg(any(unix, windows))]
pub async fn run_session(
    project_root: &Path,
    replicator: &SnapshotReplicator,
) -> Result<(), BridgeError> {
    let mut session = Session::connect(project_root).await?;
    loop {
        let needs_resync = session.snapshot(replicator).await?;
        if !needs_resync {
            session.wait_for_invalidation(replicator).await?;
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub async fn run_session(
    _project_root: &Path,
    _replicator: &SnapshotReplicator,
) -> Result<(), BridgeError> {
    Err(BridgeError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn handshake_transcript_matches_the_canonical_vector() {
        let discovery = Discovery {
            canonical_root: std::path::PathBuf::new(),
            project_id:
                "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
                    .to_owned(),
            editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
            endpoint: crate::BridgeEndpoint::Unsupported,
            token: std::array::from_fn(|index| 0xa0 + u8::try_from(index).unwrap()),
            raw_record: Vec::new(),
        };
        let client_nonce = std::array::from_fn(|index| u8::try_from(index).unwrap());
        let server_nonce = std::array::from_fn(|index| 0x20 + u8::try_from(index).unwrap());
        let transcript = handshake_transcript(
            &["1.0".to_owned()],
            "1.0",
            &discovery,
            &client_nonce,
            &server_nonce,
        );
        assert!(transcript.starts_with(b"godot-codex-bridge/handshake-transcript/v1\0"));
        assert_eq!(
            format!("{:x}", Sha256::digest(&transcript)),
            "57e92eadc54792bd048597950a62a8325a39ddb2f29b707be6a8b3d78d3e6241"
        );
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hmac_proof(
                true,
                &discovery.token,
                &transcript
            )),
            "FA-WdGW6swe0_f-qWeYapRoRqHvPjBQIWI-zkFLUFI8"
        );
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hmac_proof(
                false,
                &discovery.token,
                &transcript
            )),
            "Po5Rvle7JrEO3__pIRWtSyFMNJSFX3XSU6iVRMbUvw0"
        );
    }

    #[test]
    fn public_error_summary_does_not_expose_local_details() {
        let error = BridgeError::Invalid(
            "discovery at /Users/alice/secret-project/.godot/codex failed".to_owned(),
        );
        let summary = error.safe_summary();
        assert_eq!(summary, "Godot bridge validation failed");
        assert!(!summary.contains("alice"));
        assert!(!summary.contains("secret-project"));
    }

    #[test]
    fn sync_notifications_are_recognized_while_waiting_for_rpc_data() {
        assert!(
            is_sync_invalidation(&json!({
                "kind": "notification",
                "method": "sync.event",
                "params": {"event_seq": 8}
            }))
            .unwrap()
        );
        assert!(
            is_sync_invalidation(&json!({
                "kind": "notification",
                "method": "sync.invalidated",
                "params": {"reason": "journal_overflow"}
            }))
            .unwrap()
        );
        assert!(!is_sync_invalidation(&json!({"kind": "chunk"})).unwrap());
        assert!(
            is_sync_invalidation(&json!({
                "kind": "notification",
                "method": "sync.event",
                "params": {}
            }))
            .is_err()
        );

        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/rpc-snapshot-response.json"
        ))
        .unwrap();
        let limits = &response["result"]["limits_applied"];
        assert!(valid_snapshot_limits(limits));
        let mut oversized = limits.clone();
        oversized["total_inspector_bytes"] = json!(4_194_305);
        assert!(!valid_snapshot_limits(&oversized));
    }
}
