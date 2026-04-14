//! Ingestion service — deduplicates reviews and manages sync state.

use domain::{Platform, Review};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum IngestionError {
    #[error("review from {platform} with source id {source_id} failed validation: {reason}")]
    ValidationFailed {
        platform: Platform,
        source_id: String,
        reason: String,
    },

    #[error("storage error: {0}")]
    Storage(String),
}

/// Outcome of attempting to ingest a single review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Brand-new review, persisted for the first time.
    Created { review_id: Uuid },
    /// Review already existed but had updated content.
    Updated { review_id: Uuid },
    /// Review already existed with identical content; no action taken.
    Duplicate { review_id: Uuid },
}

/// Tracks the sync state for a platform adapter.
#[derive(Debug, Clone)]
pub struct SyncState {
    pub platform: Platform,
    pub last_seen_update_time: Option<OffsetDateTime>,
    pub last_run_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
}

impl SyncState {
    #[must_use]
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            last_seen_update_time: None,
            last_run_at: None,
            last_error: None,
        }
    }

    pub fn mark_success(&mut self, latest_update_time: OffsetDateTime) {
        self.last_seen_update_time = Some(latest_update_time);
        self.last_run_at = Some(OffsetDateTime::now_utc());
        self.last_error = None;
    }

    pub fn mark_failure(&mut self, error: String) {
        self.last_run_at = Some(OffsetDateTime::now_utc());
        self.last_error = Some(error);
    }
}

/// Determine the dedup key for a review: `(platform, source_review_id)`.
#[must_use]
pub fn dedup_key(review: &Review) -> (Platform, &str) {
    (review.platform, &review.source_review_id)
}

/// Compare two reviews to determine if the incoming one has updated content.
#[must_use]
pub fn has_content_changed(existing: &Review, incoming: &Review) -> bool {
    existing.body_text != incoming.body_text
        || existing.rating != incoming.rating
        || existing.updated_at < incoming.updated_at
}

/// In-memory dedup index for testing and single-process deployments.
#[derive(Debug, Default)]
pub struct InMemoryDedupIndex {
    seen: std::collections::HashMap<(Platform, String), Review>,
}

impl InMemoryDedupIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attempt to ingest a review. Returns the outcome.
    pub fn ingest(&mut self, review: Review) -> Result<IngestOutcome, IngestionError> {
        domain::validation::validate_review_fields(
            &review.source_review_id,
            &review.source_location_id,
            &review.author.display_name,
            review.rating,
        )
        .map_err(|e| IngestionError::ValidationFailed {
            platform: review.platform,
            source_id: review.source_review_id.clone(),
            reason: e.to_string(),
        })?;

        let key = (review.platform, review.source_review_id.clone());

        if let Some(existing) = self.seen.get(&key) {
            if has_content_changed(existing, &review) {
                let id = existing.id;
                self.seen.insert(key, review);
                Ok(IngestOutcome::Updated { review_id: id })
            } else {
                Ok(IngestOutcome::Duplicate {
                    review_id: existing.id,
                })
            }
        } else {
            let id = review.id;
            self.seen.insert(key, review);
            Ok(IngestOutcome::Created { review_id: id })
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Platform, ReviewAuthor, ReviewStatus};
    use serde_json::json;
    use time::macros::datetime;

    fn make_review(source_id: &str, rating: u8) -> Review {
        Review {
            id: Uuid::new_v4(),
            platform: Platform::Google,
            source_review_id: source_id.into(),
            source_location_id: "loc-1".into(),
            author: ReviewAuthor {
                display_name: "Maria".into(),
                avatar_url: None,
            },
            rating,
            body_text: Some("Good".into()),
            body_language: Some("en".into()),
            created_at: datetime!(2026-04-10 12:00:00 UTC),
            updated_at: datetime!(2026-04-10 12:00:00 UTC),
            ingested_at: datetime!(2026-04-10 12:01:00 UTC),
            existing_reply_text: None,
            existing_reply_updated_at: None,
            status: ReviewStatus::New,
            context_json: json!({}),
            raw_payload: json!({}),
        }
    }

    #[test]
    fn first_ingest_creates() {
        let mut index = InMemoryDedupIndex::new();
        let result = index.ingest(make_review("r1", 5)).unwrap();
        assert!(matches!(result, IngestOutcome::Created { .. }));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn duplicate_ingest_is_noop() {
        let mut index = InMemoryDedupIndex::new();
        let review = make_review("r1", 5);
        index.ingest(review.clone()).unwrap();
        let result = index.ingest(review).unwrap();
        assert!(matches!(result, IngestOutcome::Duplicate { .. }));
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn updated_review_detected() {
        let mut index = InMemoryDedupIndex::new();
        let review1 = make_review("r1", 5);
        index.ingest(review1).unwrap();

        let mut review2 = make_review("r1", 4);
        review2.updated_at = datetime!(2026-04-11 12:00:00 UTC);
        let result = index.ingest(review2).unwrap();
        assert!(matches!(result, IngestOutcome::Updated { .. }));
    }

    #[test]
    fn different_platforms_not_duplicates() {
        let mut index = InMemoryDedupIndex::new();
        let r1 = make_review("r1", 5);
        let mut r2 = make_review("r1", 5);
        r2.platform = Platform::Ubereats;
        r2.id = Uuid::new_v4();

        index.ingest(r1).unwrap();
        let result = index.ingest(r2).unwrap();
        assert!(matches!(result, IngestOutcome::Created { .. }));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn validation_failure_on_bad_rating() {
        let mut index = InMemoryDedupIndex::new();
        let mut review = make_review("r1", 5);
        review.rating = 0;
        let err = index.ingest(review).unwrap_err();
        assert!(matches!(err, IngestionError::ValidationFailed { .. }));
    }

    #[test]
    fn sync_state_success() {
        let mut state = SyncState::new(Platform::Google);
        assert!(state.last_seen_update_time.is_none());
        state.mark_success(datetime!(2026-04-10 12:00:00 UTC));
        assert!(state.last_seen_update_time.is_some());
        assert!(state.last_error.is_none());
    }

    #[test]
    fn sync_state_failure() {
        let mut state = SyncState::new(Platform::Google);
        state.mark_failure("timeout".into());
        assert!(state.last_error.is_some());
        assert!(state.last_run_at.is_some());
    }

    #[test]
    fn has_content_changed_detects_body_change() {
        let r1 = make_review("r1", 5);
        let mut r2 = make_review("r1", 5);
        r2.body_text = Some("Updated review".into());
        assert!(has_content_changed(&r1, &r2));
    }

    #[test]
    fn has_content_changed_same_content() {
        let r1 = make_review("r1", 5);
        let r2 = make_review("r1", 5);
        assert!(!has_content_changed(&r1, &r2));
    }
}
