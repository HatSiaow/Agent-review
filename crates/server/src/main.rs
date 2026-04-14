use anyhow::Context as _;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    common::init_tracing().context("init tracing")?;

    let bind_addr = std::env::var("APP_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());

    let store = api::Store::new();
    let app = api::router(store);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("bind {bind_addr}"))?;

    tracing::info!("listening on {}", listener.local_addr()?);

    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutdown signal received");
        cancel2.cancel();
    });

    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await });

    server.await.context("serve")?;
    Ok(())
}

