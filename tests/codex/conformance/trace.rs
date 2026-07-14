use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::error::{ConformanceError, Result, require};

pub struct TraceRecorder {
    project_root: String,
    secrets: Vec<String>,
    events: Vec<Value>,
}

impl TraceRecorder {
    pub fn new(project_root: &Path, secret_fingerprints: Vec<String>) -> Result<Self> {
        let project_root = project_root
            .to_str()
            .ok_or_else(|| ConformanceError("trace project root is not UTF-8".to_owned()))?
            .to_owned();
        Ok(Self {
            project_root,
            secrets: secret_fingerprints,
            events: Vec::new(),
        })
    }

    pub fn record(&mut self, case: &str, direction: &str, message: &Value) {
        collect_sensitive_values(message, &mut self.secrets);
        let redacted = redact(message, &self.project_root);
        self.events.push(json!({
            "case": case,
            "direction": direction,
            "message": redacted,
        }));
    }

    pub fn passed(&mut self, case: &str) {
        self.events.push(json!({
            "case": case,
            "direction": "local",
            "message": {
                "kind": "conformance.result",
                "outcome": "passed",
            },
        }));
    }

    fn render(&self) -> Result<Vec<u8>> {
        let document = json!({
            "events": self.events,
            "protocol_version": "1.0",
            "schema_version": 1,
            "suite": "sprint-1-stage-5",
        });
        let mut bytes = serde_json::to_vec_pretty(&document)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn write_verified(&self, path: &Path) -> Result<()> {
        let bytes = self.render()?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|error| ConformanceError(format!("trace is not UTF-8: {error}")))?;
        require(
            !text.contains(&self.project_root),
            "trace contains the absolute project root",
        )?;
        for secret in &self.secrets {
            if !secret.is_empty() {
                require(
                    !text.contains(secret),
                    "trace contains a handshake or token secret",
                )?;
            }
        }
        let parent = path
            .parent()
            .ok_or_else(|| ConformanceError("trace path has no parent".to_owned()))?;
        fs::create_dir_all(parent)?;
        let temporary = temporary_path(path);
        fs::write(&temporary, &bytes)?;
        fs::rename(&temporary, path)?;
        let readback = fs::read(path)?;
        require(
            readback == bytes,
            "trace readback differs from canonical bytes",
        )
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(".tmp");
    PathBuf::from(value)
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("token") || key.contains("proof") || key.contains("nonce")
}

fn is_path_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("path") || key == "endpoint" || key == "project_root"
}

fn collect_sensitive_values(value: &Value, secrets: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if is_secret_key(key) {
                    if let Some(value) = child.as_str() {
                        secrets.push(value.to_owned());
                    }
                } else {
                    collect_sensitive_values(child, secrets);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_sensitive_values(child, secrets);
            }
        }
        _ => {}
    }
}

fn redact(value: &Value, project_root: &str) -> Value {
    match value {
        Value::Object(object) => {
            let mut output = Map::new();
            for (key, child) in object {
                let replacement = if is_secret_key(key) {
                    Value::String("<redacted-secret>".to_owned())
                } else if is_path_key(key) {
                    Value::String("<redacted-path>".to_owned())
                } else if key == "project_id" {
                    Value::String("<project-id>".to_owned())
                } else if key == "editor_session_id" {
                    Value::String("<editor-session-id>".to_owned())
                } else {
                    redact(child, project_root)
                };
                output.insert(key.clone(), replacement);
            }
            Value::Object(output)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|child| redact(child, project_root))
                .collect(),
        ),
        Value::String(text) if text.contains(project_root) => {
            Value::String("<redacted-path>".to_owned())
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_trace_redacts_secrets_bindings_and_paths() {
        let mut recorder =
            TraceRecorder::new(Path::new("/private/project"), vec!["raw-token".to_owned()])
                .unwrap();
        recorder.record(
            "redaction",
            "client",
            &json!({
                "project_id": "project:sha256:secret",
                "editor_session_id": "editor:secret",
                "client_nonce": "nonce-secret",
                "client_proof": "proof-secret",
                "token_file": "/private/project/.godot/codex/session.token",
                "nested": { "value": "/private/project/file" },
            }),
        );
        let rendered = String::from_utf8(recorder.render().unwrap()).unwrap();
        for forbidden in [
            "/private/project",
            "raw-token",
            "project:sha256:secret",
            "editor:secret",
            "nonce-secret",
            "proof-secret",
        ] {
            assert!(!rendered.contains(forbidden));
        }
        assert!(rendered.contains("<redacted-secret>"));
        assert!(rendered.contains("<redacted-path>"));
    }
}
