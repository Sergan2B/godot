use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use godot_codex_bridge_client::{
    BridgeClient, BridgeError, RuntimeEntity, RuntimeEvent, RuntimeEventType, RuntimeInvalidated,
    RuntimeNotification, RuntimeSnapshot, RuntimeStateResult,
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct CachedRuntimeSnapshot {
    pub project_id: String,
    pub editor_session_id: String,
    pub snapshot: RuntimeSnapshot,
}

#[derive(Debug)]
struct RuntimeCache {
    summary: Value,
    snapshot: Option<Arc<CachedRuntimeSnapshot>>,
}

impl Default for RuntimeCache {
    fn default() -> Self {
        Self {
            summary: json!({
                "schema_version": "runtime-summary/1.0",
                "status": "unavailable",
                "freshness": "unavailable",
                "runtime_session_id": null,
                "runtime_event_seq": null,
                "state": null,
                "truncated": false,
                "omitted_counts": {},
                "evidence": [],
            }),
            snapshot: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeOverlay {
    project_root: Option<Arc<PathBuf>>,
    cache: Arc<RwLock<RuntimeCache>>,
}

impl RuntimeOverlay {
    pub(crate) fn unavailable() -> Self {
        Self {
            project_root: None,
            cache: Arc::new(RwLock::new(RuntimeCache::default())),
        }
    }

    pub(crate) fn for_project(project_root: PathBuf) -> Self {
        let overlay = Self {
            project_root: Some(Arc::new(project_root)),
            cache: Arc::new(RwLock::new(RuntimeCache::default())),
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            let watcher = overlay.clone();
            tokio::spawn(async move { watcher.watch_notifications().await });
        }
        overlay
    }

    pub(crate) async fn connect(&self) -> Result<BridgeClient, BridgeError> {
        let root = self.project_root.as_deref().ok_or_else(|| {
            BridgeError::Invalid("runtime bridge is not configured for this server".to_owned())
        })?;
        BridgeClient::connect(Path::new(root.as_path())).await
    }

    pub(crate) fn record_state(&self, state: &RuntimeStateResult) {
        let summary = json!({
            "schema_version": "runtime-summary/1.0",
            "status": "ready",
            "freshness": "current",
            "runtime_session_id": state.runtime_session_id,
            "runtime_event_seq": state.runtime_event_seq,
            "state": state.state,
            "origin": state.origin,
            "target": state.target,
            "scene_path": state.scene_path,
            "tree_nodes": null,
            "diagnostics": null,
            "stacks": null,
            "truncated": false,
            "omitted_counts": {},
            "evidence": [{"source": "bridge_runtime_state", "freshness": "current"}],
        });
        let mut cache = self.cache.write().expect("runtime cache lock poisoned");
        cache.summary = summary;
        cache.snapshot = None;
    }

    pub(crate) fn record_snapshot(
        &self,
        project_id: String,
        editor_session_id: String,
        snapshot: RuntimeSnapshot,
    ) -> Arc<CachedRuntimeSnapshot> {
        let mut tree_nodes = 0_usize;
        let mut diagnostics = 0_usize;
        let mut stacks = 0_usize;
        let mut state_entity = None;
        for entity in &snapshot.entities {
            match entity {
                RuntimeEntity::RuntimeState(value) => state_entity = Some(value),
                RuntimeEntity::RuntimeNode(_) => tree_nodes += 1,
                RuntimeEntity::RuntimeDiagnostic(_) => diagnostics += 1,
                RuntimeEntity::RuntimeStack(_) => stacks += 1,
            }
        }
        let summary = json!({
            "schema_version": "runtime-summary/1.0",
            "status": "ready",
            "freshness": "current",
            "project_id": project_id,
            "editor_session_id": editor_session_id,
            "runtime_session_id": snapshot.accepted.runtime_session_id,
            "runtime_event_seq": snapshot.accepted.runtime_event_seq,
            "state": snapshot.accepted.state,
            "runtime_state": state_entity,
            "tree_nodes": tree_nodes,
            "diagnostics": diagnostics,
            "stacks": stacks,
            "truncated": snapshot.accepted.limits_applied.truncated,
            "omitted_counts": {},
            "evidence": [{"source": "checksum_verified_runtime_snapshot", "snapshot_id": snapshot.accepted.snapshot_id}],
        });
        let cached = Arc::new(CachedRuntimeSnapshot {
            project_id,
            editor_session_id,
            snapshot,
        });
        let mut cache = self.cache.write().expect("runtime cache lock poisoned");
        cache.summary = summary;
        cache.snapshot = Some(cached.clone());
        cached
    }

    pub(crate) fn snapshot(&self) -> Option<Arc<CachedRuntimeSnapshot>> {
        self.cache
            .read()
            .expect("runtime cache lock poisoned")
            .snapshot
            .clone()
    }

    pub(crate) fn invalidate(&self, reason: &str) {
        let mut cache = self.cache.write().expect("runtime cache lock poisoned");
        let previous = cache.summary.clone();
        cache.summary = json!({
            "schema_version": "runtime-summary/1.0",
            "status": "invalidated",
            "freshness": "stale",
            "runtime_session_id": previous.get("runtime_session_id"),
            "runtime_event_seq": previous.get("runtime_event_seq"),
            "state": previous.get("state"),
            "reason": reason,
            "truncated": false,
            "omitted_counts": {},
            "evidence": [],
        });
        cache.snapshot = None;
    }

    fn record_event(&self, event: &RuntimeEvent) {
        let mut cache = self.cache.write().expect("runtime cache lock poisoned");
        let previous = cache.summary.clone();
        let previous_session = previous.get("runtime_session_id").and_then(Value::as_str);
        let previous_seq = previous.get("runtime_event_seq").and_then(Value::as_u64);
        let session_changed = previous_session != Some(event.runtime_session_id.as_str());
        let gap = previous_session == Some(event.runtime_session_id.as_str())
            && previous_seq.is_some_and(|seq| event.runtime_event_seq > seq.saturating_add(1));
        let reconnect = previous.get("state") == Some(&Value::String("disconnected".to_owned()));
        let requires_full_snapshot = session_changed
            || gap
            || reconnect
            || event.event_type == RuntimeEventType::Disconnected;
        if cache.snapshot.as_ref().is_some_and(|cached| {
            cached.snapshot.accepted.runtime_session_id != event.runtime_session_id
                || cached.snapshot.accepted.runtime_event_seq != event.runtime_event_seq
        }) {
            cache.snapshot = None;
        }
        cache.summary = json!({
            "schema_version": "runtime-summary/1.0",
            "status": "ready",
            "freshness": "current",
            "runtime_session_id": event.runtime_session_id,
            "runtime_event_seq": event.runtime_event_seq,
            "state": event.state,
            "event_type": event.event_type,
            "origin": (!session_changed).then(|| previous.get("origin")).flatten(),
            "target": (!session_changed).then(|| previous.get("target")).flatten(),
            "scene_path": (!session_changed).then(|| previous.get("scene_path")).flatten(),
            "tree_nodes": null,
            "diagnostics": null,
            "stacks": null,
            "truncated": false,
            "omitted_counts": {},
            "requires_full_snapshot": requires_full_snapshot,
            "evidence": [{"source": "runtime_event_stream", "event_seq": event.runtime_event_seq}],
        });
    }

    fn record_invalidated(&self, invalidated: &RuntimeInvalidated) {
        let mut cache = self.cache.write().expect("runtime cache lock poisoned");
        cache.summary = json!({
            "schema_version": "runtime-summary/1.0",
            "status": "invalidated",
            "freshness": "stale",
            "runtime_session_id": invalidated.runtime_session_id,
            "runtime_event_seq": invalidated.last_contiguous_runtime_event_seq,
            "state": null,
            "reason": invalidated.reason,
            "truncated": false,
            "omitted_counts": {},
            "requires_full_snapshot": true,
            "evidence": [{"source": "runtime_event_stream"}],
        });
        cache.snapshot = None;
    }

    async fn watch_notifications(self) {
        let mut retry = Duration::from_millis(100);
        loop {
            let mut client = match self.connect().await {
                Ok(client) if client.negotiated_profile().runtime_available => client,
                _ => {
                    tokio::time::sleep(retry).await;
                    retry = (retry * 2).min(Duration::from_secs(2));
                    continue;
                }
            };
            retry = Duration::from_millis(100);
            loop {
                match client.next_runtime_notification().await {
                    Ok(RuntimeNotification::Event(event)) => self.record_event(&event),
                    Ok(RuntimeNotification::Invalidated(invalidated)) => {
                        self.record_invalidated(&invalidated);
                    }
                    Err(_) => {
                        if self.snapshot().is_some() {
                            self.invalidate("runtime_event_stream_disconnected");
                        }
                        break;
                    }
                }
            }
        }
    }

    pub(crate) fn summary(&self) -> Value {
        self.cache
            .read()
            .expect("runtime cache lock poisoned")
            .summary
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use godot_codex_bridge_client::{
        RuntimeDomain, RuntimeInvalidationReason, RuntimeOrigin, RuntimeRevisionVector,
        RuntimeState, RuntimeTarget,
    };

    use super::*;

    fn state(session: &str, sequence: u64, runtime_state: RuntimeState) -> RuntimeStateResult {
        RuntimeStateResult {
            schema_version: "runtime/1.0".to_owned(),
            project_id: format!("project:sha256:{}", "1".repeat(64)),
            editor_session_id: format!("editor:{}", "2".repeat(32)),
            runtime_session_id: session.to_owned(),
            runtime_event_seq: sequence,
            state: runtime_state,
            origin: RuntimeOrigin::Mcp,
            target: RuntimeTarget::Project,
            scene_path: Some("res://main.tscn".to_owned()),
        }
    }

    fn event(
        session: &str,
        sequence: u64,
        event_type: RuntimeEventType,
        runtime_state: RuntimeState,
    ) -> RuntimeEvent {
        RuntimeEvent {
            runtime_session_id: session.to_owned(),
            runtime_event_seq: sequence,
            event_type,
            state: runtime_state,
            changed_domains: vec![RuntimeDomain::RuntimeState],
            revisions: RuntimeRevisionVector {
                editor_session_id: format!("editor:{}", "2".repeat(32)),
                event_seq: sequence,
                project_revision: 1,
                operation_seq: 1,
                resource_revision: 1,
                scene_graph_revision: 1,
                script_graph_revision: 1,
                scene_revisions: BTreeMap::new(),
                runtime_session_id: session.to_owned(),
                runtime_event_seq: sequence,
            },
        }
    }

    #[test]
    fn event_gap_and_session_change_require_a_full_snapshot() {
        let overlay = RuntimeOverlay::unavailable();
        let first_session = format!("runtime:{}", "3".repeat(32));
        overlay.record_state(&state(&first_session, 2, RuntimeState::Running));
        overlay.record_event(&event(
            &first_session,
            4,
            RuntimeEventType::DiagnosticAdded,
            RuntimeState::Running,
        ));
        assert_eq!(overlay.summary()["requires_full_snapshot"], true);

        let second_session = format!("runtime:{}", "4".repeat(32));
        overlay.record_event(&event(
            &second_session,
            1,
            RuntimeEventType::SessionStarted,
            RuntimeState::Starting,
        ));
        let summary = overlay.summary();
        assert_eq!(summary["requires_full_snapshot"], true);
        assert_eq!(summary["origin"], Value::Null);
        assert_eq!(summary["scene_path"], Value::Null);
    }

    #[test]
    fn disconnect_and_reconnect_retire_cache_until_a_full_snapshot() {
        let overlay = RuntimeOverlay::unavailable();
        let session = format!("runtime:{}", "5".repeat(32));
        overlay.record_state(&state(&session, 1, RuntimeState::Running));
        overlay.record_event(&event(
            &session,
            2,
            RuntimeEventType::Disconnected,
            RuntimeState::Disconnected,
        ));
        assert_eq!(overlay.summary()["requires_full_snapshot"], true);

        overlay.record_event(&event(
            &session,
            3,
            RuntimeEventType::DebuggerConnected,
            RuntimeState::Running,
        ));
        assert_eq!(overlay.summary()["requires_full_snapshot"], true);
    }

    #[test]
    fn explicit_invalidation_is_stale_and_clears_runtime_data() {
        let overlay = RuntimeOverlay::unavailable();
        let session = format!("runtime:{}", "6".repeat(32));
        overlay.record_state(&state(&session, 8, RuntimeState::Running));
        overlay.record_invalidated(&RuntimeInvalidated {
            runtime_session_id: session,
            last_contiguous_runtime_event_seq: 8,
            reason: RuntimeInvalidationReason::JournalGap,
        });
        let summary = overlay.summary();
        assert_eq!(summary["status"], "invalidated");
        assert_eq!(summary["freshness"], "stale");
        assert_eq!(summary["requires_full_snapshot"], true);
        assert!(overlay.snapshot().is_none());
    }

    #[test]
    fn reconnect_invalidation_preserves_session_coordinate_but_requires_snapshot() {
        let overlay = RuntimeOverlay::unavailable();
        let session = format!("runtime:{}", "7".repeat(32));
        overlay.record_state(&state(&session, 8, RuntimeState::Disconnected));
        overlay.record_invalidated(&RuntimeInvalidated {
            runtime_session_id: session.clone(),
            last_contiguous_runtime_event_seq: 8,
            reason: RuntimeInvalidationReason::Reconnected,
        });
        let summary = overlay.summary();
        assert_eq!(summary["runtime_session_id"], session);
        assert_eq!(summary["runtime_event_seq"], 8);
        assert_eq!(summary["reason"], "reconnected");
        assert_eq!(summary["requires_full_snapshot"], true);
        assert!(overlay.snapshot().is_none());
    }
}
