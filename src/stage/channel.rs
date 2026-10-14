//! Persistent readers and bounded queues. Cancelling a control wait cannot
//! cancel a half-read TCP frame or lose its length prefix.
use super::{
    Result, StageError,
    protocol::Envelope,
    scheduler::{Class, FairQueue, Traffic},
    transport::{
        self,
        p2p::{P2pChannel, P2pSender},
        tls::VerifiedPeer,
    },
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, watch},
    task::JoinHandle,
};
#[async_trait]
pub trait FrameReader: Send {
    async fn read(&mut self) -> Result<(Envelope, usize)>;
}
#[async_trait]
pub trait FrameWriter: Send {
    async fn write(&mut self, frame: &Envelope, bulk: bool) -> Result<usize>;
    async fn close(&mut self);
}
// Three missed two-second heartbeats end the channel, including half-open
// TCP/RTC links. A timed-out partial frame is discarded with the whole channel.
async fn read_live_frame(
    reader: &mut dyn FrameReader,
    timeout: std::time::Duration,
) -> Result<(Envelope, usize)> {
    tokio::time::timeout(timeout, reader.read())
        .await
        .map_err(|_| StageError::Unavailable("Peer heartbeat timed out"))?
}
struct StreamReader<R>(R);
struct StreamWriter<W>(W);
#[async_trait]
impl<R: AsyncRead + Send + Unpin> FrameReader for StreamReader<R> {
    async fn read(&mut self) -> Result<(Envelope, usize)> {
        transport::read_frame(&mut self.0).await
    }
}
#[async_trait]
impl<W: AsyncWrite + Send + Unpin> FrameWriter for StreamWriter<W> {
    async fn write(&mut self, frame: &Envelope, _: bool) -> Result<usize> {
        transport::write_frame(&mut self.0, frame).await
    }
    async fn close(&mut self) {
        use tokio::io::AsyncWriteExt;
        let _ = self.0.shutdown().await;
    }
}
#[async_trait]
impl FrameReader for P2pChannel {
    async fn read(&mut self) -> Result<(Envelope, usize)> {
        self.receive().await
    }
}
#[async_trait]
impl FrameWriter for P2pSender {
    async fn write(&mut self, frame: &Envelope, bulk: bool) -> Result<usize> {
        self.send(frame, bulk).await
    }
    async fn close(&mut self) {
        let _ = P2pSender::close(self).await;
    }
}
#[derive(Clone)]
pub enum SendScope {
    Core,
    Connection,
    Actor(String),
    Invocation {
        invocation: uuid::Uuid,
        content: super::protocol::BlobRef,
    },
}
pub struct Incoming {
    pub frame: Envelope,
    pub bytes: usize,
    _permit: OwnedSemaphorePermit,
}
struct Outgoing {
    frame: Envelope,
    class: Class,
    scope: SendScope,
    _permit: OwnedSemaphorePermit,
}
#[derive(Clone)]
pub struct ChannelSender {
    queue: mpsc::Sender<Outgoing>,
    budget: Arc<Semaphore>,
    control: Arc<Semaphore>,
    write_ready: Arc<Notify>,
}
impl ChannelSender {
    pub(crate) fn write_notification(&self) -> Arc<Notify> {
        self.write_ready.clone()
    }
    pub async fn send_wait(&self, frame: Envelope, class: Class, scope: SendScope) -> Result<()> {
        let bytes = frame.encode()?.len() + 4;
        let permit = if class == Class::Control {
            self.control
                .clone()
                .try_acquire_many_owned(bytes as u32)
                .ok()
        } else {
            None
        };
        let permit = match permit {
            Some(permit) => permit,
            None => self
                .budget
                .clone()
                .acquire_many_owned(bytes as u32)
                .await
                .map_err(|_| StageError::Unavailable("Peer queue closed"))?,
        };
        self.queue
            .send(Outgoing {
                frame,
                class,
                scope,
                _permit: permit,
            })
            .await
            .map_err(|_| StageError::Unavailable("Peer channel is closed"))
    }
    pub fn send(&self, frame: Envelope, class: Class, scope: SendScope) -> Result<()> {
        let bytes = frame.encode()?.len() + 4;
        let permit = if class == Class::Control {
            self.control
                .clone()
                .try_acquire_many_owned(bytes as u32)
                .or_else(|_| self.budget.clone().try_acquire_many_owned(bytes as u32))
        } else {
            self.budget.clone().try_acquire_many_owned(bytes as u32)
        }
        .map_err(|_| StageError::Capacity("Peer queue byte budget reached"))?;
        self.queue
            .try_send(Outgoing {
                frame,
                class,
                scope,
                _permit: permit,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    StageError::Capacity("Peer frame queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    StageError::Unavailable("Peer channel is closed")
                }
            })
    }
}
pub struct PeerChannel {
    pub peer: VerifiedPeer,
    pub sender: ChannelSender,
    pub incoming: mpsc::Receiver<Result<Incoming>>,
    pub closed: watch::Receiver<bool>,
    pub metrics: Arc<parking_lot::Mutex<Traffic>>,
    tasks: Vec<JoinHandle<()>>,
}
impl PeerChannel {
    pub fn stream<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        peer: VerifiedPeer,
        stream: S,
        authorize: impl Fn(&Envelope, &SendScope) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self::new(
            peer,
            Box::new(StreamReader(reader)),
            Box::new(StreamWriter(writer)),
            authorize,
        )
    }
    pub fn p2p(
        peer: VerifiedPeer,
        channel: P2pChannel,
        authorize: impl Fn(&Envelope, &SendScope) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        let writer = channel.sender();
        Self::new(peer, Box::new(channel), Box::new(writer), authorize)
    }
    fn new(
        peer: VerifiedPeer,
        mut reader: Box<dyn FrameReader>,
        mut writer: Box<dyn FrameWriter>,
        authorize: impl Fn(&Envelope, &SendScope) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        let budget = Arc::new(Semaphore::new(896 * 1024));
        let control = Arc::new(Semaphore::new(128 * 1024));
        let (incoming_tx, incoming) = mpsc::channel(8);
        let (outgoing_tx, mut outgoing) = mpsc::channel::<Outgoing>(32);
        let (closed_tx, closed) = watch::channel(false);
        let write_ready = Arc::new(Notify::new());
        let sender = ChannelSender {
            queue: outgoing_tx,
            budget: budget.clone(),
            control: control.clone(),
            write_ready: write_ready.clone(),
        };
        let metrics = Arc::new(parking_lot::Mutex::new(Traffic::default()));
        let received = metrics.clone();
        let read_closed = closed_tx.clone();
        let incoming_budget = Arc::new(Semaphore::new(896 * 1024));
        let incoming_control = Arc::new(Semaphore::new(128 * 1024));
        let read_task = tokio::spawn(async move {
            loop {
                match read_live_frame(reader.as_mut(), std::time::Duration::from_secs(6)).await {
                    Ok((frame, bytes)) => {
                        let is_control = matches!(
                            frame.body,
                            super::protocol::Body::Received { .. }
                                | super::protocol::Body::TransferAck { .. }
                                | super::protocol::Body::Cancel { .. }
                                | super::protocol::Body::Result { .. }
                                | super::protocol::Body::ResultQuery { .. }
                                | super::protocol::Body::Error { .. }
                                | super::protocol::Body::Heartbeat
                                | super::protocol::Body::RecoveryStatus { .. }
                                | super::protocol::Body::CoordinationRequest { .. }
                                | super::protocol::Body::CoordinationResponse { .. }
                                | super::protocol::Body::CoreRequest { .. }
                                | super::protocol::Body::CoreResponse { .. }
                        );
                        let permits = if is_control {
                            incoming_control
                                .clone()
                                .acquire_many_owned(bytes as u32)
                                .await
                        } else {
                            incoming_budget
                                .clone()
                                .acquire_many_owned(bytes as u32)
                                .await
                        };
                        let Ok(permit) = permits else {
                            break;
                        };
                        received.lock().record(
                            if is_control {
                                Class::Control
                            } else if matches!(frame.body, super::protocol::Body::BlobChunk { .. })
                            {
                                Class::Blob
                            } else {
                                Class::Event
                            },
                            bytes,
                        );
                        if incoming_tx
                            .send(Ok(Incoming {
                                frame,
                                bytes,
                                _permit: permit,
                            }))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = incoming_tx.send(Err(error)).await;
                        break;
                    }
                }
            }
            read_closed.send_replace(true);
        });
        let sent = metrics.clone();
        let write_task = tokio::spawn(async move {
            let mut queue = FairQueue::new(1024 * 1024);
            let mut held = std::collections::HashMap::new();
            while let Some(first) = outgoing.recv().await {
                let mut batch = vec![first];
                while batch.len() < 32 {
                    match outgoing.try_recv() {
                        Ok(next) => batch.push(next),
                        Err(_) => break,
                    }
                }
                for item in batch {
                    let id = item.frame.message_id;
                    if held.contains_key(&id) {
                        continue;
                    }
                    let class = item.class;
                    let frame = item.frame.clone();
                    held.insert(id, item);
                    if queue.push(frame, class).is_err() {
                        closed_tx.send_replace(true);
                        writer.close().await;
                        return;
                    }
                }
                while let Some(item) = queue.pop() {
                    let Some(queued) = held.remove(&item.frame.message_id) else {
                        closed_tx.send_replace(true);
                        writer.close().await;
                        return;
                    };
                    if authorize(&queued.frame, &queued.scope).is_err() {
                        closed_tx.send_replace(true);
                        writer.close().await;
                        return;
                    }
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(4),
                        writer.write(&queued.frame, item.class == Class::Blob),
                    )
                    .await
                    {
                        Ok(Ok(bytes)) => {
                            let mut metrics = sent.lock();
                            metrics.record(item.class, bytes);
                            metrics.queue_peak_bytes =
                                metrics.queue_peak_bytes.max(queue.metrics.queue_peak_bytes);
                        }
                        _ => {
                            closed_tx.send_replace(true);
                            writer.close().await;
                            return;
                        }
                    }
                    drop(queued);
                    write_ready.notify_one();
                }
            }
            closed_tx.send_replace(true);
            writer.close().await;
        });
        Self {
            peer,
            sender,
            incoming,
            closed,
            metrics,
            tasks: vec![read_task, write_task],
        }
    }
    pub async fn close(&mut self) {
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for PeerChannel {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod liveness_tests {
    use super::*;
    #[tokio::test]
    async fn silent_peer_and_partial_frame_time_out() {
        for partial in [false, true] {
            let (local, mut remote) = tokio::io::duplex(128);
            if partial {
                use tokio::io::AsyncWriteExt;
                remote.write_all(&[0, 0]).await.unwrap();
            }
            let mut reader = StreamReader(local);
            let result = read_live_frame(&mut reader, std::time::Duration::from_millis(20)).await;
            assert!(matches!(
                result,
                Err(StageError::Unavailable("Peer heartbeat timed out"))
            ));
            drop(remote);
        }
    }
}
