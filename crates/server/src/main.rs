use adapter_google::GoogleReviewClient as _;
use adapter_ubereats::UberEatsReviewClient as _;
use anyhow::Context as _;
use clap::Parser;
use secrecy::ExposeSecret as _;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(name = "server", about = "Neighbourhood Restaurant Review Agent server")]
struct ServerArgs {
    /// Comma-separated list of roles to run.
    /// Options: api,ingestion,agent,poster,notifier,sla,gc
    /// Default: all roles
    #[arg(long, default_value = "api,ingestion,agent,poster,notifier,sla,gc")]
    roles: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ServerRole {
    Api,
    Ingestion,
    Agent,
    Poster,
    Notifier,
    Sla,
    Gc,
}

impl ServerRole {
    fn from_str(s: &str) -> Option<Self> {
        match s.trim() {
            "api" => Some(Self::Api),
            "ingestion" => Some(Self::Ingestion),
            "agent" => Some(Self::Agent),
            "poster" => Some(Self::Poster),
            "notifier" => Some(Self::Notifier),
            "sla" => Some(Self::Sla),
            "gc" => Some(Self::Gc),
            other => {
                tracing::warn!("unknown server role ignored: {other}");
                None
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    common::init_tracing().context("init tracing")?;

    let args = ServerArgs::parse();
    let roles: std::collections::HashSet<ServerRole> = args
        .roles
        .split(',')
        .filter_map(ServerRole::from_str)
        .collect();

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

    let cancel = CancellationToken::new();

    // Respond to both SIGINT (Ctrl-C) and SIGTERM (e.g. `kill` / Kubernetes pod eviction).
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {
                tracing::info!("SIGINT received, shutting down");
            }
            _ = terminate => {
                tracing::info!("SIGTERM received, shutting down");
            }
        }
        cancel2.cancel();
    });

    // Background workers (in-process) — only roles explicitly enabled.
    spawn_workers(store.clone(), cancel.clone(), &roles);

    if roles.contains(&ServerRole::Api) {
        let app = api::router(store.clone());
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .with_context(|| format!("bind {bind_addr}"))?;
        tracing::info!("listening on {}", listener.local_addr()?);
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await
            .context("serve")?;
    } else {
        tracing::info!("api role not active; running background workers only");
        cancel.cancelled().await;
    }

