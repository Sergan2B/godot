use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use godot_codex_bridge_client::{
    BridgeError, ScriptSnapshot, ScriptSnapshotAccepted, ScriptSnapshotBeginParams,
    ScriptSnapshotChunk, ScriptSnapshotEndParams, ScriptSnapshotPayload, ScriptSnapshotSink,
};

use crate::IndexerError;

const MAX_SPOOL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
static NEXT_SPOOL_ID: AtomicU64 = AtomicU64::new(1);

/// Disk-backed, bounded receiver for a validated Bridge script snapshot.
/// Incomplete files stay in staging and are deleted on every drop path.
pub struct ScriptSnapshotSpool {
    path: PathBuf,
    file: File,
    bytes_written: u64,
    accepted: Option<ScriptSnapshotAccepted>,
    begin: Option<ScriptSnapshotBeginParams>,
    end: Option<ScriptSnapshotEndParams>,
}

impl ScriptSnapshotSpool {
    pub fn create(staging_directory: &Path) -> Result<Self, IndexerError> {
        fs::create_dir_all(staging_directory)
            .map_err(|_| IndexerError::Spool("script_staging_directory_unavailable"))?;
        for _ in 0..32 {
            let id = NEXT_SPOOL_ID.fetch_add(1, Ordering::Relaxed);
            let path = staging_directory.join(format!(
                "script-snapshot-{}-{id:016x}.spool",
                std::process::id()
            ));
            match OpenOptions::new()
                .write(true)
                .read(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        bytes_written: 0,
                        accepted: None,
                        begin: None,
                        end: None,
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(IndexerError::Spool("script_staging_file_unavailable")),
            }
        }
        Err(IndexerError::Spool("script_staging_name_exhausted"))
    }

    pub fn confirmed_snapshot(&mut self) -> Result<ScriptSnapshot, IndexerError> {
        let accepted = self
            .accepted
            .clone()
            .ok_or(IndexerError::Spool("script_snapshot_acceptance_missing"))?;
        let begin = self
            .begin
            .clone()
            .ok_or(IndexerError::Spool("script_snapshot_begin_missing"))?;
        let end = self
            .end
            .clone()
            .ok_or(IndexerError::Spool("script_snapshot_end_missing"))?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| IndexerError::Spool("script_staging_seek_failed"))?;
        let mut documents = Vec::with_capacity(end.document_count);
        let mut symbols = Vec::with_capacity(end.symbol_count);
        let mut relations = Vec::with_capacity(end.relation_count);
        let mut diagnostics = Vec::with_capacity(end.diagnostic_count);
        let mut adapter_statuses = Vec::with_capacity(end.adapter_status_count);
        let mut chunks = 0_usize;
        loop {
            let mut length = [0_u8; 4];
            match self.file.read_exact(&mut length) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => break,
                Err(_) => return Err(IndexerError::Spool("script_staging_read_failed")),
            }
            let length = usize::try_from(u32::from_be_bytes(length))
                .map_err(|_| IndexerError::Spool("script_staging_chunk_too_large"))?;
            let mut payload = vec![0_u8; length];
            self.file
                .read_exact(&mut payload)
                .map_err(|_| IndexerError::Spool("script_staging_chunk_truncated"))?;
            let payload: ScriptSnapshotPayload = serde_json::from_slice(&payload)
                .map_err(|_| IndexerError::Spool("script_staging_payload_invalid"))?;
            documents.extend(payload.documents);
            symbols.extend(payload.symbols);
            relations.extend(payload.relations);
            diagnostics.extend(payload.diagnostics);
            adapter_statuses.extend(payload.adapter_statuses);
            chunks = chunks
                .checked_add(1)
                .ok_or(IndexerError::Spool("script_staging_chunk_overflow"))?;
        }
        if chunks != end.chunk_count
            || documents.len() != end.document_count
            || symbols.len() != end.symbol_count
            || relations.len() != end.relation_count
            || diagnostics.len() != end.diagnostic_count
            || adapter_statuses.len() != end.adapter_status_count
        {
            return Err(IndexerError::Spool("script_staging_count_mismatch"));
        }
        Ok(ScriptSnapshot {
            accepted,
            begin,
            payload: ScriptSnapshotPayload {
                documents,
                symbols,
                relations,
                diagnostics,
                adapter_statuses,
            },
            end,
        })
    }

    fn sink_error(code: &'static str) -> BridgeError {
        BridgeError::Invalid(format!("script snapshot spool failed: {code}"))
    }
}

impl ScriptSnapshotSink for ScriptSnapshotSpool {
    fn begin(
        &mut self,
        accepted: &ScriptSnapshotAccepted,
        begin: &ScriptSnapshotBeginParams,
    ) -> Result<(), BridgeError> {
        if self.accepted.is_some() || self.begin.is_some() || self.end.is_some() {
            return Err(Self::sink_error("duplicate_begin"));
        }
        self.accepted = Some(accepted.clone());
        self.begin = Some(begin.clone());
        Ok(())
    }

