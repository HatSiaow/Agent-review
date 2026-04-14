use domain::{ReplyDraft, Review};
use sqlx::{PgPool, Pool, Postgres};
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
        let pool = Pool::<Postgres>::builder()
            .max_connections(config.max_connections)
            .build(&config.database_url)
            .await?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl Repository for PgRepository {
    async fn list_reviews(&self) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>> {
        let _ = &self.pool;
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn get_review(&self, _id: Uuid) -> RepositoryResult<(Review, Option<ReplyDraft>)> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn ingest_review(&self, _review: Review) -> RepositoryResult<()> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn upsert_review_with_draft(
        &self,
        _review: Review,
        _draft: ReplyDraft,
    ) -> RepositoryResult<()> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn transition_review_to_drafting(&self, _review_id: Uuid) -> RepositoryResult<Review> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn skip_review(&self, _review_id: Uuid) -> RepositoryResult<Review> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn unskip_review(&self, _review_id: Uuid) -> RepositoryResult<Review> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn list_drafts(&self) -> RepositoryResult<Vec<ReplyDraft>> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn store_agent_draft(&self, _draft: ReplyDraft) -> RepositoryResult<()> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn approve_draft(
        &self,
        _draft_id: Uuid,
        _reviewed_by: Uuid,
        _new_text: Option<String>,
    ) -> RepositoryResult<ReplyDraft> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn reject_draft(
        &self,
        _draft_id: Uuid,
        _reviewed_by: Uuid,
        _reason: String,
    ) -> RepositoryResult<ReplyDraft> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }

    async fn bulk_approve(
        &self,
        _draft_ids: &[Uuid],
        _reviewed_by: Uuid,
    ) -> RepositoryResult<Vec<ReplyDraft>> {
        Err(RepositoryError::Storage(
            "PgRepository not wired yet".to_string(),
        ))
    }
}

