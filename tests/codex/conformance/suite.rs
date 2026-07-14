use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::bundle::{validate_canonical_bundle, validate_instance};
use crate::discovery::Discovery;
use crate::error::{ConformanceError, Result, require};
use crate::protocol::{
    AuthenticatedConnection, FramedStream, MAX_PAYLOAD_BYTES, connect_authenticated, encode_frame,
    expect_error_code, expect_handshake_error, make_client_authenticate, make_client_hello,
    rpc_cancel, rpc_request, validate_rpc_response,
};
use crate::trace::TraceRecorder;

const INVALID_JSON_FIXTURE: &[u8] =
    include_bytes!("../../../schemas/codex_bridge/v1/fixtures/invalid/invalid-json.txt");

pub struct RunOptions {
    pub project_root: PathBuf,
    pub trace_path: PathBuf,
}

pub struct RunSummary {
    pub passed_cases: usize,
}

struct Suite<'a> {
    discovery: &'a Discovery,
    trace: TraceRecorder,
    passed_cases: usize,
}

impl Suite<'_> {
    fn pass(&mut self, name: &str) {
        self.trace.passed(name);
        self.passed_cases += 1;
    }

    fn record_handshake(&mut self, case: &str, connection: &AuthenticatedConnection) {
        for (direction, message) in &connection.messages {
            self.trace.record(case, direction, message);
        }
    }

    fn initialized_connection(&mut self, case: &str) -> Result<AuthenticatedConnection> {
        let mut connection = connect_authenticated(self.discovery, None)?;
        self.record_handshake(case, &connection);
        let request = initialize_request(self.discovery, &format!("req:{case}-init"));
        connection.stream.send_value(&request, None)?;
        let response = connection.stream.receive_value()?;
        validate_initialize_response(self.discovery, &response, &format!("req:{case}-init"))?;
        self.trace.record(case, "client", &request);
        self.trace.record(case, "server", &response);
        Ok(connection)
    }

    fn fragmented_lifecycle(&mut self) -> Result<()> {
        let case = "lifecycle.fragmented-happy-path";
        let mut connection = connect_authenticated(self.discovery, Some(1))?;
        self.record_handshake(case, &connection);

        let initialize = initialize_request(self.discovery, "req:happy-init");
        connection.stream.send_value(&initialize, Some(1))?;
        let initialize_response = connection.stream.receive_value()?;
        validate_initialize_response(self.discovery, &initialize_response, "req:happy-init")?;

        let ping = rpc_request(
            self.discovery,
            "req:happy-ping",
            "bridge.ping",
            json!({"echo": "fragmented-✓"}),
            None,
        );
        connection.stream.send_value(&ping, Some(3))?;
        let ping_response = connection.stream.receive_value()?;
        validate_rpc_response(self.discovery, &ping_response, "req:happy-ping")?;
        validate_instance(
            "lifecycle.schema.json#/$defs/pingResult",
            ping_response
                .get("result")
                .ok_or_else(|| ConformanceError("ping result is missing".to_owned()))?,
        )?;
        require(
            ping_response.pointer("/result/echo") == ping.pointer("/params/echo"),
            "bridge.ping did not return the exact echo",
        )?;

        let capabilities = rpc_request(
            self.discovery,
            "req:happy-capabilities",
            "bridge.capabilities",
            json!({}),
            None,
        );
        connection.stream.send_value(&capabilities, None)?;
        let capabilities_response = connection.stream.receive_value()?;
        validate_rpc_response(
            self.discovery,
            &capabilities_response,
            "req:happy-capabilities",
        )?;
        validate_instance(
            "lifecycle.schema.json#/$defs/capabilitiesResult",
            capabilities_response
                .get("result")
                .ok_or_else(|| ConformanceError("capabilities result is missing".to_owned()))?,
        )?;
        require_capabilities(&capabilities_response)?;

        let shutdown = rpc_request(
            self.discovery,
            "req:happy-shutdown",
            "bridge.shutdown",
            json!({"reason": "conformance_complete"}),
            None,
        );
        connection.stream.send_value(&shutdown, None)?;
        let shutdown_response = connection.stream.receive_value()?;
        validate_rpc_response(self.discovery, &shutdown_response, "req:happy-shutdown")?;
        validate_instance(
            "lifecycle.schema.json#/$defs/shutdownResult",
            shutdown_response
                .get("result")
                .ok_or_else(|| ConformanceError("shutdown result is missing".to_owned()))?,
        )?;
        connection.stream.expect_closed()?;
        self.discovery.assert_unchanged()?;

        for (direction, message) in [
            ("client", &initialize),
            ("server", &initialize_response),
            ("client", &ping),
            ("server", &ping_response),
            ("client", &capabilities),
            ("server", &capabilities_response),
            ("client", &shutdown),
            ("server", &shutdown_response),
        ] {
            self.trace.record(case, direction, message);
        }
        self.pass(case);
        Ok(())
    }

    fn lifecycle_sequencing_and_limits(&mut self) -> Result<()> {
        let case = "rpc.lifecycle-sequencing-and-limits";
        let mut connection = connect_authenticated(self.discovery, None)?;
        self.record_handshake(case, &connection);

        let before_initialize = rpc_request(
            self.discovery,
            "req:before-initialize",
            "bridge.ping",
            json!({"echo": "not-ready"}),
            None,
        );
        connection.stream.send_value(&before_initialize, None)?;
        let before_initialize_response = connection.stream.receive_value()?;
        expect_error_code(
            self.discovery,
            &before_initialize_response,
            "req:before-initialize",
            "not_initialized",
        )?;

        let initialize = initialize_request(self.discovery, "req:sequence-init");
        connection.stream.send_value(&initialize, None)?;
        let initialize_response = connection.stream.receive_value()?;
        validate_initialize_response(self.discovery, &initialize_response, "req:sequence-init")?;

        let duplicate_initialize = initialize_request(self.discovery, "req:sequence-init-again");
        connection.stream.send_value(&duplicate_initialize, None)?;
        let duplicate_initialize_response = connection.stream.receive_value()?;
        expect_error_code(
            self.discovery,
            &duplicate_initialize_response,
            "req:sequence-init-again",
            "already_initialized",
        )?;

        let maximum_echo = "é".repeat(128);
        require(
            maximum_echo.len() == 256,
            "maximum ping fixture is not exactly 256 UTF-8 bytes",
        )?;
        let maximum_ping = rpc_request(
            self.discovery,
            "req:ping-256-bytes",
            "bridge.ping",
            json!({"echo": maximum_echo}),
            None,
        );
        connection.stream.send_value(&maximum_ping, None)?;
        let maximum_ping_response = connection.stream.receive_value()?;
        validate_rpc_response(self.discovery, &maximum_ping_response, "req:ping-256-bytes")?;
        require(
            maximum_ping_response.pointer("/result/echo") == maximum_ping.pointer("/params/echo"),
            "bridge.ping changed the exact 256-byte UTF-8 echo",
        )?;

        let oversized_ping = rpc_request(
            self.discovery,
            "req:ping-258-bytes",
            "bridge.ping",
            json!({"echo": "é".repeat(129)}),
            None,
        );
        connection.stream.send_value(&oversized_ping, None)?;
        let oversized_ping_response = connection.stream.receive_value()?;
        expect_error_code(
            self.discovery,
            &oversized_ping_response,
            "req:ping-258-bytes",
            "invalid_request",
        )?;

        self.validate_invalid_deadline_limits(&mut connection)?;

        for (direction, message) in [
            ("client", &before_initialize),
            ("server", &before_initialize_response),
            ("client", &initialize),
            ("server", &initialize_response),
            ("client", &duplicate_initialize),
            ("server", &duplicate_initialize_response),
            ("client", &maximum_ping),
            ("server", &maximum_ping_response),
            ("client", &oversized_ping),
            ("server", &oversized_ping_response),
        ] {
            self.trace.record(case, direction, message);
        }
        self.pass(case);
        Ok(())
    }

    fn validate_invalid_deadline_limits(
        &self,
        connection: &mut AuthenticatedConnection,
    ) -> Result<()> {
        for (request_id, deadline_ms) in
            [("req:deadline-zero", 0), ("req:deadline-excessive", 30_001)]
        {
            let request = rpc_request(
                self.discovery,
                request_id,
                "bridge.ping",
                json!({"echo": "invalid-deadline"}),
                Some(deadline_ms),
            );
            connection.stream.send_value(&request, None)?;
            let response = connection.stream.receive_value()?;
            expect_error_code(self.discovery, &response, request_id, "invalid_request")?;
        }
        Ok(())
    }

    fn wrong_project(&mut self) -> Result<()> {
        let case = "handshake.wrong-project";
        let mut stream = FramedStream::connect(&self.discovery.endpoint)?;
        let wrong =
            "project:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let versions = vec!["1.0".to_owned()];
        let (hello, _) = make_client_hello(self.discovery, wrong, &versions)?;
        stream.send_value(&hello, Some(1))?;
        let response = stream.receive_value()?;
        expect_handshake_error(&response, "project_not_bound")?;
        stream.expect_closed()?;
        self.trace.record(case, "client", &hello);
        self.trace.record(case, "server", &response);
        self.pass(case);
        Ok(())
    }

    fn incompatible_version(&mut self) -> Result<()> {
        let case = "handshake.incompatible-major";
        let mut stream = FramedStream::connect(&self.discovery.endpoint)?;
        let versions = vec!["2.0".to_owned()];
        let (hello, _) = make_client_hello(self.discovery, &self.discovery.project_id, &versions)?;
        stream.send_value(&hello, None)?;
        let response = stream.receive_value()?;
        expect_handshake_error(&response, "protocol_mismatch")?;
        require(
            response
                .get("supported_protocol_versions")
                .and_then(Value::as_array)
                .is_some_and(|values| values.iter().any(|value| value == "1.0")),
            "protocol_mismatch did not include supported version 1.0",
        )?;
        stream.expect_closed()?;
        self.trace.record(case, "client", &hello);
        self.trace.record(case, "server", &response);
        self.pass(case);
        Ok(())
    }

    fn wrong_token(&mut self) -> Result<()> {
        let case = "handshake.wrong-token";
        let mut stream = FramedStream::connect(&self.discovery.endpoint)?;
        let versions = vec!["1.0".to_owned()];
        let (hello, client_nonce) =
            make_client_hello(self.discovery, &self.discovery.project_id, &versions)?;
        stream.send_value(&hello, None)?;
        let challenge = stream.receive_value()?;
        let mut wrong_token = self.discovery.token;
        wrong_token[0] ^= 0xff;
        let authenticate = make_client_authenticate(
            self.discovery,
            &versions,
            &client_nonce,
            &challenge,
            &wrong_token,
            false,
        )?;
        stream.send_value(&authenticate, None)?;
        let response = stream.receive_value()?;
        expect_handshake_error(&response, "unauthenticated")?;
        stream.expect_closed()?;
        for (direction, message) in [
            ("client", &hello),
            ("server", &challenge),
            ("client", &authenticate),
            ("server", &response),
        ] {
            self.trace.record(case, direction, message);
        }
        self.pass(case);
        Ok(())
    }

    fn maximum_payload(&mut self) -> Result<()> {
        let case = "framing.maximum-payload";
        let mut stream = FramedStream::connect(&self.discovery.endpoint)?;
        let versions = vec!["1.0".to_owned()];
        let (mut hello, _) =
            make_client_hello(self.discovery, &self.discovery.project_id, &versions)?;
        hello["padding"] = Value::String(String::new());
        let empty_length = serde_json::to_vec(&hello)?.len();
        require(
            empty_length < MAX_PAYLOAD_BYTES,
            "base maximum-payload hello is unexpectedly large",
        )?;
        hello["padding"] = Value::String("x".repeat(MAX_PAYLOAD_BYTES - empty_length));
        let frame = encode_frame(&hello)?;
        require(
            frame.len() == MAX_PAYLOAD_BYTES + 4,
            "generated maximum-size frame is not exactly 1 MiB",
        )?;
        stream.send_bytes(&frame, Some(4096))?;
        let challenge = stream.receive_value()?;
        require(
            challenge.get("kind").and_then(Value::as_str) == Some("handshake.server_challenge"),
            "server rejected the maximum valid frame length",
        )?;
        self.trace.record(case, "server", &challenge);
        self.pass(case);
        Ok(())
    }

    fn invalid_framing(&mut self) -> Result<()> {
        let cases: [(&str, Vec<u8>); 3] = [
            ("framing.zero-length", vec![0, 0, 0, 0]),
            (
                "framing.oversized-payload",
                (u32::try_from(MAX_PAYLOAD_BYTES).expect("protocol limit fits u32") + 1)
                    .to_be_bytes()
                    .to_vec(),
            ),
            ("framing.invalid-json", {
                let mut frame = Vec::with_capacity(INVALID_JSON_FIXTURE.len() + 4);
                frame.extend_from_slice(
                    &u32::try_from(INVALID_JSON_FIXTURE.len())
                        .expect("fixture length fits u32")
                        .to_be_bytes(),
                );
                frame.extend_from_slice(INVALID_JSON_FIXTURE);
                frame
            }),
        ];
        for (case, bytes) in cases {
            let mut stream = FramedStream::connect(&self.discovery.endpoint)?;
            stream.send_bytes(&bytes, Some(1))?;
            stream.expect_closed()?;
            self.pass(case);
        }
        Ok(())
    }

    fn unknown_method_and_duplicate_id(&mut self) -> Result<()> {
        let case = "rpc.unknown-method-and-duplicate-id";
        let mut connection = self.initialized_connection(case)?;

        let unknown = rpc_request(
            self.discovery,
            "req:unknown",
            "bridge.unknown",
            json!({}),
            None,
        );
        connection.stream.send_value(&unknown, None)?;
        let unknown_response = connection.stream.receive_value()?;
        expect_error_code(
            self.discovery,
            &unknown_response,
            "req:unknown",
            "method_not_found",
        )?;

        let ping = rpc_request(
            self.discovery,
            "req:duplicate",
            "bridge.ping",
            json!({"echo": "first"}),
            None,
        );
        connection.stream.send_value(&ping, None)?;
        let first = connection.stream.receive_value()?;
        validate_rpc_response(self.discovery, &first, "req:duplicate")?;
        require(
            first.pointer("/result/echo").and_then(Value::as_str) == Some("first"),
            "first duplicate-ID request did not complete",
        )?;
        connection.stream.send_value(&ping, None)?;
        let duplicate = connection.stream.receive_value()?;
        expect_error_code(
            self.discovery,
            &duplicate,
            "req:duplicate",
            "duplicate_request_id",
        )?;

        for (direction, message) in [
            ("client", &unknown),
            ("server", &unknown_response),
            ("client", &ping),
            ("server", &first),
            ("client", &ping),
            ("server", &duplicate),
        ] {
            self.trace.record(case, direction, message);
        }
        self.pass(case);
        Ok(())
    }

    fn cancellation(&mut self) -> Result<()> {
        let case = "rpc.cancellation";
        let mut connection = self.initialized_connection(case)?;
        let request = rpc_request(
            self.discovery,
            "req:cancel",
            "bridge.ping",
            json!({"echo": "cancel-before-main"}),
            Some(30_000),
        );
        let cancel = rpc_cancel(self.discovery, "req:cancel");
        connection
            .stream
            .send_values_coalesced(&[request.clone(), cancel.clone()])?;
        let response = connection.stream.receive_value()?;
        expect_error_code(self.discovery, &response, "req:cancel", "cancelled")?;
        self.trace.record(case, "client", &request);
        self.trace.record(case, "client", &cancel);
        self.trace.record(case, "server", &response);
        self.pass(case);
        Ok(())
    }

    fn deadlines(&mut self) -> Result<()> {
        let case = "rpc.deadline-and-terminal-once";
        let mut connection = self.initialized_connection(case)?;
        let requests = (0..32)
            .map(|index| {
                rpc_request(
                    self.discovery,
                    &format!("req:deadline-{index}"),
                    "bridge.ping",
                    json!({"echo": index.to_string()}),
                    Some(1),
                )
            })
            .collect::<Vec<_>>();
        connection.stream.send_values_coalesced(&requests)?;
        let responses = receive_responses(self.discovery, &mut connection.stream, 32)?;
        let deadline_count = responses
            .values()
            .filter(|response| {
                response.pointer("/error/code").and_then(Value::as_str) == Some("deadline_exceeded")
            })
            .count();
        require(deadline_count > 0, "no 1 ms request reached its deadline")?;
        self.pass(case);
        Ok(())
    }

    fn saturation(&mut self) -> Result<()> {
        const REQUEST_COUNT: usize = 256;

        let case = "rpc.saturation";
        let mut connection = self.initialized_connection(case)?;
        let requests = (0..REQUEST_COUNT)
            .map(|index| {
                rpc_request(
                    self.discovery,
                    &format!("req:saturation-{index}"),
                    "bridge.ping",
                    json!({"echo": index.to_string()}),
                    Some(30_000),
                )
            })
            .collect::<Vec<_>>();
        connection.stream.send_values_coalesced(&requests)?;
        let responses = receive_responses(self.discovery, &mut connection.stream, REQUEST_COUNT)?;
        let overloaded = responses
            .values()
            .filter(|response| {
                response.pointer("/error/code").and_then(Value::as_str) == Some("overloaded")
            })
            .count();
        require(overloaded >= 1, "burst requests did not trigger overload")?;
        self.pass(case);
        Ok(())
    }

    fn abrupt_disconnect_and_recovery(&mut self) -> Result<()> {
        let case = "rpc.abrupt-disconnect-recovery";
        let mut connection = self.initialized_connection(case)?;
        let pending = (0..16)
            .map(|index| {
                rpc_request(
                    self.discovery,
                    &format!("req:abrupt-{index}"),
                    "bridge.ping",
                    json!({"echo": index.to_string()}),
                    Some(30_000),
                )
            })
            .collect::<Vec<_>>();
        connection.stream.send_values_coalesced(&pending)?;
        drop(connection);

        let mut recovered = self.initialized_connection(case)?;
        let request = rpc_request(
            self.discovery,
            "req:recovered",
            "bridge.ping",
            json!({"echo": "healthy-after-disconnect"}),
            None,
        );
        recovered.stream.send_value(&request, None)?;
        let response = recovered.stream.receive_value()?;
        validate_rpc_response(self.discovery, &response, "req:recovered")?;
        require(
            response.pointer("/result/echo").and_then(Value::as_str)
                == Some("healthy-after-disconnect"),
            "bridge did not recover after abrupt disconnect",
        )?;
        self.trace.record(case, "client", &request);
        self.trace.record(case, "server", &response);
        self.pass(case);
        Ok(())
    }
}