    fn chunk(&mut self, chunk: &ScriptSnapshotChunk) -> Result<(), BridgeError> {
        if self.begin.is_none() || self.end.is_some() {
            return Err(Self::sink_error("chunk_outside_snapshot"));
        }
        let payload = chunk.payload_json.as_bytes();
        let length =
            u32::try_from(payload.len()).map_err(|_| Self::sink_error("chunk_too_large"))?;
        let next_size = self
            .bytes_written
            .checked_add(4)
            .and_then(|value| value.checked_add(u64::from(length)))
            .ok_or_else(|| Self::sink_error("size_overflow"))?;
        if next_size > MAX_SPOOL_BYTES {
            return Err(Self::sink_error("size_limit"));
        }
        self.file
            .write_all(&length.to_be_bytes())
            .and_then(|()| self.file.write_all(payload))
            .map_err(|_| Self::sink_error("write_failed"))?;
        self.bytes_written = next_size;
        Ok(())
    }

    fn end(&mut self, end: &ScriptSnapshotEndParams) -> Result<(), BridgeError> {
        if self.begin.is_none() || self.end.is_some() {
            return Err(Self::sink_error("invalid_end"));
        }
        self.file
            .flush()
            .and_then(|()| self.file.sync_all())
            .map_err(|_| Self::sink_error("sync_failed"))?;
        self.end = Some(end.clone());
        Ok(())
    }
}

impl Drop for ScriptSnapshotSpool {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use godot_codex_bridge_client::{RpcContext, ScriptRevisionVector, ScriptSnapshotLimits};
    use tempfile::TempDir;

    use super::*;

    fn revision() -> ScriptRevisionVector {
        ScriptRevisionVector {
            editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
            event_seq: 1,
            project_revision: 1,
            operation_seq: 1,
            resource_revision: 1,
            scene_graph_revision: 1,
            script_graph_revision: 1,
            scene_revisions: BTreeMap::new(),
        }
    }

    #[test]
    fn only_confirmed_script_spool_materializes_and_drop_cleans_up() {
        let temp = TempDir::new().expect("temp");
        let mut spool = ScriptSnapshotSpool::create(temp.path()).expect("spool");
        let path = spool.path.clone();
        let accepted = ScriptSnapshotAccepted {
            snapshot_id: "script-snapshot:0123456789abcdef0123456789abcdef".to_owned(),
            domain: "script_graph".to_owned(),
            resource_revision: 1,
            scene_graph_revision: 1,
            script_graph_revision: 1,
            revisions: revision(),
            limits_applied: ScriptSnapshotLimits {
                script_documents: 250_000,
                script_symbols: 2_000_000,
                script_relations: 4_000_000,
                script_diagnostics: 2_000_000,
                symbols_per_document: 65_536,
                relations_per_document: 262_144,
                diagnostics_per_document: 4_096,
                script_path_bytes: 1_024,
                script_name_bytes: 1_024,
                script_signature_bytes: 4_096,
                diagnostic_message_bytes: 2_048,
                snapshot_chunk_bytes: 256 * 1_024,
                snapshot_window_bytes: 32 * 1_024 * 1_024,
                snapshot_timeout_ms: 120_000,
                adapter_status_count: 16,
            },
        };
        let begin = ScriptSnapshotBeginParams {
            snapshot_id: accepted.snapshot_id.clone(),
            domain: "script_graph".to_owned(),
            resource_revision: 1,
            scene_graph_revision: 1,
            script_graph_revision: 1,
            revisions: revision(),
        };
        spool.begin(&accepted, &begin).expect("begin");
        spool
            .chunk(&ScriptSnapshotChunk {
                protocol_version: "1.4".to_owned(),
                kind: "chunk".to_owned(),
                snapshot_id: accepted.snapshot_id.clone(),
                domain: "script_graph".to_owned(),
                chunk_index: 0,
                payload: ScriptSnapshotPayload {
                    documents: Vec::new(),
                    symbols: Vec::new(),
                    relations: Vec::new(),
                    diagnostics: Vec::new(),
                    adapter_statuses: Vec::new(),
                },
                payload_json: "{\"adapter_statuses\":[],\"diagnostics\":[],\"documents\":[],\"relations\":[],\"symbols\":[]}".to_owned(),
                checksum: "unused_by_sink".to_owned(),
                context: RpcContext {
                    project_id: "project:test".to_owned(),
                    editor_session_id: revision().editor_session_id,
                },
            })
            .expect("chunk");
        let end = ScriptSnapshotEndParams {
            snapshot_id: accepted.snapshot_id.clone(),
            domain: "script_graph".to_owned(),
            resource_revision: 1,
            scene_graph_revision: 1,
            script_graph_revision: 1,
            chunk_count: 1,
            document_count: 0,
            symbol_count: 0,
            relation_count: 0,
            diagnostic_count: 0,
            adapter_status_count: 0,
            checksum: "fixture".to_owned(),
            revisions: revision(),
        };
        assert!(spool.confirmed_snapshot().is_err());
        spool.end(&end).expect("end");
        assert_eq!(spool.confirmed_snapshot().expect("snapshot").end, end);
        drop(spool);
        assert!(!path.exists());
    }
}