    Ok(())
}

fn spawn_workers(
    store: api::Store,
    cancel: CancellationToken,
    roles: &std::collections::HashSet<ServerRole>,
) {
    if roles.contains(&ServerRole::Ingestion) {
        tokio::spawn(ingestion_worker(store.clone(), cancel.clone()));
    }
    if roles.contains(&ServerRole::Agent) {
        tokio::spawn(agent_worker(store.clone(), cancel.clone()));
    }
    if roles.contains(&ServerRole::Poster) {
        tokio::spawn(poster_worker(store.clone(), cancel.clone()));
    }
    if roles.contains(&ServerRole::Sla) {
        tokio::spawn(sla_worker(store.clone(), cancel.clone()));
    }
    if roles.contains(&ServerRole::Notifier) {
        tokio::spawn(notifier_worker(store.clone(), cancel.clone()));
    }
    if roles.contains(&ServerRole::Gc) {
        tokio::spawn(gc_worker(store, cancel));
    }
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

    let mut google_consecutive_failures: u32 = 0;
    let mut ubereats_consecutive_failures: u32 = 0;
    const INGESTION_FAILURE_THRESHOLD: u32 = 3;

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

                let result = tokio::task::spawn_blocking(move || {
                    adapter_google::HttpGoogleClient::new().list_reviews(&cfg)
                }).await;

                match result {
                    Ok(Ok(raws)) => {
                        google_consecutive_failures = 0;
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
                    Ok(Err(e)) => {
                        google_consecutive_failures += 1;
                        tracing::warn!(
                            error = %e,
                            consecutive_failures = google_consecutive_failures,
                            "google ingestion adapter error"
                        );
                        if google_consecutive_failures >= INGESTION_FAILURE_THRESHOLD {
                            let notification_id = stable_notification_id(
                                &format!("ingestion_failure_google_{}", google_consecutive_failures),
                                uuid::Uuid::from_u128(0),
                            );
                            store.enqueue_notification_outbox(
                                notification_id,
                                time::OffsetDateTime::now_utc(),
                                domain::NotificationType::IngestionFailure,
                                None,
                                None,
                                serde_json::json!({
                                    "kind": "ingestion_failure",
                                    "platform": "google",
                                    "consecutive_failures": google_consecutive_failures,
                                    "error": e.to_string()
                                }),
                            ).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "google ingestion spawn_blocking panicked");
                    }
                }
            }
            _ = ubereats_tick.tick() => {
                let cfg = ubereats_cfg.clone();
                let store2 = store.clone();
                let last_seen = store2.get_reviews_sync_state(domain::Platform::Ubereats).await;

                let result = tokio::task::spawn_blocking(move || {
                    adapter_ubereats::HttpUberEatsClient::new().list_reviews(&cfg)
                }).await;

                match result {
                    Ok(Ok(raws)) => {
                        ubereats_consecutive_failures = 0;
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
                    Ok(Err(e)) => {
                        ubereats_consecutive_failures += 1;
                        tracing::warn!(
                            error = %e,
                            consecutive_failures = ubereats_consecutive_failures,
                            "ubereats ingestion adapter error"
                        );
                        if ubereats_consecutive_failures >= INGESTION_FAILURE_THRESHOLD {
                            let notification_id = stable_notification_id(
                                &format!("ingestion_failure_ubereats_{}", ubereats_consecutive_failures),
                                uuid::Uuid::from_u128(0),
                            );
                            store.enqueue_notification_outbox(
                                notification_id,
                                time::OffsetDateTime::now_utc(),
                                domain::NotificationType::IngestionFailure,
                                None,
                                None,
                                serde_json::json!({
                                    "kind": "ingestion_failure",
                                    "platform": "ubereats",
                                    "consecutive_failures": ubereats_consecutive_failures,
                                    "error": e.to_string()
                                }),
                            ).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "ubereats ingestion spawn_blocking panicked");
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

    let worker_id = format!("agent-worker-{}", uuid::Uuid::new_v4());
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("agent worker stopped");
                return;
            }
            _ = tick.tick() => {
                let now = time::OffsetDateTime::now_utc();
                let jobs = store
                    .claim_work_jobs(storage::WorkJobType::AgentDraftReview, 5, &worker_id, now)
                    .await;

                for job in jobs {
                    let review_id = match job
                        .payload_json
                        .get("review_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    {
                        Some(id) => id,
                        None => {
                            tracing::warn!(job_id = %job.id, "agent work job missing review_id in payload");
                            store
                                .mark_work_job_failed(
                                    job.id,
                                    "missing review_id in payload",
                                    storage::WorkJobState::DeadLetter,
                                    now,
                                    now,
                                )
                                .await;
                            continue;
                        }
                    };

                    let (review, active_draft) = match store.get_review(review_id).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!(job_id = %job.id, review_id = %review_id, error = %e, "review not found for agent work job");
                            store
                                .mark_work_job_failed(
                                    job.id,
                                    "review not found",
                                    storage::WorkJobState::DeadLetter,
                                    now,
                                    now,
                                )
                                .await;
                            continue;
                        }
                    };

                    // Skip if already has a draft or is not in a draftable state.
                    if active_draft.is_some()
                        || !matches!(
                            review.status,
                            domain::ReviewStatus::New | domain::ReviewStatus::Drafting
                        )
                    {
                        store.mark_work_job_succeeded(job.id, now).await;
                        continue;
                    }

                    let agent_cfg = match store.get_restaurant_settings().await {
                        Ok(s) => agent::agent_config_from_settings(&s),
                        Err(_) => agent::AgentConfig::default(),
                    };

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
                            store.mark_work_job_succeeded(job.id, now).await;
                        }
                        Err(e) => {
                            tracing::warn!(job_id = %job.id, review_id = %review_id, error = %e, "agent drafting failed");
                            let attempts = job.attempts + 1;
                            let (next_state, run_after) = if attempts >= job.max_attempts {
                                (storage::WorkJobState::DeadLetter, now)
                            } else {
                                let delay_secs =
                                    5u64.saturating_mul(2u64.saturating_pow(attempts as u32));
                                (
                                    storage::WorkJobState::Pending,
                                    now + time::Duration::seconds(delay_secs as i64),
                                )
                            };
                            store
                                .mark_work_job_failed(
                                    job.id,
                                    &e.to_string(),
                                    next_state,
                                    run_after,
                                    now,
                                )
                                .await;
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

    let worker_id = format!("poster-worker-{}", uuid::Uuid::new_v4());
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("poster worker stopped");
                return;
            }
            _ = tick.tick() => {
                let now = time::OffsetDateTime::now_utc();
                let jobs = store
                    .claim_work_jobs(storage::WorkJobType::PosterPostReply, 5, &worker_id, now)
                    .await;

                for job in jobs {
                    let draft_id = match job
                        .payload_json
                        .get("draft_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    {
                        Some(id) => id,
                        None => {
                            tracing::warn!(job_id = %job.id, "poster work job missing draft_id in payload");
                            store
                                .mark_work_job_failed(
                                    job.id,
                                    "missing draft_id in payload",
                                    storage::WorkJobState::DeadLetter,
                                    now,
                                    now,
                                )
                                .await;
                            continue;
                        }
                    };

                    let review_id = match job
                        .payload_json
                        .get("review_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    {
                        Some(id) => id,
                        None => {
                            tracing::warn!(job_id = %job.id, "poster work job missing review_id in payload");
                            store
                                .mark_work_job_failed(
                                    job.id,
                                    "missing review_id in payload",
                                    storage::WorkJobState::DeadLetter,
                                    now,
                                    now,
                                )
                                .await;
                            continue;
                        }
                    };

                    let (review, active_draft) = match store.get_review(review_id).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!(job_id = %job.id, draft_id = %draft_id, error = %e, "review not found for poster work job");
                            store
                                .mark_work_job_failed(
                                    job.id,
                                    "review not found",
                                    storage::WorkJobState::DeadLetter,
                                    now,
                                    now,
                                )
                                .await;
                            continue;
                        }
                    };

                    // Verify the target draft is still the active draft for this review.
                    let draft = match active_draft {
                        Some(d) if d.id == draft_id => d,
                        _ => {
                            // Already posted, rejected, or superseded — nothing to do.
                            store.mark_work_job_succeeded(job.id, now).await;
                            continue;
                        }
                    };

                    if draft.posted_at.is_some() {
                        store.mark_work_job_succeeded(job.id, now).await;
                        continue;
                    }

                    // Check eligibility (state + undo window).
                    let eligible = match draft.state {
                        domain::DraftState::Approved => match draft.post_eligible_at {
                            None => true,
                            Some(t) => now >= t,
                        },
                        domain::DraftState::ApprovedPendingUndo => match draft.post_eligible_at {
                            None => false,
                            Some(t) => now >= t,
                        },
                        _ => false,
                    };

                    if !eligible {
                        // run_after is set to post_eligible_at so this should be rare (clock skew).
                        let retry_after = draft
                            .post_eligible_at
                            .unwrap_or_else(|| now + time::Duration::seconds(5));
                        store
                            .mark_work_job_failed(
                                job.id,
                                "not yet eligible for posting",
                                storage::WorkJobState::Pending,
                                retry_after,
                                now,
                            )
                            .await;
                        continue;
                    }

                    if poster::validate_for_posting(&draft).is_err() {
                        tracing::warn!(job_id = %job.id, draft_id = %draft_id, "draft failed posting validation");
                        store
                            .mark_work_job_failed(
                                job.id,
                                "draft failed posting validation",
                                storage::WorkJobState::DeadLetter,
                                now,
                                now,
                            )
                            .await;
                        continue;
                    }

                    let result = poster::post_with_retries(
                        &http_poster,
                        &poster_cfg,
                        review.platform,
                        &review.source_review_id,
                        &draft.text,
                    )
                    .await;

                    match result {
                        Ok(()) => {
                            let _ = store
                                .mark_draft_posted(draft.id, time::OffsetDateTime::now_utc())
                                .await;
                            store.mark_work_job_succeeded(job.id, now).await;
                        }
                        Err(e) => {
                            tracing::warn!(job_id = %job.id, draft_id = %draft_id, error = %e, "draft posting failed");
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
                            let attempts = job.attempts + 1;
                            let (next_state, run_after) = if attempts >= job.max_attempts {
                                (storage::WorkJobState::DeadLetter, now)
                            } else {
                                let delay_secs =
                                    5u64.saturating_mul(2u64.saturating_pow(attempts as u32));
                                (
                                    storage::WorkJobState::Pending,
                                    now + time::Duration::seconds(delay_secs as i64),
                                )
                            };
                            store
                                .mark_work_job_failed(
                                    job.id,
                                    &e.to_string(),
                                    next_state,
                                    run_after,
                                    now,
                                )
                                .await;
                        }
                    }
                }
            }
        }
    }
}

