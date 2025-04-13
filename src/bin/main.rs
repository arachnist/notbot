use notbot::BotManager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config_path: String = std::env::args().nth(1).expect("no config path provided");

    tracing::trace!("provided config: {:#?}", config_path);
    let notbot = BotManager::new(config_path)
        .await
        .expect("initialization failed");

    notbot.run().await.expect("critical error occured");

    Ok(())
}
