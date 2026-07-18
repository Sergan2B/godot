use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use godot_codex_bridge_client::{
    BridgeError, ResourceSnapshot, ResourceSnapshotAccepted, ResourceSnapshotBeginParams,
    ResourceSnapshotChunk, ResourceSnapshotEndParams, ResourceSnapshotPayload,
    ResourceSnapshotSink,
};

use crate::IndexerError;

const MAX_SPOOL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
static NEXT_SPOOL_ID: AtomicU64 = AtomicU64::new(1);

/// Disk-backed receiver for one fully validated Bridge resource snapshot.
///
/// The file remains inside the store staging namespace, is bounded, and is
/// removed on every drop path. Only a confirmed `snapshot.end` can be read.
pub struct ResourceSnapshotSpool {
    path: PathBuf,
    file: File,
    bytes_written: u64,
    accepted: Option<ResourceSnapshotAccepted>,
    begin: Option<ResourceSnapshotBeginParams>,
    end: Option<ResourceSnapshotEndParams>,
}

impl ResourceSnapshotSpool {
    /// Creates a unique staging file without following or replacing a link.
    pub fn create(staging_directory: &Path) -> Result<Self, IndexerError> {
        fs::create_dir_all(staging_directory)
            .map_err(|_| IndexerError::Spool("staging_directory_unavailable"))?;
        for _ in 0..32 {
            let id = NEXT_SPOOL_ID.fetch_add(1, Ordering::Relaxed);
            let path = staging_directory.join(format!(
                "resource-snapshot-{}-{id:016x}.spool",
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
                Err(_) => return Err(IndexerError::Spool("staging_file_unavailable")),
            }
        }
        Err(IndexerError::Spool("staging_name_exhausted"))
    }

    /// Materializes the confirmed payload after the Bridge has validated all
    /// chunk and end checksums. The caller can then perform deterministic
    /// resource/dependency normalization without retaining network frames.
    pub fn confirmed_snapshot(&mut self) -> Result<ResourceSnapshot, IndexerError> {
        let accepted = self
            .accepted
            .clone()
            .ok_or(IndexerError::Spool("snapshot_acceptance_missing"))?;
        let begin = self
            .begin
            .clone()
            .ok_or(IndexerError::Spool("snapshot_begin_missing"))?;
        let end = self
            .end
            .clone()
            .ok_or(IndexerError::Spool("snapshot_end_missing"))?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| IndexerError::Spool("staging_seek_failed"))?;
        let mut resources = Vec::with_capacity(end.resource_count);
        let mut dependencies = Vec::with_capacity(end.dependency_count);
        let mut diagnostics = Vec::with_capacity(end.diagnostic_count);
        let mut chunks = 0_usize;
        loop {
            let mut length = [0_u8; 4];
            match self.file.read_exact(&mut length) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => break,
                Err(_) => return Err(IndexerError::Spool("staging_read_failed")),
            }
            let length = usize::try_from(u32::from_be_bytes(length))
                .map_err(|_| IndexerError::Spool("staging_chunk_too_large"))?;
            let mut payload = vec![0_u8; length];
            self.file
                .read_exact(&mut payload)
                .map_err(|_| IndexerError::Spool("staging_chunk_truncated"))?;
            let payload: ResourceSnapshotPayload = serde_json::from_slice(&payload)
                .map_err(|_| IndexerError::Spool("staging_payload_invalid"))?;
            resources.extend(payload.resources);
            dependencies.extend(payload.dependencies);
            diagnostics.extend(payload.diagnostics);
            chunks = chunks
                .checked_add(1)
                .ok_or(IndexerError::Spool("staging_chunk_overflow"))?;
        }
        if chunks != end.chunk_count
            || resources.len() != end.resource_count
            || dependencies.len() != end.dependency_count
            || diagnostics.len() != end.diagnostic_count
        {
            return Err(IndexerError::Spool("staging_count_mismatch"));
        }
        Ok(ResourceSnapshot {
            accepted,
            begin,
            payload: ResourceSnapshotPayload {
                resources,
                dependencies,
                diagnostics,
            },
            end,
        })
    }

    fn sink_error(code: &'static str) -> BridgeError {
        BridgeError::Invalid(format!("resource snapshot spool failed: {code}"))
    }
}

