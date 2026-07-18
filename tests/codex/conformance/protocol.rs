#[cfg(unix)]
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
#[cfg(any(unix, test))]
use serde_json::json;
use sha2::{Digest, Sha256};

#[cfg(unix)]
use crate::bundle::validate_instance;
#[cfg(unix)]
use crate::discovery::Discovery;
#[cfg(unix)]
use crate::error::fail;
use crate::error::{ConformanceError, Result, require};
#[cfg(unix)]
use crate::json::parse_strict_object;

pub const MAX_PAYLOAD_BYTES: usize = 1_048_576;
#[cfg(unix)]
const IO_TIMEOUT: Duration = Duration::from_secs(3);
type HmacSha256 = Hmac<Sha256>;

pub fn project_id_for_root(root: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"godot-codex-project-id/v1\0");
    hasher.update(root);
    format!("project:sha256:{:x}", hasher.finalize())
}

fn append_length_prefixed(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(
        &u32::try_from(value.len())
            .expect("handshake field length is bounded")
            .to_be_bytes(),
    );
    output.extend_from_slice(value);
}

pub fn build_handshake_transcript(
    handshake_version: &str,
    offered_versions: &[String],
    selected_version: &str,
    project_id: &str,
    editor_session_id: &str,
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
) -> Vec<u8> {
    let mut transcript = b"godot-codex-bridge/handshake-transcript/v1\0".to_vec();
    append_length_prefixed(&mut transcript, handshake_version.as_bytes());
    transcript.extend_from_slice(
        &u32::try_from(offered_versions.len())
            .expect("offered version count is bounded")
            .to_be_bytes(),
    );
    for version in offered_versions {
        append_length_prefixed(&mut transcript, version.as_bytes());
    }
    append_length_prefixed(&mut transcript, selected_version.as_bytes());
    append_length_prefixed(&mut transcript, project_id.as_bytes());
    append_length_prefixed(&mut transcript, editor_session_id.as_bytes());
    append_length_prefixed(&mut transcript, client_nonce);
    append_length_prefixed(&mut transcript, server_nonce);
    transcript
}

pub fn handshake_proof(server: bool, token: &[u8; 32], transcript: &[u8]) -> Result<[u8; 32]> {
    let domain = if server {
        b"godot-codex-bridge/server-proof/v1\0".as_slice()
    } else {
        b"godot-codex-bridge/client-proof/v1\0".as_slice()
    };
    let mut hmac = HmacSha256::new_from_slice(token)
        .map_err(|error| ConformanceError(format!("cannot initialize HMAC: {error}")))?;
    hmac.update(domain);
    hmac.update(transcript);
    Ok(hmac.finalize().into_bytes().into())
}

#[cfg(unix)]
fn decode_base64url_32(value: &str, field: &str) -> Result<[u8; 32]> {
    require(
        value.len() == 43
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'),
        format!("{field} is not an unpadded 32-byte base64url value"),
    )?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|error| ConformanceError(format!("cannot decode {field}: {error}")))?;
    bytes
        .try_into()
        .map_err(|_| ConformanceError(format!("{field} is not exactly 32 bytes")))
}

pub fn encode_frame(value: &Value) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(value)?;
    require(!payload.is_empty(), "cannot encode an empty JSON frame")?;
    require(
        payload.len() <= MAX_PAYLOAD_BYTES,
        "JSON frame exceeds the 1 MiB protocol limit",
    )?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(
        &u32::try_from(payload.len())
            .expect("payload length was checked against the 1 MiB limit")
            .to_be_bytes(),
    );
    frame.extend_from_slice(&payload);
    Ok(frame)
}

#[cfg(unix)]
pub struct FramedStream {
    stream: UnixStream,
}

#[cfg(unix)]
impl FramedStream {
    pub fn connect(endpoint: &Path) -> Result<Self> {
        let stream = UnixStream::connect(endpoint)
            .map_err(|error| ConformanceError(format!("cannot connect to bridge UDS: {error}")))?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        Ok(Self { stream })
    }

    pub fn send_value(&mut self, value: &Value, fragment_size: Option<usize>) -> Result<()> {
        let frame = encode_frame(value)?;
        self.send_bytes(&frame, fragment_size)
    }

