use adapter_google::GoogleReviewClient as _;
use adapter_ubereats::UberEatsReviewClient as _;
use anyhow::Context as _;
use secrecy::ExposeSecret as _;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    common::init_tracing().context("init tracing")?;

    let bind_addr = std::env::var("APP_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());

    let secrets = build_secrets_backend().await.context("init secrets backend")?;

    let session_hmac_key = load_required_secret_bytes(&*secrets, "app.session_secret", "APP_SESSION_SECRET")
        .await
        .context("load session secret")?;
    if session_hmac_key.len() < 32 {
        anyhow::bail!("app.session_secret/APP_SESSION_SECRET must be at least 32 bytes");
    }

    let ubereats_webhook_secret =
        load_optional_secret_bytes(&*secrets, "ubereats.webhook_secret", "UBEREATS_WEBHOOK_SECRET")
            .await
            .context("load ubereats webhook secret")?;

    let store = if let Some(cfg) = storage::PgRepositoryConfig::from_env() {
        let repo = storage::PgRepository::connect(&cfg)
            .await
            .context("connect db")?;
        repo.migrate().await.context("migrate db")?;
        api::Store::from_parts(
            std::sync::Arc::new(repo),
            secrets.clone(),
            session_hmac_key,
            ubereats_webhook_secret,
        )
    } else {
        api::Store::from_parts(
            std::sync::Arc::new(storage::InMemoryRepository::new()),
            secrets.clone(),
            session_hmac_key,
            ubereats_webhook_secret,
        )
    };
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

    let server =
        axum::serve(listener, app).with_graceful_shutdown(async move { cancel.cancelled().await });

    server.await.context("serve")?;
    Ok(())
}

fn spawn_workers(store: api::Store, cancel: CancellationToken) {
    tokio::spawn(ingestion_worker(store.clone(), cancel.clone()));
    tokio::spawn(agent_worker(store.clone(), cancel.clone()));
    tokio::spawn(poster_worker(store.clone(), cancel.clone()));
    tokio::spawn(sla_worker(store.clone(), cancel.clone()));
    tokio::spawn(notifier_worker(store, cancel));
}

fn should_process_google_review(
    last_seen: Option<time::OffsetDateTime>,
    review_updated_at: time::OffsetDateTime,
) -> bool {
    last_seen.is_none_or(|cursor| review_updated_at > cursor)
}

fn review_cursor_time(review: &domain::Review) -> time::OffsetDateTime {
    std::cmp::max(review.created_at, review.updated_at)
}

fn should_process_ubereats_review(
    last_seen: Option<time::OffsetDateTime>,
    review: &domain::Review,
) -> bool {
    last_seen.is_none_or(|cursor| review_cursor_time(review) > cursor)
}

fn parse_hour_component(value: &str) -> Option<u8> {
    let raw = value.trim();
    let hour = raw
        .split(':')
        .next()
        .and_then(|h| h.trim().parse::<u8>().ok())?;
    (hour < 24).then_some(hour)
}

fn parse_quiet_hours(value: &str) -> Option<notifier::QuietHours> {
    let (start, end) = value.split_once('-').or_else(|| value.split_once(','))?;
    Some(notifier::QuietHours {
        start_hour: parse_hour_component(start)?,
        end_hour: parse_hour_component(end)?,
    })
}

fn stable_notification_id(kind: &str, review_id: uuid::Uuid) -> uuid::Uuid {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(review_id.as_bytes());
    let digest = hasher.finalize();

    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

fn google_config_from_env() -> adapter_google::GoogleConfig {
    let mut cfg = adapter_google::GoogleConfig::default();
    if let Ok(v) = std::env::var("GOOGLE_ACCOUNT_ID") {
        cfg.account_id = v;
    }
    if let Ok(v) = std::env::var("GOOGLE_LOCATION_ID") {
        cfg.location_id = v;
    }
    if let Ok(v) = std::env::var("GOOGLE_POLL_INTERVAL_SECS")
        .and_then(|v| v.parse::<u64>().map_err(|_| std::env::VarError::NotPresent))
    {
        cfg.poll_interval_secs = v;
    }
    if let Ok(v) = std::env::var("GOOGLE_API_BASE_URL") {
        cfg.api_base_url = v;
    }
    if let Ok(v) = std::env::var("GOOGLE_OAUTH_TOKEN_URL") {
        cfg.oauth_token_url = v;
    }
    if let Ok(v) = std::env::var("GOOGLE_OAUTH_CLIENT_ID") {
        cfg.oauth_client_id = v;
    }
    if let Ok(v) = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET") {
        cfg.oauth_client_secret = v;
    }
    cfg
}

