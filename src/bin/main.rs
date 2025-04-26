use futures::future::try_join;
use notbot::BotManager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // tracing_subscriber::fmt::init();
    console_subscriber::init();

    let config_path: String = std::env::args().nth(1).expect("no config path provided");

    tracing::trace!("provided config: {:#?}", config_path);
    let (notbot, tx) = &BotManager::new(config_path)
        .await
        .expect("initialization failed");

    let rx = tx.subscribe();

    let pair = try_join(notbot.reload(rx), notbot.run());

    pair.await.expect("critical error occured");

    Ok(())
}