fn initialize_request(discovery: &Discovery, request_id: &str) -> Value {
    rpc_request(
        discovery,
        request_id,
        "bridge.initialize",
        json!({
            "client": {
                "name": "codex-bridge-conformance",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "requested_capabilities": ["bridge.lifecycle", "transport.uds"],
        }),
        None,
    )
}

fn validate_initialize_response(
    discovery: &Discovery,
    response: &Value,
    request_id: &str,
) -> Result<()> {
    validate_rpc_response(discovery, response, request_id)?;
    let result = response
        .get("result")
        .ok_or_else(|| ConformanceError("initialize result is missing".to_owned()))?;
    validate_instance("lifecycle.schema.json#/$defs/initializeResult", result)?;
    require(
        result.get("protocol_version").and_then(Value::as_str) == Some("1.0"),
        "initialize selected an unexpected protocol version",
    )?;
    require(
        result.get("project_id").and_then(Value::as_str) == Some(&discovery.project_id),
        "initialize project binding mismatch",
    )?;
    require(
        result.get("editor_session_id").and_then(Value::as_str)
            == Some(&discovery.editor_session_id),
        "initialize editor session binding mismatch",
    )?;
    require(
        result
            .pointer("/revisions/event_seq")
            .and_then(Value::as_u64)
            == Some(0)
            && result
                .pointer("/revisions/project_revision")
                .and_then(Value::as_u64)
                == Some(0)
            && result
                .pointer("/revisions/operation_seq")
                .and_then(Value::as_u64)
                == Some(0),
        "initialize revisions are not session-scoped zero values",
    )?;
    require_capabilities(response)
}

fn require_capabilities(response: &Value) -> Result<()> {
    let capabilities = response
        .pointer("/result/capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| ConformanceError("capability list is missing".to_owned()))?;
    let named = capabilities
        .iter()
        .filter_map(|capability| {
            Some((
                capability.get("name")?.as_str()?,
                capability.get("version")?.as_str()?,
                capability.get("readiness")?.as_str()?,
            ))
        })
        .collect::<HashSet<_>>();
    require(
        named.contains(&("bridge.lifecycle", "1.0", "ready")),
        "bridge.lifecycle 1.0 is not ready",
    )?;
    require(
        named.contains(&("transport.uds", "1.0", "ready")),
        "transport.uds 1.0 is not ready",
    )?;
    require(
        response
            .pointer("/result/limits/frame_bytes")
            .and_then(Value::as_u64)
            == Some(MAX_PAYLOAD_BYTES as u64),
        "server frame limit differs from the protocol",
    )?;
    require(
        response
            .pointer("/result/limits/max_in_flight_requests")
            .and_then(Value::as_u64)
            == Some(64),
        "server in-flight limit differs from the protocol",
    )
}

fn receive_responses(
    discovery: &Discovery,
    stream: &mut FramedStream,
    expected: usize,
) -> Result<HashMap<String, Value>> {
    let mut responses = HashMap::with_capacity(expected);
    for _ in 0..expected {
        let response = stream.receive_value()?;
        let request_id = response
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ConformanceError("response has no request ID".to_owned()))?
            .to_owned();
        validate_rpc_response(discovery, &response, &request_id)?;
        require(
            responses.insert(request_id.clone(), response).is_none(),
            format!("request {request_id} received more than one terminal response"),
        )?;
    }
    require(
        responses.len() == expected,
        "did not receive exactly one terminal response per request",
    )?;
    Ok(responses)
}

/// Runs the complete Sprint 1 live conformance suite.
///
/// # Errors
///
/// Returns an error when discovery is unsafe, a schema/vector check fails, the
/// bridge violates the wire contract, or the canonical trace cannot be written.
pub fn run(options: &RunOptions) -> Result<RunSummary> {
    let manifest_cases = validate_canonical_bundle()?;
    let discovery = Discovery::load(&options.project_root)?;
    let mut suite = Suite {
        trace: TraceRecorder::new(&discovery.canonical_root, discovery.secret_fingerprints())?,
        discovery: &discovery,
        passed_cases: manifest_cases,
    };
    suite.trace.passed("canonical.schemas-and-fixtures");
    suite.trace.passed("discovery.private-project-binding");
    suite.passed_cases += 1;

    suite.fragmented_lifecycle()?;
    suite.lifecycle_sequencing_and_limits()?;
    suite.wrong_project()?;
    suite.incompatible_version()?;
    suite.wrong_token()?;
    suite.maximum_payload()?;
    suite.invalid_framing()?;
    suite.unknown_method_and_duplicate_id()?;
    suite.cancellation()?;
    suite.deadlines()?;
    suite.saturation()?;
    suite.abrupt_disconnect_and_recovery()?;
    discovery.assert_unchanged()?;
    suite.trace.write_verified(&options.trace_path)?;
    Ok(RunSummary {
        passed_cases: suite.passed_cases,
    })
}
