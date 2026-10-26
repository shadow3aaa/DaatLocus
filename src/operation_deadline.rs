//! Optional operation deadlines, independent of transport and cleanup timeouts.
use std::{future::Future, time::Duration};

#[derive(Clone, Copy)]
pub(crate) struct Deadline(Option<tokio::time::Instant>);

impl Deadline {
    pub(crate) fn after_ms(milliseconds: Option<u64>) -> miette::Result<Self> {
        let end = milliseconds
            .map(|milliseconds| {
                tokio::time::Instant::now()
                    .checked_add(Duration::from_millis(milliseconds))
                    .ok_or_else(|| miette::miette!("Operation timeout is too large"))
            })
            .transpose()?;
        Ok(Self(end))
    }
    pub(crate) async fn wait<F: Future>(
        self,
        future: F,
    ) -> Result<F::Output, tokio::time::error::Elapsed> {
        match self.0 {
            Some(end) => tokio::time::timeout_at(end, future).await,
            None => Ok(future.await),
        }
    }
}
