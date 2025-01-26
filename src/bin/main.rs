use notbot::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let config_path = std::env::args().nth(1);

    tracing::info!("provided config: {:#?}", config_path.clone().unwrap());
    let config = Config::from_path(config_path)?;

    tracing::info!("creating bot for {0}", config.user_id);
    notbot::run(config).await
}
