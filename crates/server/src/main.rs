use anyhow::Context as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    common::init_tracing().context("init tracing")?;

    let store = api::Store::new();
    let app = api::router(store);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .context("bind 127.0.0.1:3000")?;

    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await.context("serve")?;
    Ok(())
}