impl ResourceSnapshotSink for ResourceSnapshotSpool {
    fn begin(
        &mut self,
        accepted: &ResourceSnapshotAccepted,
        begin: &ResourceSnapshotBeginParams,
    ) -> Result<(), BridgeError> {
        if self.accepted.is_some() || self.begin.is_some() || self.end.is_some() {
            return Err(Self::sink_error("duplicate_begin"));
        }
        self.accepted = Some(accepted.clone());
        self.begin = Some(begin.clone());
        Ok(())
    }

    fn chunk(&mut self, chunk: &ResourceSnapshotChunk) -> Result<(), BridgeError> {
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

    fn end(&mut self, end: &ResourceSnapshotEndParams) -> Result<(), BridgeError> {
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

impl Drop for ResourceSnapshotSpool {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use godot_codex_bridge_client::{ResourceRevisionVector, ResourceSnapshotLimits, RpcContext};
    use tempfile::TempDir;

    use super::*;

    fn revision() -> ResourceRevisionVector {
        ResourceRevisionVector {
            editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
            event_seq: 1,
            project_revision: 1,
            operation_seq: 1,
            resource_revision: 1,
            scene_graph_revision: None,
            script_graph_revision: None,
            scene_revisions: BTreeMap::new(),
        }
    }

    #[test]
    fn only_confirmed_spool_is_materialized_and_drop_removes_it() {
        let temp = TempDir::new().unwrap();
        let mut spool = ResourceSnapshotSpool::create(temp.path()).unwrap();
        let path = spool.path.clone();
        let accepted = ResourceSnapshotAccepted {
            snapshot_id: "snapshot:1123456789abcdef0123456789abcdef".to_owned(),
            domain: "resource_graph".to_owned(),
            resource_revision: 1,
            revisions: revision(),
            limits_applied: ResourceSnapshotLimits {
                resource_records: 250_000,
                resource_dependencies: 2_000_000,
                resource_dependencies_per_record: 4_096,
                resource_path_bytes: 1_024,
                snapshot_chunk_bytes: 256 * 1_024,
                snapshot_window_bytes: 32 * 1_024 * 1_024,
                snapshot_timeout_ms: 120_000,
            },
        };
        let begin = ResourceSnapshotBeginParams {
            snapshot_id: accepted.snapshot_id.clone(),
            domain: "resource_graph".to_owned(),
            resource_revision: 1,
            revisions: revision(),
        };
        spool.begin(&accepted, &begin).unwrap();
        let payload = ResourceSnapshotPayload {
            resources: Vec::new(),
            dependencies: Vec::new(),
            diagnostics: Vec::new(),
        };
        spool
            .chunk(&ResourceSnapshotChunk {
                protocol_version: "1.2".to_owned(),
                kind: "chunk".to_owned(),
                snapshot_id: accepted.snapshot_id.clone(),
                domain: "resource_graph".to_owned(),
                chunk_index: 0,
                payload,
                payload_json: "{\"dependencies\":[],\"diagnostics\":[],\"resources\":[]}"
                    .to_owned(),
                checksum: "unused_by_sink".to_owned(),
                context: RpcContext {
                    project_id: "project:test".to_owned(),
                    editor_session_id: revision().editor_session_id,
                },
            })
            .unwrap();
        let end = ResourceSnapshotEndParams {
            snapshot_id: accepted.snapshot_id.clone(),
            domain: "resource_graph".to_owned(),
            resource_revision: 1,
            chunk_count: 1,
            resource_count: 0,
            dependency_count: 0,
            diagnostic_count: 0,
            checksum: "fixture".to_owned(),
            revisions: revision(),
        };
        assert!(spool.confirmed_snapshot().is_err());
        spool.end(&end).unwrap();
        assert_eq!(spool.confirmed_snapshot().unwrap().end, end);
        drop(spool);
        assert!(!path.exists());
    }
}
