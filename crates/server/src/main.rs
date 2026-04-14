use anyhow::Context as _;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    common::init_tracing().context("init tracing")?;

    let bind_addr = std::env::var("APP_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());

    let store = api::Store::new();
    let app = api::router(store.clone());

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

    // Background workers (in-process) — minimal wiring for v0.1.
    spawn_workers(store.clone(), cancel.clone());

    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await });

    server.await.context("serve")?;
    Ok(())
}

fn spawn_workers(store: api::Store, cancel: CancellationToken) {
    tokio::spawn(ingestion_worker(store.clone(), cancel.clone()));
    tokio::spawn(agent_worker(store.clone(), cancel.clone()));
    tokio::spawn(poster_worker(store.clone(), cancel.clone()));
    tokio::spawn(notifier_worker(store, cancel));
}

async fn ingestion_worker(store: api::Store, cancel: CancellationToken) {
    let google_cfg = adapter_google::GoogleConfig::default();
    let ubereats_cfg = adapter_ubereats::UberEatsConfig::default();

    let mut google_tick =
        tokio::time::interval(std::time::Duration::from_secs(google_cfg.poll_interval_secs));
    let mut ubereats_tick =
        tokio::time::interval(std::time::Duration::from_secs(ubereats_cfg.poll_interval_secs));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("ingestion worker stopped");
                return;
            }
            _ = google_tick.tick() => {
                let cfg = google_cfg.clone();
                let store2 = store.clone();
                let _ = tokio::task::spawn_blocking(move || adapter_google::HttpGoogleClient::new().list_reviews(&cfg))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .map(|raws| async move {
                        for raw in raws {
                            if let Ok(review) = adapter_google::normalize_google_review(&raw) {
                                store2.ingest_review(review).await;
                            }
                        }
                    })
                    .map(|f| tokio::spawn(f));
            }
            _ = ubereats_tick.tick() => {
                let cfg = ubereats_cfg.clone();
                let store2 = store.clone();
                let _ = tokio::task::spawn_blocking(move || adapter_ubereats::HttpUberEatsClient::new().list_reviews(&cfg))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .map(|raws| async move {
                        for raw in raws {
                            if let Ok(review) = adapter_ubereats::normalize_ubereats_review(&raw) {
                                store2.ingest_review(review).await;
                            }
                        }
                    })
                    .map(|f| tokio::spawn(f));
            }
        }
    }
}

async fn agent_worker(store: api::Store, cancel: CancellationToken) {
    let agent_cfg = agent::AgentConfig::default();

    // Prefer Anthropic if configured; otherwise use the deterministic in-memory fake.
    let llm: Box<dyn llm_client::LlmClient> = if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        let mut cfg = llm_client::LlmConfig::default();
        cfg.api_key = Some(key);
        Box::new(llm_client::AnthropicClient::new(cfg))
    } else {
        Box::new(llm_client::InMemoryLlm::default())
    };

    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("agent worker stopped");
                return;
            }
            _ = tick.tick() => {
                let items = store.list_reviews().await;
                for (review, active) in items {
                    if review.status != domain::ReviewStatus::New {
                        continue;
                    }
                    if active.is_some() {
                        continue;
                    }

                    // Draft immediately for new reviews.
                    match agent::run_agent(llm.as_ref(), &agent_cfg, &review, None).await {
                        Ok(res) => {
                            store.store_agent_draft(res.draft).await;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, review_id = %review.id, "agent drafting failed");
                        }
                    }
                }
            }
        }
    }
}

async fn poster_worker(store: api::Store, cancel: CancellationToken) {
    let poster_cfg = poster::PosterConfig::default();

    // Construct a real HTTP poster when env is present; otherwise no-op (tests/dev).
    let google_cfg = adapter_google::GoogleConfig::default();
    let ubereats_cfg = adapter_ubereats::UberEatsConfig::default();
    let http_poster = poster::HttpPlatformPoster::new(
        Some((adapter_google::HttpGoogleClient::new(), google_cfg)),
        Some((adapter_ubereats::HttpUberEatsClient::new(), ubereats_cfg)),
    );

    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("poster worker stopped");
                return;
            }
            _ = tick.tick() => {
                let drafts = store.list_drafts().await;
                for draft in drafts {
                    if draft.state != domain::DraftState::Approved {
                        continue;
                    }
                    if draft.posted_at.is_some() {
                        continue;
                    }

                    let (review, _) = match store.get_review(draft.review_id).await {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    if poster::validate_for_posting(&draft).is_err() {
                        continue;
                    }

                    let result = poster::post_with_retries(
                        &http_poster,
                        &poster_cfg,
                        review.platform,
                        &review.source_review_id,
                        &draft.text,
                    ).await;

                    match result {
                        Ok(()) => {
                            let _ = store.mark_draft_posted(draft.id, time::OffsetDateTime::now_utc()).await;
                        }
                        Err(e) => {
                            let _ = store.mark_draft_post_failed(draft.id, e.to_string()).await;
                        }
                    }
                }
            }
        }
    }
}

async fn notifier_worker(store: api::Store, cancel: CancellationToken) {
    // Minimal notifier: send DraftReady for new pending_review drafts once per draft id.
    let mut sent: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
    let sender = notifier::InMemoryNotificationSender::default();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("notifier worker stopped");
                return;
            }
            _ = tick.tick() => {
                let drafts = store.list_drafts().await;
                for d in drafts {
                    if d.state != domain::DraftState::PendingReview {
                        continue;
                    }
                    if sent.contains(&d.id) {
                        continue;
                    }
                    let _ = notifier::dispatch_notification(
                        &sender,
                        &notifier::DispatchParams{
                            notification_type: domain::NotificationType::DraftReady,
                            recipient: "owner@example.com",
                            subject: "Draft ready",
                            body: "A reply draft is ready for review.",
                            entity_id: Some(d.review_id),
                            quiet_hours: None,
                            current_hour: 12,
                        }
                    ).await;
                    sent.insert(d.id);
                }
            }
        }
    }
}

