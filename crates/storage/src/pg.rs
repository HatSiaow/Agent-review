use domain::{DraftState, ReplyDraft, Review};
use sqlx::PgPool;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::repo::{Repository, RepositoryError, RepositoryResult};

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

    pub fn migrate(&self) -> Result<(), sqlx::Error> {
        // NOTE: Intentionally minimal; we use raw SQL migrations in this crate.
        // In a follow-up we can switch to an embedded migrator.
        //
        // For now, callers can run the initial migration file manually in dev,
        // and tests can create schema per connection.
        let _ = &self.pool;
        Ok(())
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
        _ => Err(RepositoryError::Storage("invalid review status".to_string())),
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
        rating: u8::try_from(row.rating).map_err(|_| RepositoryError::Storage("bad rating".into()))?,
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

#[async_trait::async_trait]
impl Repository for PgRepository {
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

        let res = sqlx::query(
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
            on conflict (platform, source_review_id) do nothing
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
        .execute(&self.pool)
        .await
        .map_err(|e| RepositoryError::Storage(e.to_string()))?;

        if res.rows_affected() == 0 {
            return Ok(());
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
        Ok(review)
    }

    async fn unskip_review(&self, _review_id: Uuid) -> RepositoryResult<Review> {
        let res = sqlx::query(r"update reviews set status = 'new' where id = $1 and status = 'skipped'")
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

    async fn list_drafts(&self) -> RepositoryResult<Vec<ReplyDraft>> {
        let rows: Vec<DraftRow> = sqlx::query_as("select * from reply_drafts order by created_at desc")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| RepositoryError::Storage(e.to_string()))?;
        rows.into_iter().map(draft_from_row).collect()
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
        .bind(serde_json::to_value(_draft.guardrail_warnings).unwrap_or_else(|_| serde_json::json!([])))
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

        Ok(())
    }

    async fn approve_draft(
        &self,
        _draft_id: Uuid,
        _reviewed_by: Uuid,
        _new_text: Option<String>,
    ) -> RepositoryResult<ReplyDraft> {
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
        draft_from_row(row)
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
        draft_from_row(row)
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
            if !row.guardrail_warnings.as_array().is_some_and(|a| a.is_empty()) {
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
            out.push(draft_from_row(row)?);
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
            where id = $2 and state in ('approved','approved_pending_undo')
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
        draft_from_row(row)
    }

    async fn mark_draft_post_failed(
        &self,
        draft_id: Uuid,
        error: String,
    ) -> RepositoryResult<ReplyDraft> {
        let res = sqlx::query(
            r"
            update reply_drafts
            set state = 'failed',
                platform_post_error = $1,
                post_eligible_at = null
            where id = $2 and state in ('approved','approved_pending_undo')
            ",
        )
        .bind(error)
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
        draft_from_row(row)
    }
}

