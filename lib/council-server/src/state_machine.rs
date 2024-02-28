use std::{result, sync::Arc};

use council_core::subject;
use si_data_nats::async_nats::jetstream;
use thiserror::Error;

#[remain::sorted]
#[derive(Debug, Error)]
pub enum StateMachineError {
    #[error("publish message error: {0}")]
    Publish(#[from] jetstream::context::PublishError),
}

pub type Result<T> = result::Result<T, StateMachineError>;

#[derive(Clone, Debug)]
pub struct StateMachine {
    prefix: Arc<Option<String>>,
    inner: jetstream::Context,
}

impl StateMachine {
    pub fn new(prefix: Arc<Option<String>>, inner: jetstream::Context) -> Self {
        Self { prefix, inner }
    }

    pub async fn send_find_ready_to_process(&self) -> Result<()> {
        self.inner
            .publish(
                subject::find_ready_to_process(self.prefix.as_deref()),
                Default::default(),
            )
            .await?
            .await?;

        Ok(())
    }
}
