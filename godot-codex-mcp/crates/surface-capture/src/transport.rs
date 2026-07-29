use std::borrow::Cow;
use std::marker::PhantomData;
#[cfg(unix)]
use std::{fs::File, sync::Arc};

use rmcp::service::{RxJsonRpcMessage, ServiceRole, TxJsonRpcMessage};
use rmcp::transport::Transport;
use serde::Serialize;

use crate::{CaptureDirection, CaptureRecorder};

/// Transparent typed rmcp transport tap.
///
/// Messages are passed to the wrapped transport unchanged. The tap serializes
/// only a temporary in-memory view and records a bounded safe projection.
pub struct TappedTransport<R, T>
where
    R: ServiceRole,
    T: Transport<R>,
{
    inner: T,
    recorder: CaptureRecorder,
    #[cfg(unix)]
    _claim_guard: Option<Arc<File>>,
    _role: PhantomData<fn() -> R>,
}

impl<R, T> TappedTransport<R, T>
where
    R: ServiceRole,
    T: Transport<R>,
{
    /// Wrap a transport with a shared recorder.
    #[must_use]
    pub const fn new(inner: T, recorder: CaptureRecorder) -> Self {
        Self {
            inner,
            recorder,
            #[cfg(unix)]
            _claim_guard: None,
            _role: PhantomData,
        }
    }

    #[cfg(unix)]
    /// Retain the exact claimed-run advisory lock for this transport's lifetime.
    pub(crate) fn with_claim_guard(mut self, claim_guard: Arc<File>) -> Self {
        self._claim_guard = Some(claim_guard);
        self
    }

    /// Recorder shared with the owner of the claimed lease.
    #[must_use]
    pub const fn recorder(&self) -> &CaptureRecorder {
        &self.recorder
    }

    /// Recover the untouched wrapped transport.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<R, T> Transport<R> for TappedTransport<R, T>
where
    R: ServiceRole,
    T: Transport<R>,
    TxJsonRpcMessage<R>: Clone + Serialize,
    RxJsonRpcMessage<R>: Serialize,
{
    type Error = T::Error;

    fn name() -> Cow<'static, str> {
        "sprint11-surface-capture-tap".into()
    }

    fn send(
        &mut self,
        item: TxJsonRpcMessage<R>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let direction = if R::IS_CLIENT {
            CaptureDirection::ClientToServer
        } else {
            CaptureDirection::ServerToClient
        };
        let observed = item.clone();
        let recorder = self.recorder.clone();
        let send = self.inner.send(item);
        async move {
            let result = send.await;
            if result.is_ok() {
                recorder.observe_serializable(direction, &observed);
            }
            result
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<R>> {
        let item = self.inner.receive().await;
        if let Some(item) = &item {
            let direction = if R::IS_CLIENT {
                CaptureDirection::ServerToClient
            } else {
                CaptureDirection::ClientToServer
            };
            self.recorder.observe_serializable(direction, item);
        }
        item
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::{Arc, Mutex};

    use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
    use rmcp::{RoleServer, transport::Transport};

    use super::*;
    use crate::journal::JournalBinding;
    use crate::{CaptureEventClass, SemanticProjectionAllowlist};

    struct MockServerTransport {
        received: VecDeque<ClientJsonRpcMessage>,
        sent: Arc<Mutex<Vec<ServerJsonRpcMessage>>>,
        closed: Arc<Mutex<bool>>,
        fail_send: bool,
    }

    impl Transport<RoleServer> for MockServerTransport {
        type Error = io::Error;

        fn send(
            &mut self,
            item: ServerJsonRpcMessage,
        ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
            if self.fail_send {
                return std::future::ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "fixture send failed",
                )));
            }
            self.sent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(item);
            std::future::ready(Ok(()))
        }

        async fn receive(&mut self) -> Option<ClientJsonRpcMessage> {
            self.received.pop_front()
        }

        fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
            *self
                .closed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            async { Ok(()) }
        }
    }

    fn recorder(capacity: usize) -> CaptureRecorder {
        CaptureRecorder::with_capacity_and_allowlist(
            JournalBinding {
                run_id: "11".repeat(32),
                surface: "cli".to_owned(),
                project_identity: format!("sha256:{}", "22".repeat(32)),
                metadata_sha256: format!("sha256:{}", "33".repeat(32)),
                package_launcher_sha256: format!("sha256:{}", "44".repeat(32)),
                project_config_sha256: format!("sha256:{}", "55".repeat(32)),
                setup_receipt_sha256: format!("sha256:{}", "66".repeat(32)),
            },
            capacity,
            SemanticProjectionAllowlist::sprint11(),
        )
    }

    #[tokio::test]
    async fn tap_passes_typed_messages_through_unchanged() {
        let request: ClientJsonRpcMessage = serde_json::from_str(
            r#"{
                "jsonrpc":"2.0",
                "id":7,
                "method":"tools/call",
                "params":{
                    "name":"godot_get_connection_status",
                    "arguments":{"password":"do-not-record","path":"/private/project"}
                }
            }"#,
        )
        .unwrap();
        let response: ServerJsonRpcMessage = serde_json::from_str(
            r#"{
                "jsonrpc":"2.0",
                "id":7,
                "result":{
                    "content":[{"type":"text","text":"sk-do-not-record /private/project"}]
                }
            }"#,
        )
        .unwrap();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(Mutex::new(false));
        let inner = MockServerTransport {
            received: VecDeque::from([request.clone()]),
            sent: sent.clone(),
            closed: closed.clone(),
            fail_send: false,
        };
        let recorder = recorder(8);
        let mut tap = TappedTransport::<RoleServer, _>::new(inner, recorder.clone());

        let received = tap.receive().await.unwrap();
        assert_eq!(
            serde_json::to_value(received).unwrap(),
            serde_json::to_value(request).unwrap()
        );
        tap.send(response.clone()).await.unwrap();
        tap.close().await.unwrap();

        let sent = sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            serde_json::to_value(&sent[0]).unwrap(),
            serde_json::to_value(response).unwrap()
        );
        assert!(
            *closed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );
        let journal = recorder.snapshot();
        assert_eq!(journal.events.len(), 2);
        assert!(
            journal
                .events
                .iter()
                .all(|event| event.class == CaptureEventClass::Status)
        );
        let encoded = serde_json::to_string(&journal).unwrap();
        for forbidden in ["do-not-record", "/private/project", "arguments", "content"] {
            assert!(!encoded.contains(forbidden), "retained {forbidden}");
        }
    }

    #[tokio::test]
    async fn failed_outbound_send_is_not_recorded_as_delivered() {
        let response: ServerJsonRpcMessage = serde_json::from_str(
            r#"{
                "jsonrpc":"2.0",
                "id":7,
                "result":{"content":[{"type":"text","text":"not delivered"}]}
            }"#,
        )
        .unwrap();
        let recorder = recorder(8);
        let mut tap = TappedTransport::<RoleServer, _>::new(
            MockServerTransport {
                received: VecDeque::new(),
                sent: Arc::new(Mutex::new(Vec::new())),
                closed: Arc::new(Mutex::new(false)),
                fail_send: true,
            },
            recorder.clone(),
        );

        let error = tap.send(response).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert!(
            recorder.snapshot().events.is_empty(),
            "an outbound message that the inner transport rejected must not become evidence"
        );
    }
}