    pub fn send_values_coalesced(&mut self, values: &[Value]) -> Result<()> {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend_from_slice(&encode_frame(value)?);
        }
        self.stream.write_all(&bytes)?;
        Ok(())
    }

    pub fn send_bytes(&mut self, bytes: &[u8], fragment_size: Option<usize>) -> Result<()> {
        let fragment_size = fragment_size.unwrap_or(bytes.len()).max(1);
        for fragment in bytes.chunks(fragment_size) {
            self.stream.write_all(fragment)?;
        }
        Ok(())
    }

    pub fn receive_value(&mut self) -> Result<Value> {
        let mut prefix = [0_u8; 4];
        self.stream
            .read_exact(&mut prefix)
            .map_err(|error| ConformanceError(format!("cannot read frame prefix: {error}")))?;
        let length = u32::from_be_bytes(prefix) as usize;
        require(length > 0, "server sent a zero-length frame")?;
        require(
            length <= MAX_PAYLOAD_BYTES,
            "server sent an oversized frame",
        )?;
        let mut payload = vec![0_u8; length];
        self.stream
            .read_exact(&mut payload)
            .map_err(|error| ConformanceError(format!("cannot read frame payload: {error}")))?;
        parse_strict_object(&payload)
    }

    pub fn expect_closed(&mut self) -> Result<()> {
        let mut byte = [0_u8; 1];
        match self.stream.read(&mut byte) {
            Ok(0) => Ok(()),
            Ok(_) => fail("server returned bytes instead of closing the invalid connection"),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::NotConnected
                ) =>
            {
                Ok(())
            }
            Err(error) => fail(format!("server did not close invalid connection: {error}")),
        }
    }
}

#[cfg(unix)]
fn random_nonce() -> Result<[u8; 32]> {
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce)
        .map_err(|error| ConformanceError(format!("cannot generate client nonce: {error}")))?;
    Ok(nonce)
}

#[cfg(unix)]
pub fn make_client_hello(
    discovery: &Discovery,
    project_id: &str,
    versions: &[String],
) -> Result<(Value, [u8; 32])> {
    let client_nonce = random_nonce()?;
    Ok((
        json!({
            "handshake_version": "1.0",
            "kind": "handshake.client_hello",
            "supported_protocol_versions": versions,
            "project_id": project_id,
            "editor_session_id": discovery.editor_session_id,
            "client_nonce": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(client_nonce),
        }),
        client_nonce,
    ))
}

#[cfg(unix)]
pub fn make_client_authenticate(
    discovery: &Discovery,
    offered_versions: &[String],
    client_nonce: &[u8; 32],
    challenge: &Value,
    token: &[u8; 32],
    verify_server_proof: bool,
) -> Result<Value> {
    validate_instance("handshake.schema.json", challenge)?;
    require(
        challenge.get("kind").and_then(Value::as_str) == Some("handshake.server_challenge"),
        "expected handshake.server_challenge",
    )?;
    let selected = challenge
        .get("selected_protocol_version")
        .and_then(Value::as_str)
        .ok_or_else(|| ConformanceError("challenge has no selected version".to_owned()))?;
    require(
        selected == "1.0",
        "server selected an unexpected protocol version",
    )?;
    require(
        challenge.get("project_id").and_then(Value::as_str) == Some(&discovery.project_id),
        "challenge project binding mismatch",
    )?;
    require(
        challenge.get("editor_session_id").and_then(Value::as_str)
            == Some(&discovery.editor_session_id),
        "challenge editor session binding mismatch",
    )?;
    let server_nonce = decode_base64url_32(
        challenge
            .get("server_nonce")
            .and_then(Value::as_str)
            .ok_or_else(|| ConformanceError("challenge has no server nonce".to_owned()))?,
        "server_nonce",
    )?;
    let transcript = build_handshake_transcript(
        "1.0",
        offered_versions,
        selected,
        &discovery.project_id,
        &discovery.editor_session_id,
        client_nonce,
        &server_nonce,
    );
    if verify_server_proof {
        let received = decode_base64url_32(
            challenge
                .get("server_proof")
                .and_then(Value::as_str)
                .ok_or_else(|| ConformanceError("challenge has no server proof".to_owned()))?,
            "server_proof",
        )?;
        let mut verifier = HmacSha256::new_from_slice(token)
            .map_err(|error| ConformanceError(format!("cannot initialize HMAC: {error}")))?;
        verifier.update(b"godot-codex-bridge/server-proof/v1\0");
        verifier.update(&transcript);
        verifier
            .verify_slice(&received)
            .map_err(|_| ConformanceError("server proof authentication failed".to_owned()))?;
    }
    let client_proof = handshake_proof(false, token, &transcript)?;
    Ok(json!({
        "handshake_version": "1.0",
        "kind": "handshake.client_authenticate",
        "selected_protocol_version": selected,
        "project_id": discovery.project_id,
        "editor_session_id": discovery.editor_session_id,
        "client_proof": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(client_proof),
    }))
}