fn ubereats_config_from_env() -> adapter_ubereats::UberEatsConfig {
    let mut cfg = adapter_ubereats::UberEatsConfig::default();
    if let Ok(v) = std::env::var("UBEREATS_STORE_ID") {
        cfg.store_id = v;
    }
    if let Ok(v) = std::env::var("UBEREATS_WEBHOOK_SECRET") {
        cfg.webhook_secret = v;
    }
    if let Ok(v) = std::env::var("UBEREATS_POLL_INTERVAL_SECS")
        .and_then(|v| v.parse::<u64>().map_err(|_| std::env::VarError::NotPresent))
    {
        cfg.poll_interval_secs = v;
    }
    if let Ok(v) = std::env::var("UBEREATS_API_BASE_URL") {
        cfg.api_base_url = v;
    }
    if let Ok(v) = std::env::var("UBEREATS_OAUTH_TOKEN_URL") {
        cfg.oauth_token_url = v;
    }
    if let Ok(v) = std::env::var("UBEREATS_OAUTH_CLIENT_ID") {
        cfg.oauth_client_id = v;
    }
    if let Ok(v) = std::env::var("UBEREATS_OAUTH_CLIENT_SECRET") {
        cfg.oauth_client_secret = v;
    }
    cfg
}

async fn ingestion_worker(store: api::Store, cancel: CancellationToken) {
    let mut google_cfg = google_config_from_env();
    let mut ubereats_cfg = ubereats_config_from_env();

    if let Ok(tok) = store.get_secret_string("google.oauth_refresh_token").await {
        google_cfg.oauth_refresh_token = tok.expose_secret().to_string();
    } else if let Ok(v) = std::env::var("GOOGLE_OAUTH_REFRESH_TOKEN") {
        // Back-compat only; production should use secrets backend.
        google_cfg.oauth_refresh_token = v;
    }

    if let Ok(secret) = store.get_secret_string("ubereats.oauth_client_secret").await {
        ubereats_cfg.oauth_client_secret = secret.expose_secret().to_string();
    }

    let mut google_tick = tokio::time::interval(std::time::Duration::from_secs(
        google_cfg.poll_interval_secs,
    ));
    let mut ubereats_tick = tokio::time::interval(std::time::Duration::from_secs(
        ubereats_cfg.poll_interval_secs,
    ));

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("ingestion worker stopped");
                return;
            }
            _ = google_tick.tick() => {
                let cfg = google_cfg.clone();
                let store2 = store.clone();
                let last_seen = store2.get_reviews_sync_state(domain::Platform::Google).await;
                if let Ok(Ok(raws)) = tokio::task::spawn_blocking(move || {
                    adapter_google::HttpGoogleClient::new().list_reviews(&cfg)
                }).await {
                    let mut max_seen: Option<time::OffsetDateTime> = last_seen;
                    for raw in raws {
                        if let Ok(review) = adapter_google::normalize_google_review(&raw) {
                            if !should_process_google_review(last_seen, review.updated_at) {
                                break; // stop-at-watermark
                            }
                            max_seen = Some(max_seen.map_or(review.updated_at, |m| m.max(review.updated_at)));
                            store2.ingest_review(review).await;
                        }
                    }
                    if let Some(max_seen) = max_seen {
                        store2.set_reviews_sync_state(domain::Platform::Google, max_seen).await;
                    }
                }
            }
            _ = ubereats_tick.tick() => {
                let cfg = ubereats_cfg.clone();
                let store2 = store.clone();
                let last_seen = store2.get_reviews_sync_state(domain::Platform::Ubereats).await;
                if let Ok(Ok(raws)) = tokio::task::spawn_blocking(move || {
                    adapter_ubereats::HttpUberEatsClient::new().list_reviews(&cfg)
                }).await {
                    let mut max_seen: Option<time::OffsetDateTime> = last_seen;
                    for raw in raws {
                        if let Ok(review) = adapter_ubereats::normalize_ubereats_review(&raw) {
                            if !should_process_ubereats_review(last_seen, &review) {
                                continue;
                            }
                            let cursor_time = review_cursor_time(&review);
                            max_seen = Some(max_seen.map_or(cursor_time, |m| m.max(cursor_time)));
                            store2.ingest_review(review).await;
                        }
                    }
                    if let Some(max_seen) = max_seen {
                        store2
                            .set_reviews_sync_state(domain::Platform::Ubereats, max_seen)
                            .await;
                    }
                }
            }
        }
    }
}

