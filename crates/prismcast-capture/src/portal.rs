use crate::{CaptureError, CaptureKind, Portal, PortalFuture, Result};
use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
    PersistMode, Session,
};
use futures_util::StreamExt;
use std::{os::fd::OwnedFd, sync::Arc};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};

pub(crate) struct PortalConnection {
    connection: ashpd::zbus::Connection,
    proxy: Screencast,
    session: Option<Arc<Session<Screencast>>>,
    closed: watch::Sender<bool>,
    watcher: Option<JoinHandle<()>>,
}
impl PortalConnection {
    pub(crate) async fn connect() -> Result<Self> {
        let connection = ashpd::zbus::Connection::session()
            .await
            .map_err(bus_error)?;
        let proxy = Screencast::with_connection(connection.clone())
            .await
            .map_err(portal_error)?;
        let (closed, _) = watch::channel(false);
        Ok(Self {
            connection,
            proxy,
            session: None,
            closed,
            watcher: None,
        })
    }
    fn session(&self) -> Result<&Session<Screencast>> {
        self.session.as_deref().ok_or(CaptureError::Closed)
    }
}
fn bus_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Portal(error.to_string())
}
fn portal_error(error: ashpd::Error) -> CaptureError {
    match error {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => CaptureError::Cancelled,
        other => CaptureError::Portal(other.to_string()),
    }
}
fn source_type(kind: CaptureKind) -> SourceType {
    match kind {
        CaptureKind::Monitor => SourceType::Monitor,
        CaptureKind::Window => SourceType::Window,
    }
}
impl Portal for PortalConnection {
    fn create(&mut self, kind: CaptureKind) -> PortalFuture<'_, ()> {
        Box::pin(async move {
            if self.proxy.version() < 2 {
                return Err(CaptureError::Unsupported(
                    "ScreenCast portal version <2".into(),
                ));
            }
            if !self
                .proxy
                .available_source_types()
                .await
                .map_err(portal_error)?
                .contains(source_type(kind))
            {
                return Err(CaptureError::Unsupported(format!("{kind:?} selection")));
            }
            if !self
                .proxy
                .available_cursor_modes()
                .await
                .map_err(portal_error)?
                .contains(CursorMode::Embedded)
            {
                return Err(CaptureError::Unsupported("embedded cursor".into()));
            }
            let session = Arc::new(
                self.proxy
                    .create_session(Default::default())
                    .await
                    .map_err(portal_error)?,
            );
            self.session = Some(session.clone());
            let closed = self.closed.clone();
            let (ready_tx, ready) = oneshot::channel();
            self.watcher = Some(tokio::spawn(async move {
                match session.receive_closed().await {
                    Ok(stream) => {
                        futures_util::pin_mut!(stream);
                        let _ = ready_tx.send(Ok(()));
                        let _ = stream.next().await;
                        closed.send_replace(true);
                    }
                    Err(error) => {
                        closed.send_replace(true);
                        let _ = ready_tx.send(Err(portal_error(error)));
                    }
                }
            }));
            ready.await.map_err(|_| CaptureError::Closed)??;
            Ok(())
        })
    }
    fn closed(&self) -> watch::Receiver<bool> {
        self.closed.subscribe()
    }
    fn select(&self, kind: CaptureKind) -> PortalFuture<'_, ()> {
        Box::pin(async move {
            self.proxy
                .select_sources(
                    self.session()?,
                    SelectSourcesOptions::default()
                        .set_sources(Some(source_type(kind).into()))
                        .set_multiple(false)
                        .set_cursor_mode(CursorMode::Embedded)
                        .set_persist_mode(PersistMode::DoNot),
                )
                .await
                .map_err(portal_error)?
                .response()
                .map_err(portal_error)
        })
    }
    fn start(&self) -> PortalFuture<'_, u32> {
        Box::pin(async move {
            let response = self
                .proxy
                .start(self.session()?, None, Default::default())
                .await
                .map_err(portal_error)?
                .response()
                .map_err(portal_error)?;
            if response.streams().len() != 1 {
                return Err(CaptureError::Unsupported(
                    "portal must return exactly one stream".into(),
                ));
            }
            let stream = response.streams().first().ok_or(CaptureError::Closed)?;
            Ok(stream.pipe_wire_node_id())
        })
    }
    fn remote(&self) -> PortalFuture<'_, OwnedFd> {
        Box::pin(async move {
            self.proxy
                .open_pipe_wire_remote(self.session()?, Default::default())
                .await
                .map_err(portal_error)
        })
    }
    fn cleanup(&mut self) -> PortalFuture<'_, ()> {
        Box::pin(async move {
            if let Some(watcher) = self.watcher.take() {
                watcher.abort();
                let _ = watcher.await;
            }
            // Closing the connection is mandatory even if the explicit session close fails.
            let session = self.session.take();
            // Dedicated bus disconnect is the final authority even if Session.Close
            // stalls; perform both concurrently so connection close is never skipped.
            let (_, connection_result) = tokio::join!(
                async {
                    if let Some(session) = session {
                        let _ = session.close().await;
                    }
                },
                self.connection.clone().close()
            );
            connection_result.map_err(bus_error)
        })
    }
}
impl Drop for PortalConnection {
    fn drop(&mut self) {
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_portal_cancellation_remains_typed_cancelled() {
        assert!(matches!(
            portal_error(ashpd::Error::Response(
                ashpd::desktop::ResponseError::Cancelled
            )),
            CaptureError::Cancelled
        ));
    }
}
