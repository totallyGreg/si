use std::time::Duration;

use council_server::{Config, Server};
use si_service::{color_eyre, prelude::*, rt, shutdown, telemetry_application};

mod args;

fn main() -> Result<()> {
    rt::block_on("bin/council-tokio::runtime", async_main())
}

async fn async_main() -> Result<()> {
    let tracker = TaskTracker::new();
    let token = CancellationToken::new();

    color_eyre::install()?;
    let args = args::parse();
    let (mut telemetry, telemetry_shutdown) = {
        let config = TelemetryConfig::builder()
            .force_color(args.force_color.then_some(true))
            .no_color(args.no_color.then_some(true))
            .console_log_format(
                args.log_json
                    .then_some(ConsoleLogFormat::Json)
                    .unwrap_or_default(),
            )
            .service_name("council")
            .service_namespace("si")
            .log_env_var_prefix("SI")
            .app_modules(vec!["council", "council_server"])
            .interesting_modules(vec!["naxum", "si_data_nats"])
            .build()?;

        telemetry_application::init(config, &tracker, token.clone())?
    };

    if args.verbose > 0 {
        telemetry
            .set_verbosity_and_wait(args.verbose.into())
            .await?;
    }
    debug!(arguments =?args, "parsed cli arguments");

    let config = Config::try_from(args)?;

    let server = Server::from_config(config, token.clone()).await?;

    tracker.spawn(async move {
        info!("ready to receive messages");
        server.run().await
    });

    shutdown::graceful(
        tracker,
        token,
        Some(telemetry_shutdown.into_future()),
        Some(Duration::from_secs(10)),
    )
    .await
    .map_err(Into::into)
}
