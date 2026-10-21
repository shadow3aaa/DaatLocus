//! Bounded application queues; priority does not remove stream head-of-line blocking.
use super::{Result, StageError, protocol::Envelope};
use serde::Serialize;
use std::collections::VecDeque;
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Control,
    Event,
    Snapshot,
    Blob,
    Retransmission,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Traffic {
    pub control_bytes: u64,
    pub event_bytes: u64,
    pub snapshot_bytes: u64,
    pub blob_bytes: u64,
    pub retransmission_bytes: u64,
    pub frames: u64,
    pub queue_peak_bytes: usize,
    pub content_payload_bytes: u64,
    pub content_wire_bytes: u64,
}
impl Traffic {
    pub fn record_frame(&mut self, class: Class, frame: &Envelope, bytes: usize) {
        self.record(class, bytes);
        if let super::protocol::Body::BlobChunk { bytes: payload, .. } = &frame.body {
            self.content_payload_bytes = self
                .content_payload_bytes
                .saturating_add(payload.len() as u64);
            self.content_wire_bytes = self.content_wire_bytes.saturating_add(bytes as u64);
        }
    }
    /// Protocol bytes, including a TCP frame prefix where the adapter adds one.
    /// These counters deliberately do not claim to measure IP/TLS overhead.
    pub fn record(&mut self, class: Class, bytes: usize) {
        let target = match class {
            Class::Control => &mut self.control_bytes,
            Class::Event => &mut self.event_bytes,
            Class::Snapshot => &mut self.snapshot_bytes,
            Class::Blob => &mut self.blob_bytes,
            Class::Retransmission => &mut self.retransmission_bytes,
        };
        *target = target.saturating_add(bytes as u64);
        self.frames = self.frames.saturating_add(1);
    }
}
pub struct Queued {
    pub frame: Envelope,
    pub class: Class,
    pub bytes: usize,
}
pub struct FairQueue {
    control: VecDeque<Queued>,
    event: VecDeque<Queued>,
    bulk: VecDeque<Queued>,
    bytes: usize,
    budget: usize,
    step: u8,
    pub metrics: Traffic,
}
impl FairQueue {
    pub fn new(budget: usize) -> Self {
        Self {
            control: VecDeque::new(),
            event: VecDeque::new(),
            bulk: VecDeque::new(),
            bytes: 0,
            budget: budget.min(1024 * 1024),
            step: 0,
            metrics: Traffic::default(),
        }
    }
    pub fn push(&mut self, frame: Envelope, class: Class) -> Result<()> {
        let bytes = frame.encode()?.len() + 4;
        // Reserve a control window even when bulk subscribers are slow.
        let limit = if class == Class::Control {
            self.budget
        } else {
            self.budget.saturating_sub(128 * 1024)
        };
        if self.bytes.saturating_add(bytes) > limit {
            return Err(StageError::Capacity("Peer transport queue is full"));
        }
        self.bytes += bytes;
        self.metrics.queue_peak_bytes = self.metrics.queue_peak_bytes.max(self.bytes);
        let queue = match class {
            Class::Control => &mut self.control,
            Class::Event | Class::Snapshot => &mut self.event,
            _ => &mut self.bulk,
        };
        queue.push_back(Queued {
            frame,
            class,
            bytes,
        });
        Ok(())
    }
    pub fn pop(&mut self) -> Option<Queued> {
        // Eight control slots, four event slots, one content slot. Empty classes
        // lend slots; content cannot starve under sustained control traffic.
        for _ in 0..13 {
            let step = self.step;
            self.step = (self.step + 1) % 13;
            let item = match step {
                0..=7 => self.control.pop_front(),
                8..=11 => self.event.pop_front(),
                _ => self.bulk.pop_front(),
            };
            if let Some(item) = item {
                self.bytes -= item.bytes;
                return Some(item);
            }
        }
        None
    }
    pub fn queued_bytes(&self) -> usize {
        self.bytes
    }
}
