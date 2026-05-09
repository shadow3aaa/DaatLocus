use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

use super::frame_rate_limiter::FrameRateLimiter;

/// A lightweight handle that widgets and background tasks can clone
/// to request future redraws of the TUI.
#[derive(Clone, Debug)]
pub struct FrameRequester {
    /// The TX half of the frame-scheduling channel. The RX half is owned by
    /// `FrameScheduler`. This field is never read directly but must be kept
    /// alive so the channel stays open while `FrameRequester` exists.
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub fn schedule_frame(&self) {
        let _ = self.frame_schedule_tx.send(Instant::now());
    }

    /// Schedule a frame draw to occur after the specified duration.
    #[allow(dead_code)]
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
        // Drain any immediate requests on startup so the first frame renders.
        let now = Instant::now();
        let _ = self.draw_tx.send(());
        self.limiter.mark_emitted(now);

        while let Some(deadline) = self.rx.recv().await {
            let clamped = self.limiter.clamp_deadline(deadline);
            let now = Instant::now();
            if clamped > now {
                let delay = clamped - now;
                // Wait for either the delay or a new request that may supersede.
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {
                        let _ = self.draw_tx.send(());
                        self.limiter.mark_emitted(Instant::now());
                    }
                    next = self.rx.recv() => {
                        let Some(new_deadline) = next else { break };

                        // Re-evaluate with the new deadline
                        let _new_clamped = self.limiter.clamp_deadline(new_deadline);
                        self.limiter.mark_emitted(Instant::now());
                        let _ = self.draw_tx.send(());
                        // Continue the outer loop with the new deadline
                        // For simplicity, just fire and continue
                    }
                }
            } else {
                let _ = self.draw_tx.send(());
                self.limiter.mark_emitted(now);
            }
        }
    }
}
