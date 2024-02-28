use std::{
    fmt,
    future::{Future, IntoFuture},
    io, result,
    sync::Arc,
    time::Duration,
};

use council_core::{council_incoming_stream, council_kv, subject};
use naxum::{
    handler::Handler,
    middleware::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer},
    response::Response,
    ServiceExt as _,
};
use si_data_nats::{async_nats::jetstream, NatsClient, NatsConfig, NatsError};
use telemetry::prelude::*;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;

use crate::{
    app_state::{AppState, AttributeValueMaxAttempts, Prefix},
    Config, StateMachine, StateStore, StatusStore,
};

use self::handlers::HandlerError;

mod extract;
mod handlers;

const INCOMING_CONSUMER_NAME: &str = "council-incoming";

#[derive(Debug, Error)]
pub enum Error {
    #[error("stream consumer error: {0}")]
    JsConsumer(#[from] jetstream::stream::ConsumerError),
    #[error("consumer stream error: {0}")]
    JsConsumerStream(#[from] jetstream::consumer::StreamError),
    #[error("stream create error: {0}")]
    JsCreateStreamError(#[from] jetstream::context::CreateStreamError),
    #[error("kv create bucket error: {0}")]
    KvCreateBucket(#[from] jetstream::context::CreateKeyValueError),
    #[error("nats error: {0}")]
    Nats(#[from] NatsError),
    #[error("naxum error: {0}")]
    Naxum(#[source] io::Error),
}

type Result<T> = result::Result<T, Error>;

#[derive(Clone, Debug)]
pub struct ServerMetadata {
    job_instance: String,
    job_invoked_provider: &'static str,
}

pub struct Server {
    metadata: Arc<ServerMetadata>,
    inner: Box<dyn Future<Output = io::Result<()>> + Unpin + Send>,
    shutdown_token: CancellationToken,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("metadata", &self.metadata)
            .field("shutdown_token", &self.shutdown_token)
            .finish_non_exhaustive()
    }
}

impl Server {
    #[instrument(name = "council.init.from_config", level = "info", skip_all)]
    pub async fn from_config(config: Config, shutdown_token: CancellationToken) -> Result<Self> {
        let nats = Self::connect_to_nats(config.nats()).await?;

        Self::from_client_with_config(nats, config, shutdown_token).await
    }

    #[instrument(
        name = "council.init.from_client_with_config",
        level = "info",
        skip_all
    )]
    pub async fn from_client_with_config(
        nats: NatsClient,
        config: Config,
        shutdown_token: CancellationToken,
    ) -> Result<Self> {
        // Take the *active* subject prefix from the connected NATS client
        let prefix = nats.metadata().subject_prefix().map(|s| s.to_owned());

        let context = jetstream::new(nats.as_inner().clone());

        let incoming = council_incoming_stream(&context, prefix.as_deref())
            .await?
            .create_consumer(Self::incoming_consumer_config(prefix.as_deref()))
            .await?
            .messages()
            .await?;

        let kv = council_kv(&context, prefix.as_deref()).await?;

        Ok(Self::from_services(
            config.instance_id().to_string(),
            prefix,
            context,
            kv,
            incoming,
            config.attribute_value_max_attempts(),
            shutdown_token,
        ))
    }

    #[instrument(name = "council.init.from_services", level = "info", skip_all)]
    #[allow(clippy::too_many_arguments)]
    pub fn from_services(
        instance_id: impl Into<String>,
        mut prefix: Option<String>,
        context: jetstream::Context,
        kv: jetstream::kv::Store,
        incoming: jetstream::consumer::pull::Stream,
        attribute_value_max_attempts: u16,
        shutdown_token: CancellationToken,
    ) -> Self {
        let metadata = ServerMetadata {
            job_instance: instance_id.into(),
            job_invoked_provider: "si",
        };
        if let Some(prefix) = &mut prefix {
            if !prefix.ends_with('.') {
                prefix.push('.');
            }
        }
        let prefix = Arc::new(prefix);
        let state = AppState::new(
            Prefix(prefix.clone()),
            StateMachine::new(prefix, context),
            StateStore::new(kv.clone()),
            StatusStore::new(kv),
            AttributeValueMaxAttempts(attribute_value_max_attempts),
        );

        let app = ServiceBuilder::new()
            .concurrency_limit(1)
            .layer(
                TraceLayer::new()
                    .make_span_with(DefaultMakeSpan::new().level(Level::TRACE))
                    .on_request(DefaultOnRequest::new().level(Level::TRACE))
                    .on_response(DefaultOnResponse::new().level(Level::TRACE)),
            )
            .map_err(HandlerError::Timeout)
            .timeout(Duration::from_secs(10))
            .service(handlers::router.with_state(state))
            .handle_error::<_, Response>(handlers::error);

        let inner = naxum::serve(incoming, app.into_make_service())
            .with_graceful_shutdown(naxum::wait_on_cancelled(shutdown_token.clone()));

        Self {
            metadata: Arc::new(metadata),
            inner: Box::new(inner.into_future()),
            shutdown_token,
        }
    }

    #[inline]
    pub async fn run(self) {
        if let Err(err) = self.try_run().await {
            error!(error = ?err, "error while running council main loop");
        }
    }

    pub async fn try_run(self) -> Result<()> {
        self.inner.await.map_err(Error::Naxum)?;
        info!("council main loop shutdown complete");
        Ok(())
    }

    /// Gets a [`ShutdownHandle`] that can externally or on demand trigger the server's shutdown
    /// process.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            token: self.shutdown_token.clone(),
        }
    }

    #[instrument(name = "council.init.connect_to_nats", level = "info", skip_all)]
    async fn connect_to_nats(nats_config: &NatsConfig) -> Result<NatsClient> {
        let client = NatsClient::new(nats_config).await?;
        debug!("successfully connected nats client");
        Ok(client)
    }

    #[inline]
    fn incoming_consumer_config(subject_prefix: Option<&str>) -> jetstream::consumer::pull::Config {
        jetstream::consumer::pull::Config {
            durable_name: Some(INCOMING_CONSUMER_NAME.to_owned()),
            filter_subject: subject::incoming(subject_prefix).to_string(),
            max_ack_pending: 1,
            ..Default::default()
        }
    }
}

pub struct ShutdownHandle {
    token: CancellationToken,
}

impl ShutdownHandle {
    pub fn shutdown(self) {
        self.token.cancel()
    }
}
