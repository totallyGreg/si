use std::{
    collections::{HashMap, HashSet},
    env,
    str::FromStr,
};

use council_client::{AttributeValueId, ChangeSetPk, Client, Status};
use futures::TryStreamExt as _;
use si_data_nats::{NatsClient, NatsConfig};
use telemetry::prelude::*;
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    prelude::*,
    EnvFilter, Registry,
};

const TRACING_LOG_ENV_VAR: &str = "SI_LOG";
const DEFAULT_TRACING_DIRECTIVES: &str =
    "simple=trace,council_client=trace,si_data_nats=trace,info";

#[tokio::main]
async fn main() -> Result<(), Box<(dyn std::error::Error + 'static)>> {
    Registry::default()
        .with(
            EnvFilter::try_from_env(TRACING_LOG_ENV_VAR)
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_TRACING_DIRECTIVES)),
        )
        .with(
            fmt::layer()
                .with_thread_ids(true)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE),
        )
        .try_init()?;

    run().await
}

#[instrument(name = "main", level = "info", skip_all)]
async fn run() -> Result<(), Box<(dyn std::error::Error + 'static)>> {
    let client = {
        #[allow(clippy::disallowed_methods)] // Allowing example to override default
        let url = env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_owned());
        let config = NatsConfig {
            url,
            ..Default::default()
        };
        let nats = NatsClient::new(&config).await?;
        Client::from_services(nats, None).await?
    };

    let change_set_id = match env::args().nth(1) {
        Some(s) => ChangeSetPk::from_str(&s)?,
        None => ChangeSetPk::default(),
    };

    let mut vals = HashSet::new();
    let mut failed = HashSet::new();

    let mut leaf_that_will_fail = None;

    let mut dependency_graph = HashMap::new();
    for _ in 0..1 {
        let depends_on_1 = AttributeValueId::default();
        let depends_on_2 = AttributeValueId::default();
        let target = AttributeValueId::default();
        dependency_graph.insert(target, vec![depends_on_1, depends_on_2]);
        vals.insert(depends_on_1);
        vals.insert(depends_on_2);
        vals.insert(target);
        // Last leaf through this loop will be the "winning failure"
        leaf_that_will_fail = Some(depends_on_2);
    }

    // let depends_on: Vec<_> = dependency_graph.keys().copied().collect();
    // let last_target = AttributeValueId::default();
    // dependency_graph.insert(last_target, depends_on);
    // vals.insert(last_target);

    if let Some(leaf_that_will_fail) = leaf_that_will_fail {
        warn!(attribute_value_id = %leaf_that_will_fail, "value *will* fail!");
    }

    info!(%change_set_id, "collaborating on change set");
    let collaboration = client
        .collaborate_on_change_set(change_set_id, dependency_graph)
        .await?;

    let client_id = collaboration.id();

    info!(
        %client_id,
        ?vals,
        "collaboration client is running",
    );
    let mut statuses = collaboration.statuses().await?;
    let acker = collaboration.clone_acker();

    // Consume a stream of status messages that direct the work we perform to help drive the
    // change set collaboration to completion
    while let Some(status) = statuses.try_next().await? {
        debug!(?status, "incoming status");

        match status {
            // We receive a request for our client to process an attribute value
            Status::Process(attribute_value_id) => {
                info!(%attribute_value_id, "<-- got [Pending->Process] status, starting to process");

                // Sending an "ack pending" signals that we intend to start work on the attribute
                // value
                info!(
                    %attribute_value_id,
                    "--> sending <Ack pending>",
                );
                acker.ack_pending(attribute_value_id).await?;

                // // Simulate work with a short delay
                // tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                match leaf_that_will_fail {
                    // The "winning failure" will signal that it wasn't successfully processed
                    Some(leaf_that_will_fail) if leaf_that_will_fail == attribute_value_id => {
                        // Sending a "nack processed" signals that we failed to process the
                        // attribute value
                        warn!(
                            %attribute_value_id,
                            "--> sending <Nack PROCESSED> (value processing simulated error)",
                        );
                        acker.nack_processed(attribute_value_id).await?;
                    }
                    // Otherwise, value was successfully processed
                    _ => {
                        // Sending an "ack processed" signals that we processed the attribute value
                        info!(
                            %attribute_value_id,
                            "--> sending <Ack PROCESSED>",
                        );
                        acker.ack_processed(attribute_value_id).await?;
                    }
                }
            }
            // A status that tells us an attribute value has been processed and can be removed from
            // any work-in-flight or removed from any dependent work
            Status::Available(attribute_value_id) => {
                // In our case we simulate the available attribute value by removing it from our
                // "values of interest" collection
                vals.remove(&attribute_value_id);

                info!(%attribute_value_id, ?vals, "<-- got [Available] status");
            }
            // A status that tells us an attribute value has failed to be processed and will never
            // be re-attempted in this collaboration.
            Status::Failed(attribute_value_id) => {
                // In our case, we will remove the attribute value from our "values of interest"
                // collection and add it to our "failed" collection
                vals.remove(&attribute_value_id);
                failed.insert(attribute_value_id);

                error!(%attribute_value_id, ?vals, "<-- got [Failed] status");
            }
            Status::Orphaned(_) => todo!(),
            Status::ProcessingByAnother(_, _) => todo!(),
        }
    }

    // When the stream closes naturally we know that the change set collaboration has successfully
    // completed.
    info!("collaboration was successful!");

    assert!(vals.is_empty());

    for f in failed {
        error!(attribute_value_id = %f, "value failed to be processed");
    }

    // There is no need to explicitly shutdown the client-side after a successful completion, but
    // the server will still accept the message and is effectively a no-op
    info!(
        %client_id,
        "explicitly shutting down collaboration client (not normally required)",
    );
    statuses.shutdown().await?;

    Ok(())
}
