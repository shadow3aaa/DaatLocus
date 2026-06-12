use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

use super::frame_rate_limiter::FrameRateLimiter;

/// A lightweight handle that widgets and background tasks can clone
/// to request future redraws of the TUI.
#[derive(Clone, Debug)]
pub struct FrameRequester {
    frame_schedule_tx: mpsc::UnboundedSender<Instant>,
}

impl FrameRequester {
    /// Create a new FrameRequester and spawn its associated FrameScheduler task.
    pub fn new(draw_tx: broadcast::Sender<()>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let scheduler = FrameScheduler::new(rx, draw_tx);
        tokio::spawn(scheduler.run());
        Self {
            frame_schedule_tx: tx,
        }
    }

    /// Schedule a frame draw as soon as possible.
    pub fn schedule_frame(&self) {
        let _ = self.frame_schedule_tx.send(Instant::now());
    }

    /// Schedule a frame draw to occur after the specified duration.
    pub fn schedule_frame_in(&self, dur: Duration) {
        let _ = self.frame_schedule_tx.send(Instant::now() + dur);
    }
}

/// Internal actor that coalesces many frame requests into a single draw notification.
struct FrameScheduler {
    rx: mpsc::UnboundedReceiver<Instant>,
    draw_tx: broadcast::Sender<()>,
    limiter: FrameRateLimiter,
}

impl FrameScheduler {
    fn new(rx: mpsc::UnboundedReceiver<Instant>, draw_tx: broadcast::Sender<()>) -> Self {
        Self {
            rx,
            draw_tx,
            limiter: FrameRateLimiter::default(),
        }
    }

    async fn run(mut self) {
        const FAR_FUTURE: Duration = Duration::from_secs(60 * 60 * 24 * 365);
        let mut next_deadline: Option<Instant> = None;

        loop {
            let target = next_deadline.unwrap_or_else(|| Instant::now() + FAR_FUTURE);
            let sleep = tokio::time::sleep_until(target.into());
            tokio::pin!(sleep);

            tokio::select! {
                deadline = self.rx.recv() => {
                    let Some(deadline) = deadline else {
                        break;
                    };
                    let deadline = self.limiter.clamp_deadline(deadline);
                    next_deadline = Some(next_deadline.map_or(deadline, |current| current.min(deadline)));
                }
                _ = &mut sleep => {
                    if next_deadline.is_some() {
                        next_deadline = None;
                        self.limiter.mark_emitted(target);
                        let _ = self.draw_tx.send(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    #[tokio::test(flavor = "current_thread")]
    async fn immediate_frame_request_emits_one_draw() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();

        timeout(Duration::from_millis(200), draw_rx.recv())
            .await
            .expect("timed out waiting for draw")
            .expect("draw channel closed");
        assert!(
            timeout(Duration::from_millis(20), draw_rx.recv())
                .await
                .is_err(),
            "unexpected extra draw"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delayed_frame_request_waits_before_emitting() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame_in(Duration::from_millis(40));

        assert!(
            timeout(Duration::from_millis(10), draw_rx.recv())
                .await
                .is_err(),
            "draw fired too early"
        );
        timeout(Duration::from_millis(200), draw_rx.recv())
            .await
            .expect("timed out waiting for delayed draw")
            .expect("draw channel closed");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn multiple_pending_requests_coalesce_to_one_draw() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);

        requester.schedule_frame();
        requester.schedule_frame();
        requester.schedule_frame_in(Duration::from_millis(40));

        timeout(Duration::from_millis(200), draw_rx.recv())
            .await
            .expect("timed out waiting for coalesced draw")
            .expect("draw channel closed");
        assert!(
            timeout(Duration::from_millis(80), draw_rx.recv())
                .await
                .is_err(),
            "unexpected extra draw"
        );
    }
}
