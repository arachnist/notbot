use futures::future::try_join;
use notbot::BotManager;
use tokio::sync::mpsc::channel;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config_path: String = std::env::args().nth(1).expect("no config path provided");

    match std::env::args().nth(2) {
        None => {
            tracing_subscriber::fmt::init();
            tracing::info!("using tracing subscriber");
        }
        Some(opt) => {
            if opt == "--tokio-console" {
                console_subscriber::ConsoleLayer::builder()
                    .retention(std::time::Duration::from_secs(6))
                    .init();
                tracing::info!("using console subscriber");
            } else {
                tracing_subscriber::fmt::init();
                tracing::info!("using tracing subscriber");
            };
        }
    }

    tracing::trace!("provided config: {:#?}", config_path);
    let (tx, rx) = channel::<matrix_sdk::Room>(1);
    let notbot = &BotManager::new(config_path, tx)
        .await
        .expect("initialization failed");

    let pair = try_join(notbot.reload(rx), notbot.run());

    pair.await.expect("critical error occured");

    Ok(())
}
