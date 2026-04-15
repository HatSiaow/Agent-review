use std::collections::HashMap;

use domain::{DraftState, ReplyDraft, RestaurantSettings, Review};
use sqlx::PgPool;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::repo::{
    DraftListQuery, NotificationOutboxItem, Repository, RepositoryError, RepositoryResult,
    ReviewListQuery, WorkJob, WorkJobState, WorkJobType,
};

/// Singleton row id for `restaurant_settings` (see migration `0005_restaurant_settings.sql`).
const RESTAURANT_SETTINGS_ID: Uuid = uuid::Uuid::from_u128(1);

#[derive(Debug, Clone)]
pub struct PgRepositoryConfig {
    pub database_url: String,
    pub max_connections: u32,
}

impl PgRepositoryConfig {
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let database_url = std::env::var("DATABASE_URL").ok()?;
        let max_connections = std::env::var("DATABASE_MAX_CONNECTIONS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        Some(Self {
            database_url,
            max_connections,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PgRepository {
    pool: PgPool,
}

impl PgRepository {
    pub async fn connect(config: &PgRepositoryConfig) -> Result<Self, sqlx::Error> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.max_connections)
            .connect(&config.database_url)
            .await?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Apply embedded SQL migrations for this crate.
    ///
    /// This is critical for correctness in production and CI: without applying migrations,
    /// background workers and the API can silently fail (missing tables/indexes) and
    /// the "single inbox" will lose or duplicate work.
    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        // Embeds `crates/storage/migrations/*.sql` into the binary at compile time.
        sqlx::migrate!("./migrations").run(&self.pool).await
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ReviewRow {
    id: Uuid,
    platform: String,
    source_review_id: String,
    source_location_id: String,
    author_display_name: String,
    author_avatar_url: Option<String>,
    rating: i16,
    body_text: Option<String>,
    body_language: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    ingested_at: OffsetDateTime,
    existing_reply_text: Option<String>,
    existing_reply_updated_at: Option<OffsetDateTime>,
    status: String,
    context_json: serde_json::Value,
    raw_payload: serde_json::Value,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct DraftRow {
    id: Uuid,
    review_id: Uuid,
    generated_by: String,
    model_name: Option<String>,
    prompt_fingerprint: Option<String>,
    text: String,
    language: String,
    char_count: i32,
    state: String,
    guardrail_warnings: serde_json::Value,
    flags: serde_json::Value,
    created_at: OffsetDateTime,
    reviewed_by: Option<Uuid>,
    reviewed_at: Option<OffsetDateTime>,
    rejection_reason: Option<String>,
    post_eligible_at: Option<OffsetDateTime>,
    posted_at: Option<OffsetDateTime>,
    platform_post_error: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AuditEventRow {
    id: Uuid,
    occurred_at: OffsetDateTime,
    actor_type: String,
    actor_id: Option<Uuid>,
    entity_type: String,
    entity_id: Uuid,
    event_type: String,
    details_json: serde_json::Value,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct WorkJobRow {
    id: Uuid,
    job_type: String,
    dedupe_key: String,
    payload_json: serde_json::Value,
    state: String,
    attempts: i32,
    max_attempts: i32,
    run_after: OffsetDateTime,
    locked_by: Option<String>,
    locked_at: Option<OffsetDateTime>,
    last_error: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AgentRunRow {
    id: Uuid,
    review_id: Uuid,
    draft_id: Option<Uuid>,
    model_name: Option<String>,
    prompt_fingerprint: Option<String>,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    latency_ms: Option<i32>,
    tool_calls_json: serde_json::Value,
    guardrail_verdict_json: Option<serde_json::Value>,
    error: Option<String>,
    created_at: OffsetDateTime,
}

fn parse_platform(s: &str) -> Result<domain::Platform, RepositoryError> {
    match s {
        "google" => Ok(domain::Platform::Google),
        "ubereats" => Ok(domain::Platform::Ubereats),
        _ => Err(RepositoryError::Storage("invalid platform".to_string())),
    }
}

fn parse_review_status(s: &str) -> Result<domain::ReviewStatus, RepositoryError> {
    match s {
        "new" => Ok(domain::ReviewStatus::New),
        "drafting" => Ok(domain::ReviewStatus::Drafting),
        "awaiting_human" => Ok(domain::ReviewStatus::AwaitingHuman),
        "replied" => Ok(domain::ReviewStatus::Replied),
        "withdrawn" => Ok(domain::ReviewStatus::Withdrawn),
        "skipped" => Ok(domain::ReviewStatus::Skipped),
        _ => Err(RepositoryError::Storage(
            "invalid review status".to_string(),
        )),
    }
}

fn parse_generator(s: &str) -> Result<domain::Generator, RepositoryError> {
    match s {
        "agent_llm" => Ok(domain::Generator::AgentLlm),
        "human_edit" => Ok(domain::Generator::HumanEdit),
        "template" => Ok(domain::Generator::Template),
        _ => Err(RepositoryError::Storage("invalid generator".to_string())),
    }
}

fn parse_draft_state(s: &str) -> Result<DraftState, RepositoryError> {
    match s {
        "pending_review" => Ok(DraftState::PendingReview),
        "approved" => Ok(DraftState::Approved),
        "approved_pending_undo" => Ok(DraftState::ApprovedPendingUndo),
        "edited" => Ok(DraftState::Edited),
        "rejected" => Ok(DraftState::Rejected),
        "posted" => Ok(DraftState::Posted),
        "failed" => Ok(DraftState::Failed),
        _ => Err(RepositoryError::Storage("invalid draft state".to_string())),
    }
}

fn parse_actor_type(s: &str) -> Result<domain::ActorType, RepositoryError> {
    match s {
        "user" => Ok(domain::ActorType::User),
        "system" => Ok(domain::ActorType::System),
        "agent" => Ok(domain::ActorType::Agent),
        _ => Err(RepositoryError::Storage("invalid actor_type".to_string())),
    }
}

fn parse_event_type(s: &str) -> Result<domain::EventType, RepositoryError> {
    match s {
        "review_ingested" => Ok(domain::EventType::ReviewIngested),
        "review_withdrawn" => Ok(domain::EventType::ReviewWithdrawn),
        "review_skipped" => Ok(domain::EventType::ReviewSkipped),
        "review_unskipped" => Ok(domain::EventType::ReviewUnskipped),
        "draft_created" => Ok(domain::EventType::DraftCreated),
        "draft_approved" => Ok(domain::EventType::DraftApproved),
        "draft_edited" => Ok(domain::EventType::DraftEdited),
        "draft_rejected" => Ok(domain::EventType::DraftRejected),
        "draft_posted" => Ok(domain::EventType::DraftPosted),
        "draft_post_failed" => Ok(domain::EventType::DraftPostFailed),
        "drift_detected" => Ok(domain::EventType::DriftDetected),
        "login_success" => Ok(domain::EventType::LoginSuccess),
        "login_failed" => Ok(domain::EventType::LoginFailed),
        "password_reset" => Ok(domain::EventType::PasswordReset),
        _ => Err(RepositoryError::Storage("invalid event_type".to_string())),
    }
}

fn review_from_row(row: ReviewRow) -> Result<Review, RepositoryError> {
    let avatar_url = match row.author_avatar_url {
        None => None,
        Some(s) => Some(Url::parse(&s).map_err(|_| RepositoryError::Storage("bad url".into()))?),
    };
    Ok(Review {
        id: row.id,
        platform: parse_platform(&row.platform)?,
        source_review_id: row.source_review_id,
        source_location_id: row.source_location_id,
        author: domain::ReviewAuthor {
            display_name: row.author_display_name,
            avatar_url,
        },
        rating: u8::try_from(row.rating)
            .map_err(|_| RepositoryError::Storage("bad rating".into()))?,
        body_text: row.body_text,
        body_language: row.body_language,
        created_at: row.created_at,
        updated_at: row.updated_at,
        ingested_at: row.ingested_at,
        existing_reply_text: row.existing_reply_text,
        existing_reply_updated_at: row.existing_reply_updated_at,
        status: parse_review_status(&row.status)?,
        context_json: row.context_json,
        raw_payload: row.raw_payload,
    })
}

fn draft_from_row(row: DraftRow) -> Result<ReplyDraft, RepositoryError> {
    let guardrail_warnings: Vec<String> = serde_json::from_value(row.guardrail_warnings)
        .map_err(|_| RepositoryError::Storage("bad guardrail_warnings".into()))?;
    let flags: Vec<String> = serde_json::from_value(row.flags)
        .map_err(|_| RepositoryError::Storage("bad flags".into()))?;
    Ok(ReplyDraft {
        id: row.id,
        review_id: row.review_id,
        generated_by: parse_generator(&row.generated_by)?,
        model_name: row.model_name,
        prompt_fingerprint: row.prompt_fingerprint,
        text: row.text,
        language: row.language,
        char_count: u32::try_from(row.char_count).unwrap_or(u32::MAX),
        state: parse_draft_state(&row.state)?,
        guardrail_warnings,
        flags,
        created_at: row.created_at,
        reviewed_by: row.reviewed_by,
        reviewed_at: row.reviewed_at,
        rejection_reason: row.rejection_reason,
        post_eligible_at: row.post_eligible_at,
        posted_at: row.posted_at,
        platform_post_error: row.platform_post_error,
    })
}

fn audit_from_row(row: AuditEventRow) -> Result<domain::AuditEvent, RepositoryError> {
    Ok(domain::AuditEvent {
        id: row.id,
        occurred_at: row.occurred_at,
        actor_type: parse_actor_type(&row.actor_type)?,
        actor_id: row.actor_id,
        entity_type: row.entity_type,
        entity_id: row.entity_id,
        event_type: parse_event_type(&row.event_type)?,
        details_json: row.details_json,
    })
}

fn agent_run_from_row(row: AgentRunRow) -> Result<domain::AgentRun, RepositoryError> {
    Ok(domain::AgentRun {
        id: row.id,
        review_id: row.review_id,
        draft_id: row.draft_id,
        model_name: row.model_name,
        prompt_fingerprint: row.prompt_fingerprint,
        prompt_tokens: row.prompt_tokens.and_then(|v| u32::try_from(v).ok()),
        completion_tokens: row.completion_tokens.and_then(|v| u32::try_from(v).ok()),
        latency_ms: row.latency_ms.and_then(|v| u64::try_from(v).ok()),
        tool_calls_json: row.tool_calls_json,
        guardrail_verdict_json: row.guardrail_verdict_json,
        error: row.error,
        created_at: row.created_at,
    })
}

impl PgRepository {
    async fn append_audit(&self, event: domain::AuditEvent) -> Result<(), RepositoryError> {
        sqlx::query(
            r"
            insert into audit_events (
              id, occurred_at, actor_type, actor_id,
              entity_type, entity_id, event_type, details_json
            ) values ($1,$2,$3,$4,$5,$6,$7,$8)
            ",
        )
        .bind(event.id)
        .bind(event.occurred_at)
        .bind(event.actor_type.to_string())
        .bind(event.actor_id)
        .bind(event.entity_type)
        .bind(event.entity_id)
        .bind(event.event_type.to_string())
        .bind(event.details_json)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Repository for PgRepository {
    async fn ping(&self) -> RepositoryResult<()> {
        sqlx::query_scalar::<_, i64>("select 1")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn list_reviews(&self) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>> {
        let reviews: Vec<ReviewRow> =
            sqlx::query_as("select * from reviews order by updated_at desc")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        let mut out = Vec::with_capacity(reviews.len());
        for r in reviews {
            let review = review_from_row(r)?;
            let d: Option<DraftRow> = sqlx::query_as(
                "select * from reply_drafts where review_id = $1 order by created_at desc limit 1",
            )
            .bind(review.id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
            let draft = d.map(draft_from_row).transpose()?;
            out.push((review, draft));
        }
        Ok(out)
    }

    async fn list_reviews_filtered(
        &self,
        query: ReviewListQuery,
    ) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>> {
        let rows = self.list_reviews().await?;
        Ok(crate::list_filters::filter_sort_reviews(rows, &query))
    }

    async fn get_review(&self, id: Uuid) -> RepositoryResult<(Review, Option<ReplyDraft>)> {
        let r: Option<ReviewRow> = sqlx::query_as("select * from reviews where id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        let Some(r) = r else {
            return Err(RepositoryError::NotFound);
        };
        let review = review_from_row(r)?;
        let d: Option<DraftRow> = sqlx::query_as(
            "select * from reply_drafts where review_id = $1 order by created_at desc limit 1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok((review, d.map(draft_from_row).transpose()?))
    }

    async fn ingest_review(&self, _review: Review) -> RepositoryResult<()> {
        let platform = _review.platform.to_string();
        let status = _review.status.to_string();
        let avatar_url = _review.author.avatar_url.as_ref().map(ToString::to_string);

        let stored_id: Option<Uuid> = sqlx::query_scalar(
            r"
            insert into reviews (
              id, platform, source_review_id, source_location_id,
              author_display_name, author_avatar_url, rating,
              body_text, body_language,
              created_at, updated_at, ingested_at,
              existing_reply_text, existing_reply_updated_at,
              status, context_json, raw_payload
            ) values (
              $1,$2,$3,$4,
              $5,$6,$7,
              $8,$9,
              $10,$11,$12,
              $13,$14,
              $15,$16,$17
            )
            on conflict (platform, source_review_id) do update set
              source_location_id = excluded.source_location_id,
              author_display_name = excluded.author_display_name,
              author_avatar_url = excluded.author_avatar_url,
              rating = excluded.rating,
              body_text = excluded.body_text,
              body_language = excluded.body_language,
              created_at = least(reviews.created_at, excluded.created_at),
              updated_at = greatest(reviews.updated_at, excluded.updated_at),
              ingested_at = excluded.ingested_at,
              existing_reply_text = excluded.existing_reply_text,
              existing_reply_updated_at = excluded.existing_reply_updated_at,
              context_json = excluded.context_json,
              raw_payload = excluded.raw_payload
            where
              excluded.updated_at > reviews.updated_at
              or excluded.rating <> reviews.rating
              or excluded.body_text is distinct from reviews.body_text
              or excluded.existing_reply_text is distinct from reviews.existing_reply_text
              or excluded.existing_reply_updated_at is distinct from reviews.existing_reply_updated_at
            returning reviews.id
            ",
        )
        .bind(_review.id)
        .bind(platform)
        .bind(_review.source_review_id)
        .bind(_review.source_location_id)
        .bind(_review.author.display_name)
        .bind(avatar_url)
        .bind(i16::from(_review.rating))
        .bind(_review.body_text)
        .bind(_review.body_language)
        .bind(_review.created_at)
        .bind(_review.updated_at)
        .bind(_review.ingested_at)
        .bind(_review.existing_reply_text)
        .bind(_review.existing_reply_updated_at)
        .bind(status)
        .bind(_review.context_json)
        .bind(_review.raw_payload)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        if let Some(stored_id) = stored_id {
            self.append_audit(domain::AuditEvent::new(
                domain::ActorType::System,
                None,
                "review",
                stored_id,
                domain::EventType::ReviewIngested,
                serde_json::json!({}),
            ))
            .await?;
        }
        Ok(())
    }

    async fn upsert_review_with_draft(
        &self,
        _review: Review,
        _draft: ReplyDraft,
    ) -> RepositoryResult<()> {
        self.ingest_review(_review).await?;
        self.store_agent_draft(_draft).await?;
        Ok(())
    }

    async fn get_reviews_sync_state(
        &self,
        platform: domain::Platform,
    ) -> RepositoryResult<Option<OffsetDateTime>> {
        let platform = platform.to_string();
        let t: Option<OffsetDateTime> = sqlx::query_scalar(
            r"
            select last_seen_update_time
            from reviews_sync_state
            where platform = $1
            ",
        )
        .bind(platform)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(t)
    }

    async fn set_reviews_sync_state(
        &self,
        platform: domain::Platform,
        last_seen_update_time: OffsetDateTime,
    ) -> RepositoryResult<()> {
        let platform = platform.to_string();
        sqlx::query(
            r"
            insert into reviews_sync_state (platform, last_seen_update_time)
            values ($1, $2)
            on conflict (platform) do update
            set last_seen_update_time = excluded.last_seen_update_time
            ",
        )
        .bind(platform)
        .bind(last_seen_update_time)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn register_webhook_event(
        &self,
        platform: domain::Platform,
        event_id: &str,
        received_at: OffsetDateTime,
    ) -> RepositoryResult<bool> {
        let platform = platform.to_string();
        let res = sqlx::query(
            r"
            insert into webhook_events (platform, event_id, received_at)
            values ($1, $2, $3)
            on conflict (platform, event_id) do nothing
            ",
        )
        .bind(platform)
        .bind(event_id)
        .bind(received_at)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(res.rows_affected() == 1)
    }

    async fn list_audit_events(
        &self,
        entity_type: &str,
        entity_id: Uuid,
    ) -> RepositoryResult<Vec<domain::AuditEvent>> {
        let rows: Vec<AuditEventRow> = sqlx::query_as(
            r"
            select *
            from audit_events
            where entity_type = $1 and entity_id = $2
            order by occurred_at desc
            ",
        )
        .bind(entity_type)
        .bind(entity_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        rows.into_iter().map(audit_from_row).collect()
    }

    async fn get_idempotency_response(
        &self,
        idempotency_key: &str,
    ) -> RepositoryResult<Option<(u16, serde_json::Value)>> {
        let row: Option<(i16, serde_json::Value)> = sqlx::query_as(
            r"
            select status, body_json
            from idempotency_responses
            where idempotency_key = $1
            ",
        )
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(row.map(|(status, body)| (u16::try_from(status).unwrap_or(500), body)))
    }

    async fn put_idempotency_response(
        &self,
        idempotency_key: &str,
        status: u16,
        body_json: serde_json::Value,
        created_at: OffsetDateTime,
    ) -> RepositoryResult<()> {
        let status_i16 = i16::try_from(status).unwrap_or(500);
        sqlx::query(
            r"
            insert into idempotency_responses (idempotency_key, status, body_json, created_at)
            values ($1, $2, $3, $4)
            on conflict (idempotency_key) do nothing
            ",
        )
        .bind(idempotency_key)
        .bind(status_i16)
        .bind(body_json)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn transition_review_to_drafting(&self, _review_id: Uuid) -> RepositoryResult<Review> {
        let res = sqlx::query(
            r"update reviews set status = 'drafting' where id = $1 and status in ('new','awaiting_human','skipped')",
        )
        .bind(_review_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::InvalidTransition);
        }
        let (review, _) = self.get_review(_review_id).await?;
        Ok(review)
    }

    async fn skip_review(&self, _review_id: Uuid) -> RepositoryResult<Review> {
        let res = sqlx::query(
            r"update reviews set status = 'skipped' where id = $1 and status in ('new','awaiting_human','drafting')",
        )
        .bind(_review_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::InvalidTransition);
        }
        let (review, _) = self.get_review(_review_id).await?;
        self.append_audit(domain::AuditEvent::new(
            domain::ActorType::System,
            None,
            "review",
            _review_id,
            domain::EventType::ReviewSkipped,
            serde_json::json!({}),
        ))
        .await?;
        Ok(review)
    }

    async fn unskip_review(&self, _review_id: Uuid) -> RepositoryResult<Review> {
        let res =
            sqlx::query(r"update reviews set status = 'new' where id = $1 and status = 'skipped'")
                .bind(_review_id)
                .execute(&self.pool)
                .await
                .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::InvalidTransition);
        }
        let (review, _) = self.get_review(_review_id).await?;
        self.append_audit(domain::AuditEvent::new(
            domain::ActorType::System,
            None,
            "review",
            _review_id,
            domain::EventType::ReviewUnskipped,
            serde_json::json!({}),
        ))
        .await?;
        Ok(review)
    }

    async fn list_drafts(&self) -> RepositoryResult<Vec<ReplyDraft>> {
        let rows: Vec<DraftRow> =
            sqlx::query_as("select * from reply_drafts order by created_at desc")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        rows.into_iter().map(draft_from_row).collect()
    }

    async fn list_drafts_filtered(
        &self,
        query: DraftListQuery,
    ) -> RepositoryResult<Vec<ReplyDraft>> {
        let drafts = self.list_drafts().await?;
        let reviews = self.list_reviews().await?;
        let map: HashMap<Uuid, Review> = reviews.into_iter().map(|(r, _)| (r.id, r)).collect();
        let pairs: Vec<(ReplyDraft, Review)> = drafts
            .into_iter()
            .filter_map(|d| map.get(&d.review_id).cloned().map(|r| (d, r)))
            .collect();
        Ok(crate::list_filters::filter_sort_drafts(pairs, &query))
    }

    async fn store_agent_draft(&self, _draft: ReplyDraft) -> RepositoryResult<()> {
        let gen = _draft.generated_by.to_string();
        let state = _draft.state.to_string();
        sqlx::query(
            r"
            insert into reply_drafts (
              id, review_id, generated_by, model_name, prompt_fingerprint,
              text, language, char_count, state,
              guardrail_warnings, flags,
              created_at,
              reviewed_by, reviewed_at, rejection_reason,
              post_eligible_at, posted_at, platform_post_error
            ) values (
              $1,$2,$3,$4,$5,
              $6,$7,$8,$9,
              $10,$11,
              $12,
              $13,$14,$15,
              $16,$17,$18
            )
            ",
        )
        .bind(_draft.id)
        .bind(_draft.review_id)
        .bind(gen)
        .bind(_draft.model_name)
        .bind(_draft.prompt_fingerprint)
        .bind(_draft.text)
        .bind(_draft.language)
        .bind(i32::try_from(_draft.char_count).unwrap_or(i32::MAX))
        .bind(state)
        .bind(
            serde_json::to_value(_draft.guardrail_warnings)
                .unwrap_or_else(|_| serde_json::json!([])),
        )
        .bind(serde_json::to_value(_draft.flags).unwrap_or_else(|_| serde_json::json!([])))
        .bind(_draft.created_at)
        .bind(_draft.reviewed_by)
        .bind(_draft.reviewed_at)
        .bind(_draft.rejection_reason)
        .bind(_draft.post_eligible_at)
        .bind(_draft.posted_at)
        .bind(_draft.platform_post_error)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        // Move review to awaiting_human once a draft exists.
        let _ = sqlx::query("update reviews set status = 'awaiting_human' where id = $1")
            .bind(_draft.review_id)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        self.append_audit(domain::AuditEvent::new(
            domain::ActorType::Agent,
            None,
            "draft",
            _draft.id,
            domain::EventType::DraftCreated,
            serde_json::json!({ "review_id": _draft.review_id }),
        ))
        .await?;

        Ok(())
    }

    async fn store_agent_run(&self, run: domain::AgentRun) -> RepositoryResult<()> {
        let prompt_tokens: Option<i32> = run.prompt_tokens.and_then(|v| i32::try_from(v).ok());
        let completion_tokens: Option<i32> =
            run.completion_tokens.and_then(|v| i32::try_from(v).ok());
        let latency_ms: Option<i32> = run.latency_ms.and_then(|v| i32::try_from(v).ok());

        sqlx::query(
            r"
            insert into agent_runs (
              id, review_id, draft_id,
              model_name, prompt_fingerprint,
              prompt_tokens, completion_tokens, latency_ms,
              tool_calls_json, guardrail_verdict_json,
              error, created_at,
              started_at, finished_at, tool_calls, status, error_text
            ) values (
              $1,$2,$3,
              $4,$5,
              $6,$7,$8,
              $9,$10,
              $11,$12,
              $13,$14,$15,$16,$17,$18
            )
            on conflict (id) do nothing
            ",
        )
        .bind(run.id)
        .bind(run.review_id)
        .bind(run.draft_id)
        .bind(run.model_name)
        .bind(run.prompt_fingerprint)
        .bind(prompt_tokens)
        .bind(completion_tokens)
        .bind(latency_ms)
        .bind(run.tool_calls_json)
        .bind(run.guardrail_verdict_json)
        .bind(run.error.clone())
        .bind(run.created_at)
        // Back-compat with the older schema columns. We keep these populated so the table
        // remains self-consistent until we ship a full schema alignment.
        .bind(run.created_at)
        .bind(run.created_at)
        .bind(0_i32)
        .bind(if run.error.is_some() {
            "failed"
        } else {
            "succeeded"
        })
        .bind(run.error)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn list_agent_runs(&self, review_id: Uuid) -> RepositoryResult<Vec<domain::AgentRun>> {
        let rows: Vec<AgentRunRow> = sqlx::query_as(
            r"
            select
              id, review_id, draft_id,
              model_name, prompt_fingerprint,
              prompt_tokens, completion_tokens, latency_ms,
              tool_calls_json, guardrail_verdict_json,
              error, created_at
            from agent_runs
            where review_id = $1
            order by created_at desc
            ",
        )
        .bind(review_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        rows.into_iter().map(agent_run_from_row).collect()
    }

    async fn approve_draft(
        &self,
        _draft_id: Uuid,
        _reviewed_by: Uuid,
        _new_text: Option<String>,
    ) -> RepositoryResult<ReplyDraft> {
        let was_edited = _new_text.is_some();
        if let Some(text) = _new_text {
            let char_count = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
            let res = sqlx::query(
                r"
                update reply_drafts
                set text = $1,
                    char_count = $2,
                    state = 'edited',
                    generated_by = 'human_edit'
                where id = $3
                ",
            )
            .bind(text)
            .bind(char_count)
            .bind(_draft_id)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
            if res.rows_affected() == 0 {
                return Err(RepositoryError::NotFound);
            }
        }

        let now = OffsetDateTime::now_utc();
        let res = sqlx::query(
            r"
            update reply_drafts
            set state = 'approved',
                reviewed_by = $1,
                reviewed_at = $2,
                post_eligible_at = $3
            where id = $4 and state in ('pending_review','edited')
            ",
        )
        .bind(_reviewed_by)
        .bind(now)
        .bind(now)
        .bind(_draft_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::InvalidTransition);
        }

        let row: DraftRow = sqlx::query_as("select * from reply_drafts where id = $1")
            .bind(_draft_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        let draft = draft_from_row(row)?;
        self.append_audit(domain::AuditEvent::new(
            domain::ActorType::User,
            Some(_reviewed_by),
            "draft",
            _draft_id,
            if was_edited {
                domain::EventType::DraftEdited
            } else {
                domain::EventType::DraftApproved
            },
            serde_json::json!({ "review_id": draft.review_id }),
        ))
        .await?;
        Ok(draft)
    }

    async fn reject_draft(
        &self,
        _draft_id: Uuid,
        _reviewed_by: Uuid,
        _reason: String,
    ) -> RepositoryResult<ReplyDraft> {
        let now = OffsetDateTime::now_utc();
        let res = sqlx::query(
            r"
            update reply_drafts
            set state = 'rejected',
                reviewed_by = $1,
                reviewed_at = $2,
                rejection_reason = $3
            where id = $4 and state in ('pending_review','edited','approved')
            ",
        )
        .bind(_reviewed_by)
        .bind(now)
        .bind(_reason)
        .bind(_draft_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::InvalidTransition);
        }
        let row: DraftRow = sqlx::query_as("select * from reply_drafts where id = $1")
            .bind(_draft_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        let draft = draft_from_row(row)?;
        self.append_audit(domain::AuditEvent::new(
            domain::ActorType::User,
            Some(_reviewed_by),
            "draft",
            _draft_id,
            domain::EventType::DraftRejected,
            serde_json::json!({ "review_id": draft.review_id }),
        ))
        .await?;
        Ok(draft)
    }

    async fn bulk_approve(
        &self,
        _draft_ids: &[Uuid],
        _reviewed_by: Uuid,
    ) -> RepositoryResult<Vec<ReplyDraft>> {
        let now = OffsetDateTime::now_utc();
        let post_eligible_at = now + time::Duration::seconds(10);

        // Validate all ids are safe for bulk approve: 5-star + no warnings.
        for &id in _draft_ids {
            let row: DraftRow = sqlx::query_as("select * from reply_drafts where id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| RepositoryError::Storage(e.to_string()))?
                .ok_or(RepositoryError::NotFound)?;
            if !row
                .guardrail_warnings
                .as_array()
                .is_some_and(|a| a.is_empty())
            {
                return Err(RepositoryError::Conflict("draft_has_guardrail_warnings"));
            }

            let review_rating: Option<i16> =
                sqlx::query_scalar("select rating from reviews where id = $1")
                    .bind(row.review_id)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(|e| RepositoryError::Storage(e.to_string()))?;
            let Some(rating) = review_rating else {
                return Err(RepositoryError::NotFound);
            };
            if rating != 5 {
                return Err(RepositoryError::Conflict("bulk_approve_requires_5_star"));
            }
        }

        let mut out = Vec::with_capacity(_draft_ids.len());
        for &id in _draft_ids {
            let res = sqlx::query(
                r"
                update reply_drafts
                set state = 'approved_pending_undo',
                    reviewed_by = $1,
                    reviewed_at = $2,
                    post_eligible_at = $3
                where id = $4 and state in ('pending_review','edited')
                ",
            )
            .bind(_reviewed_by)
            .bind(now)
            .bind(post_eligible_at)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
            if res.rows_affected() == 0 {
                return Err(RepositoryError::InvalidTransition);
            }

            let row: DraftRow = sqlx::query_as("select * from reply_drafts where id = $1")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| RepositoryError::Storage(e.to_string()))?;
            let draft = draft_from_row(row)?;
            self.append_audit(domain::AuditEvent::new(
                domain::ActorType::User,
                Some(_reviewed_by),
                "draft",
                id,
                domain::EventType::DraftApproved,
                serde_json::json!({ "bulk": true, "review_id": draft.review_id }),
            ))
            .await?;
            out.push(draft);
        }

        Ok(out)
    }

    async fn undo_bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
        now: OffsetDateTime,
    ) -> RepositoryResult<Vec<ReplyDraft>> {
        let mut out = Vec::with_capacity(draft_ids.len());
        for &id in draft_ids {
            let row: DraftRow = sqlx::query_as("select * from reply_drafts where id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| RepositoryError::Storage(e.to_string()))?
                .ok_or(RepositoryError::NotFound)?;

            if row.reviewed_by != Some(reviewed_by) {
                return Err(RepositoryError::Conflict("undo_not_reviewer"));
            }
            let Some(eligible_at) = row.post_eligible_at else {
                return Err(RepositoryError::Conflict("undo_not_bulk_approved"));
            };
            if now >= eligible_at {
                return Err(RepositoryError::Conflict("undo_window_elapsed"));
            }

            let res = sqlx::query(
                r"
                update reply_drafts
                set state = 'pending_review',
                    reviewed_at = $1,
                    post_eligible_at = null
                where id = $2 and state = 'approved_pending_undo'
                ",
            )
            .bind(now)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;

            if res.rows_affected() == 0 {
                return Err(RepositoryError::InvalidTransition);
            }

            let row2: DraftRow = sqlx::query_as("select * from reply_drafts where id = $1")
                .bind(id)
                .fetch_one(&self.pool)
                .await
                .map_err(|e| RepositoryError::Storage(e.to_string()))?;
            out.push(draft_from_row(row2)?);
        }
        Ok(out)
    }

    async fn mark_draft_posted(
        &self,
        draft_id: Uuid,
        posted_at: OffsetDateTime,
    ) -> RepositoryResult<ReplyDraft> {
        let res = sqlx::query(
            r"
            update reply_drafts
            set state = 'posted',
                posted_at = $1,
                platform_post_error = null,
                post_eligible_at = null
            where id = $2
              and state in ('approved','approved_pending_undo')
              and (
                state <> 'approved_pending_undo'
                or (post_eligible_at is not null and $1 >= post_eligible_at)
              )
            ",
        )
        .bind(posted_at)
        .bind(draft_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::InvalidTransition);
        }
        let row: DraftRow = sqlx::query_as("select * from reply_drafts where id = $1")
            .bind(draft_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        let draft = draft_from_row(row)?;
        sqlx::query(
            r"
            update reviews
            set status = 'replied'
            where id = $1
              and status <> 'withdrawn'
              and status <> 'replied'
            ",
        )
        .bind(draft.review_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        self.append_audit(domain::AuditEvent::new(
            domain::ActorType::System,
            None,
            "draft",
            draft_id,
            domain::EventType::DraftPosted,
            serde_json::json!({ "review_id": draft.review_id, "posted_at": posted_at }),
        ))
        .await?;
        Ok(draft)
    }

    async fn mark_draft_post_failed(
        &self,
        draft_id: Uuid,
        error: String,
    ) -> RepositoryResult<ReplyDraft> {
        let now = OffsetDateTime::now_utc();
        let res = sqlx::query(
            r"
            update reply_drafts
            set state = 'failed',
                platform_post_error = $1,
                post_eligible_at = null
            where id = $2
              and state in ('approved','approved_pending_undo')
              and (
                state <> 'approved_pending_undo'
                or (post_eligible_at is not null and $3 >= post_eligible_at)
              )
            ",
        )
        .bind(error)
        .bind(draft_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        if res.rows_affected() == 0 {
            return Err(RepositoryError::InvalidTransition);
        }
        let row: DraftRow = sqlx::query_as("select * from reply_drafts where id = $1")
            .bind(draft_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        let draft = draft_from_row(row)?;
        self.append_audit(domain::AuditEvent::new(
            domain::ActorType::System,
            None,
            "draft",
            draft_id,
            domain::EventType::DraftPostFailed,
            serde_json::json!({ "review_id": draft.review_id }),
        ))
        .await?;
        Ok(draft)
    }

    async fn get_user_auth_by_email(
        &self,
        email: &str,
    ) -> RepositoryResult<Option<crate::repo::UserAuth>> {
        #[derive(Debug, Clone, sqlx::FromRow)]
        struct UserRow {
            id: Uuid,
            email: String,
            password_hash: String,
            role: String,
            totp_secret: Option<String>,
            created_at: OffsetDateTime,
        }

        let row: Option<UserRow> = sqlx::query_as(
            r"
            select id, email, password_hash, role, totp_secret, created_at
            from users
            where lower(email) = lower($1)
            ",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let role = match row.role.as_str() {
            "owner" => domain::UserRole::Owner,
            "manager" => domain::UserRole::Manager,
            "viewer" => domain::UserRole::Viewer,
            _ => return Err(RepositoryError::Storage("invalid user role".into())),
        };

        Ok(Some(crate::repo::UserAuth {
            user: domain::User {
                id: row.id,
                email: row.email,
                role,
                created_at: row.created_at,
            },
            password_hash: row.password_hash,
            totp_secret: row.totp_secret,
        }))
    }

    async fn get_user_by_id(&self, user_id: Uuid) -> RepositoryResult<Option<domain::User>> {
        #[derive(Debug, Clone, sqlx::FromRow)]
        struct UserRow {
            id: Uuid,
            email: String,
            role: String,
            created_at: OffsetDateTime,
        }

        let row: Option<UserRow> = sqlx::query_as(
            r"
            select id, email, role, created_at
            from users
            where id = $1
            ",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };
        let role = match row.role.as_str() {
            "owner" => domain::UserRole::Owner,
            "manager" => domain::UserRole::Manager,
            "viewer" => domain::UserRole::Viewer,
            _ => return Err(RepositoryError::Storage("invalid user role".into())),
        };
        Ok(Some(domain::User {
            id: row.id,
            email: row.email,
            role,
            created_at: row.created_at,
        }))
    }

    async fn list_users(&self) -> RepositoryResult<Vec<domain::User>> {
        #[derive(Debug, Clone, sqlx::FromRow)]
        struct UserRow {
            id: Uuid,
            email: String,
            role: String,
            created_at: OffsetDateTime,
        }

        let rows: Vec<UserRow> = sqlx::query_as(
            r"
            select id, email, role, created_at
            from users
            order by email asc
            ",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        rows.into_iter()
            .map(|row| {
                let role = match row.role.as_str() {
                    "owner" => domain::UserRole::Owner,
                    "manager" => domain::UserRole::Manager,
                    "viewer" => domain::UserRole::Viewer,
                    _ => return Err(RepositoryError::Storage("invalid user role".into())),
                };
                Ok(domain::User {
                    id: row.id,
                    email: row.email,
                    role,
                    created_at: row.created_at,
                })
            })
            .collect()
    }

    async fn get_restaurant_settings(&self) -> RepositoryResult<RestaurantSettings> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            r"
            select payload_json
            from restaurant_settings
            where id = $1
            ",
        )
        .bind(RESTAURANT_SETTINGS_ID)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        let Some((payload,)) = row else {
            return Ok(RestaurantSettings::default());
        };
        Ok(domain::RestaurantSettings::from_json_partial(&payload))
    }

    async fn put_restaurant_settings(&self, settings: RestaurantSettings) -> RepositoryResult<()> {
        let payload = serde_json::to_value(&settings)
            .map_err(|e| RepositoryError::Storage(format!("serialize restaurant settings: {e}")))?;
        sqlx::query(
            r"
            update restaurant_settings
            set payload_json = $1, updated_at = now()
            where id = $2
            ",
        )
        .bind(payload)
        .bind(RESTAURANT_SETTINGS_ID)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn create_session(&self, session: domain::Session) -> RepositoryResult<()> {
        sqlx::query(
            r"
            insert into sessions (id, user_id, created_at, expires_at)
            values ($1, $2, $3, $4)
            on conflict (id) do nothing
            ",
        )
        .bind(session.id)
        .bind(session.user_id)
        .bind(session.created_at)
        .bind(session.expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_session_user(
        &self,
        session_id: Uuid,
        now: OffsetDateTime,
    ) -> RepositoryResult<Option<(domain::Session, domain::User)>> {
        #[derive(Debug, Clone, sqlx::FromRow)]
        struct SessionUserRow {
            session_id: Uuid,
            session_user_id: Uuid,
            session_created_at: OffsetDateTime,
            session_expires_at: OffsetDateTime,
            user_email: String,
            user_role: String,
            user_created_at: OffsetDateTime,
        }

        let row: Option<SessionUserRow> = sqlx::query_as(
            r"
            select
              s.id as session_id,
              s.user_id as session_user_id,
              s.created_at as session_created_at,
              s.expires_at as session_expires_at,
              u.email as user_email,
              u.role as user_role,
              u.created_at as user_created_at
            from sessions s
            join users u on u.id = s.user_id
            where s.id = $1
              and s.expires_at > $2
            ",
        )
        .bind(session_id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let role = match row.user_role.as_str() {
            "owner" => domain::UserRole::Owner,
            "manager" => domain::UserRole::Manager,
            "viewer" => domain::UserRole::Viewer,
            _ => return Err(RepositoryError::Storage("invalid user role".into())),
        };

        Ok(Some((
            domain::Session {
                id: row.session_id,
                user_id: row.session_user_id,
                created_at: row.session_created_at,
                expires_at: row.session_expires_at,
            },
            domain::User {
                id: row.session_user_id,
                email: row.user_email,
                role,
                created_at: row.user_created_at,
            },
        )))
    }

    async fn touch_session(
        &self,
        session_id: Uuid,
        new_expires_at: OffsetDateTime,
    ) -> RepositoryResult<()> {
        sqlx::query("update sessions set expires_at = $1 where id = $2")
            .bind(new_expires_at)
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn delete_session(&self, session_id: Uuid) -> RepositoryResult<()> {
        sqlx::query("delete from sessions where id = $1")
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn enqueue_notification_outbox(
        &self,
        id: Uuid,
        occurred_at: OffsetDateTime,
        notification_type: domain::NotificationType,
        review_id: Option<Uuid>,
        draft_id: Option<Uuid>,
        payload_json: serde_json::Value,
    ) -> RepositoryResult<()> {
        sqlx::query(
            r"
            insert into notifications_outbox (
              id, occurred_at, notification_type, review_id, draft_id, payload_json, sent_at
            ) values ($1,$2,$3,$4,$5,$6,null)
            on conflict (id) do nothing
            ",
        )
        .bind(id)
        .bind(occurred_at)
        .bind(notification_type.to_string())
        .bind(review_id)
        .bind(draft_id)
        .bind(payload_json)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn claim_notification_outbox_batch(
        &self,
        limit: u32,
        claimed_by: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Vec<NotificationOutboxItem>> {
        #[derive(Debug, Clone, sqlx::FromRow)]
        struct OutboxRow {
            id: Uuid,
            occurred_at: OffsetDateTime,
            notification_type: String,
            review_id: Option<Uuid>,
            draft_id: Option<Uuid>,
            payload_json: serde_json::Value,
        }

        let rows: Vec<OutboxRow> = sqlx::query_as(
            r"
            with to_claim as (
              select id
              from notifications_outbox
              where sent_at is null
                and claimed_at is null
              order by occurred_at asc, id asc
              for update skip locked
              limit $1
            )
            update notifications_outbox n
            set claimed_at = $2,
                claimed_by = $3
            from to_claim
            where n.id = to_claim.id
            returning n.id, n.occurred_at, n.notification_type, n.review_id, n.draft_id, n.payload_json
            ",
        )
        .bind(i64::from(limit))
        .bind(now)
        .bind(claimed_by)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let nt = match r.notification_type.as_str() {
                "draft_ready" => domain::NotificationType::DraftReady,
                "sensitive_review" => domain::NotificationType::SensitiveReview,
                "sla_breach" => domain::NotificationType::SlaBreach,
                "sla_escalation" => domain::NotificationType::SlaEscalation,
                "ingestion_failure" => domain::NotificationType::IngestionFailure,
                "post_failed" => domain::NotificationType::PostFailed,
                "drift_detected" => domain::NotificationType::DriftDetected,
                _ => {
                    return Err(RepositoryError::Storage(
                        "invalid notification_type".to_string(),
                    ))
                }
            };
            out.push(NotificationOutboxItem {
                id: r.id,
                occurred_at: r.occurred_at,
                notification_type: nt,
                review_id: r.review_id,
                draft_id: r.draft_id,
                payload_json: r.payload_json,
            });
        }
        Ok(out)
    }

    async fn mark_notification_outbox_sent(
        &self,
        id: Uuid,
        sent_at: OffsetDateTime,
    ) -> RepositoryResult<()> {
        sqlx::query(
            "update notifications_outbox set sent_at = $1, claimed_at = null, claimed_by = null where id = $2",
        )
            .bind(sent_at)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn enqueue_work_job(
        &self,
        id: Uuid,
        job_type: WorkJobType,
        dedupe_key: &str,
        payload_json: serde_json::Value,
        run_after: OffsetDateTime,
        max_attempts: i32,
        now: OffsetDateTime,
    ) -> RepositoryResult<()> {
        sqlx::query(
            r"
            insert into work_jobs (
              id, job_type, dedupe_key, payload_json,
              state, attempts, max_attempts,
              run_after, locked_by, locked_at, last_error,
              created_at, updated_at
            ) values (
              $1,$2,$3,$4,
              'pending',0,$5,
              $6,null,null,null,
              $7,$7
            )
            on conflict (job_type, dedupe_key) do nothing
            ",
        )
        .bind(id)
        .bind(job_type.as_str())
        .bind(dedupe_key)
        .bind(payload_json)
        .bind(max_attempts)
        .bind(run_after)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn claim_work_jobs(
        &self,
        job_type: WorkJobType,
        limit: u32,
        locked_by: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Vec<WorkJob>> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        let rows: Vec<WorkJobRow> = sqlx::query_as(
            r"
            with to_claim as (
              select id
              from work_jobs
              where job_type = $1
                and state = 'pending'
                and run_after <= $2
              order by run_after asc, id asc
              for update skip locked
              limit $3
            )
            update work_jobs w
            set state = 'running',
                locked_by = $4,
                locked_at = $2,
                updated_at = $2
            from to_claim
            where w.id = to_claim.id
            returning
              w.id, w.job_type, w.dedupe_key, w.payload_json,
              w.state, w.attempts, w.max_attempts, w.run_after,
              w.locked_by, w.locked_at, w.last_error,
              w.created_at, w.updated_at
            ",
        )
        .bind(job_type.as_str())
        .bind(now)
        .bind(i64::from(limit))
        .bind(locked_by)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let jt = match r.job_type.as_str() {
                "agent_draft_review" => WorkJobType::AgentDraftReview,
                "poster_post_reply" => WorkJobType::PosterPostReply,
                "notifier_dispatch" => WorkJobType::NotifierDispatch,
                _ => return Err(RepositoryError::Storage("invalid job_type".into())),
            };
            let st = match r.state.as_str() {
                "pending" => WorkJobState::Pending,
                "running" => WorkJobState::Running,
                "succeeded" => WorkJobState::Succeeded,
                "failed" => WorkJobState::Failed,
                "dead_letter" => WorkJobState::DeadLetter,
                _ => return Err(RepositoryError::Storage("invalid job state".into())),
            };
            out.push(WorkJob {
                id: r.id,
                job_type: jt,
                dedupe_key: r.dedupe_key,
                payload_json: r.payload_json,
                state: st,
                attempts: r.attempts,
                max_attempts: r.max_attempts,
                run_after: r.run_after,
                locked_by: r.locked_by,
                locked_at: r.locked_at,
                last_error: r.last_error,
                created_at: r.created_at,
                updated_at: r.updated_at,
            });
        }
        Ok(out)
    }

    async fn mark_work_job_succeeded(&self, job_id: Uuid, now: OffsetDateTime) -> RepositoryResult<()> {
        let rows = sqlx::query(
            "update work_jobs set state = 'succeeded', updated_at = $1 where id = $2",
        )
        .bind(now)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?
        .rows_affected();
        if rows == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn mark_work_job_failed(
        &self,
        job_id: Uuid,
        error: &str,
        next_state: WorkJobState,
        run_after: OffsetDateTime,
        now: OffsetDateTime,
    ) -> RepositoryResult<()> {
        let rows = sqlx::query(
            r"
            update work_jobs
            set state = $1,
                attempts = attempts + 1,
                last_error = $2,
                run_after = $3,
                updated_at = $4
            where id = $5
            ",
        )
        .bind(next_state.as_str())
        .bind(error)
        .bind(run_after)
        .bind(now)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?
        .rows_affected();
        if rows == 0 {
            return Err(RepositoryError::NotFound);
        }
        Ok(())
    }

    async fn create_password_reset_token(
        &self,
        user_id: Uuid,
        token_hash: &str,
        expires_at: OffsetDateTime,
        now: OffsetDateTime,
    ) -> RepositoryResult<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            r#"
            insert into password_reset_tokens (id, user_id, token_hash, created_at, expires_at)
            values ($1, $2, $3, $4, $5)
            on conflict (token_hash) do nothing
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(token_hash)
        .bind(now)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(id)
    }

    async fn consume_password_reset_token(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Option<Uuid>> {
        let result: Option<Uuid> = sqlx::query_scalar(
            r#"
            update password_reset_tokens
            set used_at = $1
            where token_hash = $2
              and expires_at > $1
              and used_at is null
            returning user_id
            "#,
        )
        .bind(now)
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        Ok(result)
    }

    async fn update_user_password_hash(
        &self,
        user_id: Uuid,
        new_password_hash: &str,
    ) -> RepositoryResult<()> {
        let rows = sqlx::query("update users set password_hash = $1 where id = $2")
            .bind(new_password_hash)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?
            .rows_affected();
        if rows == 0 {
            return Err(RepositoryError::NotFound);
        }
        self.append_audit(domain::AuditEvent::new(
            domain::ActorType::User,
            Some(user_id),
            "user",
            user_id,
            domain::EventType::PasswordReset,
            serde_json::json!({}),
        ))
        .await?;
        Ok(())
    }

    async fn gc_delete_expired_reset_tokens(
        &self,
        cutoff: OffsetDateTime,
    ) -> RepositoryResult<u64> {
        let rows = sqlx::query(
            "delete from password_reset_tokens where expires_at < $1 or used_at is not null",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?
        .rows_affected();
        Ok(rows)
    }
}
