use notbot::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let config_path = std::env::args().nth(1);

    tracing::trace!("provided config: {:#?}", config_path);
    let config = Config::from_path(config_path)?;

    tracing::trace!("parsed config as:\n{:#?}", config);

    tracing::info!("creating bot for {0}", config.user_id);
    notbot::run(config).await
}
