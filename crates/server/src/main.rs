use anyhow::Context as _;

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
    axum::serve(listener, app).await.context("serve")?;
    Ok(())
}

