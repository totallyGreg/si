use std::error;

use si_data_nats::async_nats::jetstream;
use si_data_pg::{PgError, PgPoolError};
use si_std::CanonicalFileError;
use thiserror::Error;

use crate::persister::{PersistMessage, PersisterTaskError};

#[remain::sorted]
#[derive(Error, Debug)]
pub enum LayerDbError {
    #[error("canonical file error: {0}")]
    CanonicalFile(#[from] CanonicalFileError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("missing internal buffer entry when expected; this is an internal bug")]
    MissingInternalBuffer,
    #[error("error parsing nats message header: {0}")]
    NatsHeaderParse(#[source] Box<dyn error::Error + Send + Sync + 'static>),
    #[error("malformed/missing nats headers")]
    NatsMalformedHeaders,
    #[error("nats message missing size header")]
    NatsMissingSizeHeader,
    #[error("error publishing message: {0}")]
    NatsPublish(#[from] jetstream::context::PublishError),
    #[error("error pull message from stream: {0}")]
    NatsPullMessages(#[from] jetstream::consumer::pull::MessagesError),
    #[error("persister task write failed: {0:?}")]
    PersisterTaskFailed(PersisterTaskError),
    #[error("persister write error: {0}")]
    PersisterWriteSend(#[from] tokio::sync::mpsc::error::SendError<PersistMessage>),
    #[error("pg error: {0}]")]
    Pg(#[from] PgError),
    #[error("pg pool error: {0}]")]
    PgPool(#[from] PgPoolError),
    #[error("postcard error: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("sled error: {0}")]
    SledError(#[from] sled::Error),
    #[error("tokio oneshot recv error: {0}")]
    TokioOneShotRecv(#[from] tokio::sync::oneshot::error::RecvError),
}

impl LayerDbError {
    pub fn nats_header_parse<E>(err: E) -> Self
    where
        E: error::Error + Send + Sync + 'static,
    {
        Self::NatsHeaderParse(Box::new(err))
    }
}

pub type LayerDbResult<T> = Result<T, LayerDbError>;