async fn agent_worker(store: api::Store, cancel: CancellationToken) {
    // Prefer Anthropic if configured; otherwise use the deterministic in-memory fake.
    let anthropic_key = store
        .get_secret_string("anthropic.api_key")
        .await
        .ok()
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok().map(secrecy::SecretString::new));

    let llm: Box<dyn llm_client::LlmClient> = if let Some(key) = anthropic_key {
        Box::new(llm_client::AnthropicClient::new(llm_client::LlmConfig {
            api_key: Some(key.expose_secret().to_string()),
            ..llm_client::LlmConfig::default()
        }))
    } else {
        Box::new(llm_client::InMemoryLlm::default())
    };

    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("agent worker stopped");
                return;
            }
            _ = tick.tick() => {
                let agent_cfg = match store.get_restaurant_settings().await {
                    Ok(s) => agent::agent_config_from_settings(&s),
                    Err(_) => agent::AgentConfig::default(),
                };
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
                            let draft_id = res.draft.id;
                            let review_id = res.draft.review_id;
                            store.store_agent_draft(res.draft).await;
                            store
                                .enqueue_notification_outbox(
                                    uuid::Uuid::new_v4(),
                                    time::OffsetDateTime::now_utc(),
                                    domain::NotificationType::DraftReady,
                                    Some(review_id),
                                    Some(draft_id),
                                    serde_json::json!({
                                        "review_id": review_id,
                                        "draft_id": draft_id,
                                        "kind": "draft_ready"
                                    }),
                                )
                                .await;

                            let run = domain::AgentRun {
                                id: uuid::Uuid::new_v4(),
                                review_id,
                                draft_id: Some(draft_id),
                                model_name: Some(res.model_name),
                                prompt_fingerprint: Some(res.prompt_fingerprint),
                                prompt_tokens: Some(res.prompt_tokens),
                                completion_tokens: Some(res.completion_tokens),
                                latency_ms: Some(res.latency_ms),
                                tool_calls_json: res.tool_calls_json,
                                guardrail_verdict_json: serde_json::to_value(res.guardrail_result)
                                    .ok(),
                                error: None,
                                created_at: time::OffsetDateTime::now_utc(),
                            };
                            store.store_agent_run(run).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Platform, Review, ReviewAuthor, ReviewStatus};
    use serde_json::json;
    use time::macros::datetime;

    #[test]
    fn google_stop_at_watermark_allows_strictly_newer() {
        let cursor = Some(datetime!(2026-04-10 12:00:00 UTC));
        assert!(should_process_google_review(
            cursor,
            datetime!(2026-04-10 12:00:01 UTC)
        ));
        assert!(!should_process_google_review(
            cursor,
            datetime!(2026-04-10 12:00:00 UTC)
        ));
        assert!(!should_process_google_review(
            cursor,
            datetime!(2026-04-10 11:59:59 UTC)
        ));
    }

    #[test]
    fn google_stop_at_watermark_with_no_cursor_processes_all() {
        assert!(should_process_google_review(
            None,
            datetime!(2026-04-10 12:00:00 UTC)
        ));
    }

    fn sample_ubereats_review(updated_at: time::OffsetDateTime) -> Review {
        Review {
            id: uuid::Uuid::new_v4(),
            platform: Platform::Ubereats,
            source_review_id: "ue-1".to_string(),
            source_location_id: "store-1".to_string(),
            author: ReviewAuthor {
                display_name: "Jamie".to_string(),
                avatar_url: None,
            },
            rating: 5,
            body_text: Some("Great".to_string()),
            body_language: Some("en".to_string()),
            created_at: updated_at,
            updated_at,
            ingested_at: updated_at,
            existing_reply_text: None,
            existing_reply_updated_at: None,
            status: ReviewStatus::New,
            context_json: json!({}),
            raw_payload: json!({}),
        }
    }

    #[test]
    fn ubereats_cursor_only_processes_strictly_newer() {
        let cursor = Some(datetime!(2026-04-10 12:00:00 UTC));
        assert!(should_process_ubereats_review(
            cursor,
            &sample_ubereats_review(datetime!(2026-04-10 12:00:01 UTC))
        ));
        assert!(!should_process_ubereats_review(
            cursor,
            &sample_ubereats_review(datetime!(2026-04-10 12:00:00 UTC))
        ));
    }

    #[test]
    fn parse_quiet_hours_supports_simple_and_clock_formats() {
        let qh = parse_quiet_hours("22-8").expect("parse");
        assert_eq!(qh.start_hour, 22);
        assert_eq!(qh.end_hour, 8);

        let qh2 = parse_quiet_hours("22:00-08:00").expect("parse");
        assert_eq!(qh2.start_hour, 22);
        assert_eq!(qh2.end_hour, 8);
        assert!(parse_quiet_hours("bad-value").is_none());
    }

    #[test]
    fn stable_notification_id_is_deterministic() {
        let review_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        assert_eq!(
            stable_notification_id("sla_breach_2h", review_id),
            stable_notification_id("sla_breach_2h", review_id)
        );
    }
}

