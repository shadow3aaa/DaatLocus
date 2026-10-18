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
struct StreamReader<R>(R, transport::wire::Format);
struct StreamWriter<W>(W, transport::wire::Format);
#[async_trait]
impl<R: AsyncRead + Send + Unpin> FrameReader for StreamReader<R> {
    async fn read(&mut self) -> Result<(Envelope, usize)> {
        transport::read_frame_with(&mut self.0, self.1).await
    }
}
#[async_trait]
impl<W: AsyncWrite + Send + Unpin> FrameWriter for StreamWriter<W> {
    async fn write(&mut self, frame: &Envelope, _: bool) -> Result<usize> {
        transport::write_frame_with(&mut self.0, frame, self.1).await
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
    bulk: Option<mpsc::Sender<Outgoing>>,
    budget: Arc<Semaphore>,
    control: Arc<Semaphore>,
    write_ready: Arc<Notify>,
}
impl ChannelSender {
    fn queue_for(&self, class: Class) -> &mpsc::Sender<Outgoing> {
        if class == Class::Blob {
            self.bulk.as_ref().unwrap_or(&self.queue)
        } else {
            &self.queue
        }
    }
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
        self.queue_for(class)
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
        self.queue_for(class)
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
        Self::stream_format(peer, stream, transport::wire::Format::Json, authorize)
    }
    pub(crate) fn stream_format<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
        peer: VerifiedPeer,
        stream: S,
        format: transport::wire::Format,
        authorize: impl Fn(&Envelope, &SendScope) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self::new(
            peer,
            Box::new(StreamReader(reader, format)),
            Box::new(StreamWriter(writer, format)),
            None,
            authorize,
        )
    }
    pub fn p2p(
        peer: VerifiedPeer,
        channel: P2pChannel,
        authorize: impl Fn(&Envelope, &SendScope) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        let writer = channel.sender();
        Self::new(
            peer,
            Box::new(channel),
            Box::new(writer.clone()),
            Some(Box::new(writer)),
            authorize,
        )
    }
    fn new(
        peer: VerifiedPeer,
        mut reader: Box<dyn FrameReader>,
        writer: Box<dyn FrameWriter>,
        bulk_writer: Option<Box<dyn FrameWriter>>,
        authorize: impl Fn(&Envelope, &SendScope) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        let budget = Arc::new(Semaphore::new(896 * 1024));
        let control = Arc::new(Semaphore::new(128 * 1024));
        let (incoming_tx, incoming) = mpsc::channel(8);
        let (outgoing_tx, outgoing) = mpsc::channel::<Outgoing>(32);
        let (closed_tx, closed) = watch::channel(false);
        let write_ready = Arc::new(Notify::new());
        let (bulk_tx, bulk_rx) = mpsc::channel(8);
        let sender = ChannelSender {
            queue: outgoing_tx,
            bulk: bulk_writer.as_ref().map(|_| bulk_tx),
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
                        received.lock().record_frame(
                            if is_control {
                                Class::Control
                            } else if matches!(frame.body, super::protocol::Body::BlobChunk { .. })
                            {
                                Class::Blob
                            } else {
                                Class::Event
                            },
                            &frame,
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
        let authorize = Arc::new(authorize);
        let write_task = spawn_writer(
            writer,
            outgoing,
            metrics.clone(),
            closed_tx.clone(),
            write_ready.clone(),
            authorize.clone(),
            std::time::Duration::from_secs(4),
        );
        let bulk_task = bulk_writer.map(|writer| {
            spawn_writer(
                writer,
                bulk_rx,
                metrics.clone(),
                closed_tx,
                write_ready,
                authorize,
                std::time::Duration::from_secs(20),
            )
        });
        Self {
            peer,
            sender,
            incoming,
            closed,
            metrics,
            tasks: std::iter::once(read_task)
                .chain(std::iter::once(write_task))
                .chain(bulk_task)
                .collect(),
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

type Authorizer = Arc<dyn Fn(&Envelope, &SendScope) -> Result<()> + Send + Sync>;

fn spawn_writer(
    mut writer: Box<dyn FrameWriter>,
    mut outgoing: mpsc::Receiver<Outgoing>,
    metrics: Arc<parking_lot::Mutex<Traffic>>,
    closed_tx: watch::Sender<bool>,
    write_ready: Arc<Notify>,
    authorize: Authorizer,
    write_timeout: std::time::Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
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
                    write_timeout,
                    writer.write(&queued.frame, item.class == Class::Blob),
                )
                .await
                {
                    Ok(Ok(bytes)) => {
                        let mut metrics = metrics.lock();
                        metrics.record_frame(item.class, &queued.frame, bytes);
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
    })
}

#[cfg(test)]
mod liveness_tests {
    use super::*;
    struct PendingReader;
    #[async_trait]
    impl FrameReader for PendingReader {
        async fn read(&mut self) -> Result<(Envelope, usize)> {
            std::future::pending().await
        }
    }
    struct ObservedWriter {
        output: mpsc::Sender<(uuid::Uuid, bool)>,
        stall: Option<Arc<Notify>>,
    }
    #[async_trait]
    impl FrameWriter for ObservedWriter {
        async fn write(&mut self, frame: &Envelope, bulk: bool) -> Result<usize> {
            self.output.send((frame.message_id, bulk)).await.unwrap();
            if let Some(stall) = &self.stall {
                stall.notified().await;
            }
            Ok(frame.encode()?.len())
        }
        async fn close(&mut self) {}
    }
    #[tokio::test]
    async fn blocked_content_writer_does_not_block_control_emission() {
        let identity = crate::stage::identity::DeviceIdentity::generate().unwrap();
        let (tx, mut output) = mpsc::channel(8);
        let release = Arc::new(Notify::new());
        let mut channel = PeerChannel::new(
            VerifiedPeer::signed_dtls(&identity.profile()),
            Box::new(PendingReader),
            Box::new(ObservedWriter {
                output: tx.clone(),
                stall: None,
            }),
            Some(Box::new(ObservedWriter {
                output: tx,
                stall: Some(release.clone()),
            })),
            |_, _| Ok(()),
        );
        let frame = Envelope {
            version: 1,
            source: crate::stage::StageId::new(),
            target: identity.profile().stage_id,
            generation: 1,
            message_id: uuid::Uuid::new_v4(),
            invocation_id: None,
            in_reply_to: None,
            created_at_ms: 1,
            expires_at_ms: 10000,
            body: crate::stage::protocol::Body::Heartbeat,
        };
        channel
            .sender
            .send(frame.clone(), Class::Blob, SendScope::Connection)
            .unwrap();
        assert_eq!(output.recv().await.unwrap(), (frame.message_id, true));
        let control = Envelope {
            message_id: uuid::Uuid::new_v4(),
            ..frame
        };
        channel
            .sender
            .send(control.clone(), Class::Control, SendScope::Connection)
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), output.recv())
                .await
                .expect("control waited for content writer")
                .unwrap(),
            (control.message_id, false)
        );
        release.notify_one();
        channel.close().await;
    }
    #[tokio::test]
    async fn silent_peer_and_partial_frame_time_out() {
        for partial in [false, true] {
            let (local, mut remote) = tokio::io::duplex(128);
            if partial {
                use tokio::io::AsyncWriteExt;
                remote.write_all(&[0, 0]).await.unwrap();
            }
            let mut reader = StreamReader(local, transport::wire::Format::Json);
            let result = read_live_frame(&mut reader, std::time::Duration::from_millis(20)).await;
            assert!(matches!(
                result,
                Err(StageError::Unavailable("Peer heartbeat timed out"))
            ));
            drop(remote);
        }
    }
}
