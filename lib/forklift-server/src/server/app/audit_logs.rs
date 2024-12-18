use std::{
    future::{Future, IntoFuture as _},
    io,
    sync::Arc,
};

use app_state::AppState;
use audit_logs::{
    database::{AuditDatabaseConfig, AuditDatabaseContext, AuditDatabaseContextError},
    AuditLogsStream, AuditLogsStreamError,
};
use naxum::{
    extract::MatchedSubject,
    handler::Handler as _,
    middleware::{
        ack::AckLayer,
        matched_subject::{ForSubject, MatchedSubjectLayer},
        trace::TraceLayer,
    },
    response::{IntoResponse, Response},
    MessageHead, ServiceBuilder, ServiceExt as _, TowerServiceExt as _,
};
use si_data_nats::{
    async_nats::{
        self,
        error::Error as AsyncNatsError,
        jetstream::{consumer::StreamErrorKind, stream::ConsumerErrorKind},
    },
    jetstream::Context,
    ConnectionMetadata,
};
use telemetry::prelude::*;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

mod app_state;
mod handlers;

#[derive(Debug, Error)]
pub enum AuditLogsAppSetupError {
    #[error("async nats consumer error: {0}")]
    AsyncNatsConsumer(#[from] AsyncNatsError<ConsumerErrorKind>),
    #[error("async nats stream error: {0}")]
    AsyncNatsStream(#[from] AsyncNatsError<StreamErrorKind>),
    #[error("audit database context error: {0}")]
    AuditDatabaseContext(#[from] AuditDatabaseContextError),
    #[error("audit logs stream error: {0}")]
    AuditLogsStream(#[from] AuditLogsStreamError),
}

type Result<T> = std::result::Result<T, AuditLogsAppSetupError>;

/// Builds a naxum app for audit logs. Note that despite having an ack layer, all audit logs remain on the stream when
/// processed. This is because the audit logs stream is limits-based and is not a work queue. Sneaky!
#[instrument(
    name = "forklift.init.app.audit_logs.build_and_run",
    level = "debug",
    skip_all
)]
pub(crate) async fn build_and_run(
    jetstream_context: Context,
    durable_consumer_name: String,
    connection_metadata: Arc<ConnectionMetadata>,
    concurrency_limit: usize,
    audit_database_config: &AuditDatabaseConfig,
    token: CancellationToken,
) -> Result<Box<dyn Future<Output = io::Result<()>> + Unpin + Send>> {
    let incoming = {
        let stream = AuditLogsStream::get_or_create(jetstream_context).await?;
        let consumer_subject = stream.consuming_subject_for_all_workspaces();
        stream
            .stream()
            .await?
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                durable_name: Some(durable_consumer_name),
                filter_subject: consumer_subject.into_string(),
                ..Default::default()
            })
            .await?
            .messages()
            .await?
    };

    let context = AuditDatabaseContext::from_config(audit_database_config).await?;
    let state = AppState::new(context, connection_metadata.subject_prefix().is_some());

    // NOTE(nick,fletcher): the "NatsMakeSpan" builder defaults to "info" level logging. Bump it down, if needed.
    let app = ServiceBuilder::new()
        .layer(
            MatchedSubjectLayer::new().for_subject(ForkliftAuditLogsForSubject::with_prefix(
                connection_metadata.subject_prefix(),
            )),
        )
        .layer(
            TraceLayer::new()
                .make_span_with(telemetry_nats::NatsMakeSpan::builder(connection_metadata).build())
                .on_response(telemetry_nats::NatsOnResponse::new()),
        )
        .layer(AckLayer::new())
        .service(handlers::default.with_state(state))
        .map_response(Response::into_response);

    let inner =
        naxum::serve_with_incoming_limit(incoming, app.into_make_service(), concurrency_limit)
            .with_graceful_shutdown(naxum::wait_on_cancelled(token));

    Ok(Box::new(inner.into_future()))
}

#[derive(Clone, Debug)]
struct ForkliftAuditLogsForSubject {
    prefix: Option<()>,
}

impl ForkliftAuditLogsForSubject {
    fn with_prefix(prefix: Option<&str>) -> Self {
        Self {
            prefix: prefix.map(|_p| ()),
        }
    }
}

impl<R> ForSubject<R> for ForkliftAuditLogsForSubject
where
    R: MessageHead,
{
    fn call(&mut self, req: &mut naxum::Message<R>) {
        let mut parts = req.subject().split('.');

        match self.prefix {
            Some(_) => {
                if let (Some(prefix), Some(p1), Some(p2), Some(_workspace_id), None) = (
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                    parts.next(),
                ) {
                    let matched = format!("{prefix}.{p1}.{p2}.:workspace_id");
                    req.extensions_mut().insert(MatchedSubject::from(matched));
                };
            }
            None => {
                if let (Some(p1), Some(p2), Some(_workspace_id), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                {
                    let matched = format!("{p1}.{p2}.:workspace_id");
                    req.extensions_mut().insert(MatchedSubject::from(matched));
                };
            }
        }
    }
}