async fn poster_worker(store: api::Store, cancel: CancellationToken) {
    let poster_cfg = poster::PosterConfig::default();

    // Construct a real HTTP poster when env is present; otherwise no-op (tests/dev).
    let mut google_cfg = google_config_from_env();
    let mut ubereats_cfg = ubereats_config_from_env();
    if let Ok(tok) = store.get_secret_string("google.oauth_refresh_token").await {
        google_cfg.oauth_refresh_token = tok.expose_secret().to_string();
    } else if let Ok(v) = std::env::var("GOOGLE_OAUTH_REFRESH_TOKEN") {
        google_cfg.oauth_refresh_token = v;
    }
    if let Ok(secret) = store.get_secret_string("ubereats.oauth_client_secret").await {
        ubereats_cfg.oauth_client_secret = secret.expose_secret().to_string();
    }
    let http_poster = poster::HttpPlatformPoster::new(Some(google_cfg), Some(ubereats_cfg));

    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("poster worker stopped");
                return;
            }
            _ = tick.tick() => {
                let drafts = store.list_drafts().await;
                for draft in drafts {
                    if draft.posted_at.is_some() {
                        continue;
                    }
                    let now = time::OffsetDateTime::now_utc();
                    let eligible = match draft.state {
                        domain::DraftState::Approved => {
                            match draft.post_eligible_at {
                                None => true,
                                Some(t) => now >= t,
                            }
                        }
                        domain::DraftState::ApprovedPendingUndo => {
                            match draft.post_eligible_at {
                                None => false,
                                Some(t) => now >= t,
                            }
                        }
                        _ => false,
                    };
                    if !eligible {
                        continue;
                    }

                    let Ok((review, _)) = store.get_review(draft.review_id).await else {
                        continue;
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
                            store
                                .enqueue_notification_outbox(
                                    uuid::Uuid::new_v4(),
                                    time::OffsetDateTime::now_utc(),
                                    domain::NotificationType::PostFailed,
                                    Some(review.id),
                                    Some(draft.id),
                                    serde_json::json!({
                                        "review_id": review.id,
                                        "draft_id": draft.id,
                                        "platform": review.platform.to_string(),
                                        "error": e.to_string(),
                                        "kind": "post_failed"
                                    }),
                                )
                                .await;
                        }
                    }
                }
            }
        }
    }
}