#[cfg(unix)]
pub struct AuthenticatedConnection {
    pub stream: FramedStream,
    pub messages: Vec<(&'static str, Value)>,
}

#[cfg(unix)]
pub fn connect_authenticated(
    discovery: &Discovery,
    fragment_size: Option<usize>,
) -> Result<AuthenticatedConnection> {
    let versions = vec!["1.0".to_owned()];
    let mut stream = FramedStream::connect(&discovery.endpoint)?;
    let (hello, client_nonce) = make_client_hello(discovery, &discovery.project_id, &versions)?;
    stream.send_value(&hello, fragment_size)?;
    let challenge = stream.receive_value()?;
    let authenticate = make_client_authenticate(
        discovery,
        &versions,
        &client_nonce,
        &challenge,
        &discovery.token,
        true,
    )?;
    stream.send_value(&authenticate, fragment_size)?;
    let ready = stream.receive_value()?;
    validate_instance("handshake.schema.json", &ready)?;
    require(
        ready.get("kind").and_then(Value::as_str) == Some("handshake.server_ready"),
        "expected handshake.server_ready",
    )?;
    require(
        ready.get("project_id").and_then(Value::as_str) == Some(&discovery.project_id),
        "server_ready project binding mismatch",
    )?;
    require(
        ready.get("editor_session_id").and_then(Value::as_str)
            == Some(&discovery.editor_session_id),
        "server_ready editor session binding mismatch",
    )?;
    discovery.assert_unchanged()?;
    Ok(AuthenticatedConnection {
        stream,
        messages: vec![
            ("client", hello),
            ("server", challenge),
            ("client", authenticate),
            ("server", ready),
        ],
    })
}

#[cfg(unix)]
pub fn rpc_context(discovery: &Discovery) -> Value {
    json!({
        "project_id": discovery.project_id,
        "editor_session_id": discovery.editor_session_id,
    })
}

#[cfg(unix)]
pub fn rpc_request(
    discovery: &Discovery,
    request_id: &str,
    method: &str,
    params: Value,
    deadline_ms: Option<u64>,
) -> Value {
    let mut request = json!({
        "protocol_version": "1.0",
        "kind": "request",
        "request_id": request_id,
        "method": method,
        "context": rpc_context(discovery),
    });
    request["params"] = params;
    if let Some(deadline) = deadline_ms {
        request["deadline_ms"] = Value::from(deadline);
    }
    request
}

#[cfg(unix)]
pub fn rpc_cancel(discovery: &Discovery, request_id: &str) -> Value {
    json!({
        "protocol_version": "1.0",
        "kind": "cancel",
        "request_id": request_id,
        "reason": "client_cancelled",
        "context": rpc_context(discovery),
    })
}

#[cfg(unix)]
pub fn validate_rpc_response(
    discovery: &Discovery,
    response: &Value,
    request_id: &str,
) -> Result<()> {
    validate_instance("rpc.schema.json", response)?;
    require(
        response.get("kind").and_then(Value::as_str) == Some("response"),
        "expected an RPC response",
    )?;
    require(
        response.get("request_id").and_then(Value::as_str) == Some(request_id),
        format!("response request ID does not match {request_id}"),
    )?;
    require(
        response.get("context") == Some(&rpc_context(discovery)),
        "response context does not match the authenticated connection",
    )
}

#[cfg(unix)]
pub fn expect_error_code(
    discovery: &Discovery,
    response: &Value,
    request_id: &str,
    code: &str,
) -> Result<()> {
    validate_rpc_response(discovery, response, request_id)?;
    require(
        response.pointer("/error/code").and_then(Value::as_str) == Some(code),
        format!("expected RPC error {code} for {request_id}"),
    )
}

#[cfg(unix)]
pub fn expect_handshake_error(response: &Value, code: &str) -> Result<()> {
    validate_instance("handshake.schema.json", response)?;
    require(
        response.get("kind").and_then(Value::as_str) == Some("handshake.error"),
        "expected handshake.error",
    )?;
    require(
        response.pointer("/error/code").and_then(Value::as_str) == Some(code),
        format!("expected handshake error {code}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_prefix_is_big_endian_and_excludes_itself() {
        let frame = encode_frame(&json!({"ok": true})).unwrap();
        assert_eq!(
            u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
    }

    #[test]
    fn project_id_uses_the_domain_separator() {
        assert_eq!(
            project_id_for_root(b"/fixtures/codex-smoke"),
            "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
        );
    }
}
