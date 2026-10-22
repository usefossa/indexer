use fossa_indexer::{App, api, config::Config};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "fossa_indexer=info".into()),
        )
        .init();
    if let Some(path) = std::env::args_os().nth(1) {
        if path == "--help" || path == "-h" {
            println!(
                "fossa-indexer [ENV_FILE]\n\nReads .env by default; existing environment variables take precedence.\nRequired: FOSSA_RPC_URL and FOSSA_WS_URL or FOSSA_GRPC_URL.\nSee docs/index.md and .env.example for settings."
            );
            return;
        }
        if path == "--version" || path == "-V" {
            println!("fossa-indexer {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        if dotenvy::from_path(path).is_err() {
            eprintln!("Cannot read environment file");
            std::process::exit(1);
        }
    } else {
        let _ = dotenvy::dotenv();
    }
    if let Err(error) = serve().await {
        tracing::error!(%error, "indexer stopped");
        std::process::exit(1);
    }
}
async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let app = App::new(Config::from_env()?).await?;
    let listener = tokio::net::TcpListener::bind(app.config.bind).await?;
    let tasks = app.start();
    tracing::info!(address=%listener.local_addr()?, "indexer listening");
    let stop = app.shutdown.clone();
    axum::serve(
        listener,
        api::router(app).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown().await;
        stop.send_replace(true);
    })
    .await?;
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