async fn build_secrets_backend() -> anyhow::Result<std::sync::Arc<dyn secrets::Secrets>> {
    let backend = std::env::var("SECRETS_BACKEND").unwrap_or_else(|_| "memory".to_string());
    match backend.as_str() {
        "memory" => Ok(std::sync::Arc::new(secrets::InMemorySecrets::default())),
        "envfile" => {
            let path = std::env::var("APP_SECRETS_FILE").unwrap_or_else(|_| "secrets.env.age".into());
            let identity_file = std::env::var("APP_AGE_IDENTITY_FILE")
                .context("APP_AGE_IDENTITY_FILE is required for SECRETS_BACKEND=envfile")?;
            Ok(std::sync::Arc::new(secrets::EnvFileSecrets::new(path, identity_file)))
        }
        "aws" => {
            let prefix = std::env::var("APP_AWS_SECRETS_PREFIX").unwrap_or_else(|_| "rr-agent".into());
            let provider = secrets::AwsSecretsManagerSecrets::new(prefix).await?;
            Ok(std::sync::Arc::new(provider))
        }
        other => anyhow::bail!("unknown SECRETS_BACKEND={other} (expected memory|envfile|aws)"),
    }
}

async fn load_required_secret_bytes(
    secrets: &dyn secrets::Secrets,
    key: &str,
    fallback_env: &str,
) -> anyhow::Result<Vec<u8>> {
    if let Ok(v) = secrets.get(key).await {
        return Ok(v.expose_secret().as_bytes().to_vec());
    }
    Ok(std::env::var(fallback_env)?.into_bytes())
}

async fn load_optional_secret_bytes(
    secrets: &dyn secrets::Secrets,
    key: &str,
    fallback_env: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    if let Ok(v) = secrets.get(key).await {
        return Ok(Some(v.expose_secret().as_bytes().to_vec()));
    }
    Ok(std::env::var(fallback_env).ok().map(String::into_bytes))
}

async fn sla_worker(store: api::Store, cancel: CancellationToken) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("sla worker stopped");
                return;
            }
            _ = tick.tick() => {
                let now = time::OffsetDateTime::now_utc();
                let rows = store.list_reviews().await;
                for (review, active) in rows {
                    if review.status != domain::ReviewStatus::AwaitingHuman {
                        continue;
                    }
                    let Some(draft) = active else {
                        continue;
                    };
                    if !draft.is_active() {
                        continue;
                    }

                    let age = now - draft.created_at;
                    if age >= time::Duration::hours(72) {
                        let _ = store.skip_review(review.id).await;
                        store
                            .enqueue_notification_outbox(
                                stable_notification_id("sla_auto_skip_72h", review.id),
                                now,
                                domain::NotificationType::SlaEscalation,
                                Some(review.id),
                                Some(draft.id),
                                serde_json::json!({
                                    "kind": "sla_auto_skip_72h",
                                    "review_id": review.id,
                                    "draft_id": draft.id,
                                    "milestone_hours": 72
                                }),
                            )
                            .await;
                        continue;
                    }

                    if age >= time::Duration::hours(24) {
                        store
                            .enqueue_notification_outbox(
                                stable_notification_id("sla_escalation_24h", review.id),
                                now,
                                domain::NotificationType::SlaEscalation,
                                Some(review.id),
                                Some(draft.id),
                                serde_json::json!({
                                    "kind": "sla_escalation_24h",
                                    "review_id": review.id,
                                    "draft_id": draft.id,
                                    "milestone_hours": 24
                                }),
                            )
                            .await;
                    }

                    if age >= time::Duration::hours(2) {
                        store
                            .enqueue_notification_outbox(
                                stable_notification_id("sla_breach_2h", review.id),
                                now,
                                domain::NotificationType::SlaBreach,
                                Some(review.id),
                                Some(draft.id),
                                serde_json::json!({
                                    "kind": "sla_breach_2h",
                                    "review_id": review.id,
                                    "draft_id": draft.id,
                                    "milestone_hours": 2
                                }),
                            )
                            .await;
                    }
                }
            }
        }
    }
}