async fn gc_worker(store: api::Store, cancel: CancellationToken) {
    // Run GC once per hour.
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3_600));

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("gc worker stopped");
                return;
            }
            _ = tick.tick() => {
                let now = time::OffsetDateTime::now_utc();

                let raw_payload_cutoff = now - time::Duration::days(90);
                let agent_runs_cutoff = now - time::Duration::days(180);
                let outbox_cutoff = now - time::Duration::days(30);
                let webhook_cutoff = now - time::Duration::hours(24);
                let idempotency_cutoff = now - time::Duration::hours(24);

                let redacted = store.gc_redact_raw_payloads(raw_payload_cutoff).await;
                if redacted > 0 {
                    tracing::info!(count = redacted, "gc: redacted raw_payload on old reviews");
                }

                let deleted_runs = store.gc_delete_agent_runs(agent_runs_cutoff).await;
                if deleted_runs > 0 {
                    tracing::info!(count = deleted_runs, "gc: deleted old agent_runs");
                }

                let deleted_notifs = store.gc_delete_sent_notifications(outbox_cutoff).await;
                if deleted_notifs > 0 {
                    tracing::info!(count = deleted_notifs, "gc: deleted old sent notifications");
                }

                let deleted_webhooks = store.gc_delete_old_webhook_events(webhook_cutoff).await;
                if deleted_webhooks > 0 {
                    tracing::info!(count = deleted_webhooks, "gc: deleted old webhook_events");
                }

                let deleted_idempotency =
                    store.gc_delete_expired_idempotency_keys(idempotency_cutoff).await;
                if deleted_idempotency > 0 {
                    tracing::info!(count = deleted_idempotency, "gc: deleted expired idempotency keys");
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
                let now = time::OffsetDateTime::now_utc();

                // Release stale claims (worker crash recovery) — claims older than 5 minutes.
                let stale_cutoff = now - time::Duration::minutes(5);
                let released = store.release_stale_notification_claims(stale_cutoff).await;
                if released > 0 {
                    tracing::info!(released, "released stale notification outbox claims");
                }

                let quiet_hours = store
                    .get_restaurant_settings()
                    .await
                    .ok()
                    .and_then(|s| s.notifier_quiet_hours)
                    .and_then(|raw| parse_quiet_hours(&raw));
                let current_hour = now.hour();

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