async fn notifier_worker(store: api::Store, cancel: CancellationToken) {
    // Durable notifier: claim from the notifications outbox and mark sent on success.
    #[derive(Debug)]
    enum Sender {
        Smtp(notifier::SmtpSender),
        InMemory(notifier::InMemoryNotificationSender),
    }

    impl notifier::NotificationSender for Sender {
        async fn send(
            &self,
            notification: &notifier::Notification,
        ) -> Result<(), notifier::NotifierError> {
            match self {
                Sender::Smtp(s) => s.send(notification).await,
                Sender::InMemory(s) => s.send(notification).await,
            }
        }
    }

    let sender = if let Some(cfg) = notifier::SmtpConfig::from_env() {
        Sender::Smtp(notifier::SmtpSender::new(cfg))
    } else {
        Sender::InMemory(notifier::InMemoryNotificationSender::default())
    };
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("notifier worker stopped");
                return;
            }
            _ = tick.tick() => {
                let quiet_hours = store
                    .get_restaurant_settings()
                    .await
                    .ok()
                    .and_then(|s| s.notifier_quiet_hours)
                    .and_then(|raw| parse_quiet_hours(&raw));
                let current_hour = time::OffsetDateTime::now_utc().hour();

                let batch = store.claim_notification_outbox_batch(50).await;
                let mut digest_ready = Vec::new();
                let mut immediate = Vec::new();
                for item in batch {
                    if item.notification_type == domain::NotificationType::DraftReady {
                        let should_digest = match item.review_id {
                            Some(review_id) => store
                                .get_review(review_id)
                                .await
                                .map(|(review, _)| review.rating >= 4)
                                .unwrap_or(false),
                            None => false,
                        };
                        if should_digest {
                            digest_ready.push(item);
                            continue;
                        }
                    }
                    immediate.push(item);
                }

                if !digest_ready.is_empty() {
                    let recipient = std::env::var("OWNER_NOTIFICATION_EMAIL")
                        .unwrap_or_else(|_| "owner@example.com".to_string());
                    let count = digest_ready.len();
                    let summary = digest_ready
                        .iter()
                        .map(|i| {
                            let review_id = i
                                .review_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "unknown-review".to_string());
                            let draft_id = i
                                .draft_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "unknown-draft".to_string());
                            format!("- review={review_id}, draft={draft_id}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let subject = if count == 1 {
                        "Draft ready digest (1 item)".to_string()
                    } else {
                        format!("Draft ready digest ({count} items)")
                    };
                    let body = format!(
                        "The following high-priority drafts are ready for review:\n{summary}"
                    );
                    let results = notifier::dispatch_notification(
                        &sender,
                        &notifier::DispatchParams {
                            notification_type: domain::NotificationType::DraftReady,
                            recipient: &recipient,
                            subject: &subject,
                            body: &body,
                            entity_id: None,
                            quiet_hours: quiet_hours.as_ref(),
                            current_hour,
                        },
                    )
                    .await;

                    if results.iter().all(Result::is_ok) {
                        let sent_at = time::OffsetDateTime::now_utc();
                        for item in digest_ready {
                            store.mark_notification_outbox_sent(item.id, sent_at).await;
                        }
                    } else {
                        tracing::warn!("draft_ready digest delivery failed");
                    }
                }

                for item in immediate {
                    // For v0.1, route all notifications to a single owner email address.
                    // Later this should map to real users + preferences.
                    let recipient = std::env::var("OWNER_NOTIFICATION_EMAIL")
                        .unwrap_or_else(|_| "owner@example.com".to_string());

                    let (subject, body) = match item.notification_type {
                        domain::NotificationType::DraftReady => (
                            "Draft ready".to_string(),
                            "A reply draft is ready for review.".to_string(),
                        ),
                        domain::NotificationType::PostFailed => (
                            "Reply failed to post".to_string(),
                            "A reply failed to post and needs attention.".to_string(),
                        ),
                        _ => (
                            format!("Notification: {}", item.notification_type),
                            "A notification event occurred.".to_string(),
                        ),
                    };

                    let results = notifier::dispatch_notification(
                        &sender,
                        &notifier::DispatchParams{
                            notification_type: item.notification_type,
                            recipient: &recipient,
                            subject: &subject,
                            body: &body,
                            entity_id: item.review_id.or(item.draft_id),
                            quiet_hours: quiet_hours.as_ref(),
                            current_hour,
                        },
                    )
                    .await;

                    if results.iter().all(Result::is_ok) {
                        store.mark_notification_outbox_sent(item.id, time::OffsetDateTime::now_utc()).await;
                    } else {
                        tracing::warn!(outbox_id = %item.id, "notification delivery failed");
                    }
                }
            }
        }
    }
}
