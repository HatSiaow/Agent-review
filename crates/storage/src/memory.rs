use std::collections::HashMap;
use std::sync::Arc;

use domain::{
    ActorType, AuditEvent, DraftEvent, DraftFsm, EventType, Generator, ReplyDraft, Review,
    ReviewEvent, ReviewFsm, ReviewStatus,
};
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::repo::{
    DraftListQuery, NotificationOutboxItem, Repository, RepositoryError, RepositoryResult,
    ReviewListQuery, WorkJob, WorkJobState, WorkJobType,
};

#[derive(Debug, Clone)]
pub struct InMemoryRepository(Arc<Mutex<State>>);

#[derive(Debug, Default)]
struct State {
    reviews: HashMap<Uuid, Review>,
    drafts: HashMap<Uuid, ReplyDraft>,
    review_to_active_draft: HashMap<Uuid, Uuid>,
    reviews_sync_state: HashMap<domain::Platform, time::OffsetDateTime>,
    webhook_events: HashMap<(domain::Platform, String), time::OffsetDateTime>,
    audit_events: Vec<AuditEvent>,
    agent_runs: HashMap<Uuid, Vec<domain::AgentRun>>,
    idempotency_responses: HashMap<String, (u16, serde_json::Value, time::OffsetDateTime)>,
    users: HashMap<Uuid, domain::User>,
    users_auth: HashMap<Uuid, (String, Option<String>)>,
    users_by_email: HashMap<String, Uuid>,
    sessions: HashMap<Uuid, domain::Session>,
    notification_outbox: HashMap<Uuid, NotificationOutboxItem>,
    notification_outbox_sent_at: HashMap<Uuid, OffsetDateTime>,
    notification_outbox_claimed: HashMap<Uuid, (String, OffsetDateTime)>,
    restaurant_settings: domain::RestaurantSettings,

    work_jobs: HashMap<Uuid, WorkJob>,
    work_jobs_by_type_dedupe: HashMap<(WorkJobType, String), Uuid>,

    /// token_hash -> (id, user_id, created_at, expires_at, used_at)
    password_reset_tokens: HashMap<String, (Uuid, Uuid, OffsetDateTime, OffsetDateTime, Option<OffsetDateTime>)>,
}

impl InMemoryRepository {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for InMemoryRepository {
    fn default() -> Self {
        // Seed a single owner user for dev/tests (single-restaurant scope).
        //
        // This keeps the API usable without requiring a provisioning flow in the
        // in-memory repository. Postgres deployments should provision users explicitly.
        let seed_user_id = Uuid::from_u128(1);
        let email = "owner@example.com".to_string();
        let created_at = OffsetDateTime::now_utc();
        let user = domain::User {
            id: seed_user_id,
            email: email.clone(),
            role: domain::UserRole::Owner,
            created_at,
        };

        let password_hash = {
            use argon2::password_hash::{PasswordHasher as _, SaltString};
            use argon2::{Algorithm, Argon2, Params, Version};

            let salt = SaltString::encode_b64(b"agent-review-seed-salt")
                .expect("seed salt must be encodable");
            let params = Params::new(65_536, 3, 1, None).expect("valid argon2 params");
            let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
            argon2
                .hash_password("password".as_bytes(), &salt)
                .expect("hash seed password")
                .to_string()
        };

        let mut state = State::default();
        state
            .users_by_email
            .insert(email.to_ascii_lowercase(), seed_user_id);
        state.users.insert(seed_user_id, user);
        state.users_auth.insert(seed_user_id, (password_hash, None));
        state.restaurant_settings = domain::RestaurantSettings::default();

        Self(Arc::new(Mutex::new(state)))
    }
}

#[async_trait::async_trait]
impl Repository for InMemoryRepository {
    async fn ping(&self) -> RepositoryResult<()> {
        Ok(())
    }

    async fn list_reviews(&self) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>> {
        let state = self.0.lock().await;
        let mut rows: Vec<(Review, Option<ReplyDraft>)> = state
            .reviews
            .values()
            .cloned()
            .map(|r| {
                let active = state
                    .review_to_active_draft
                    .get(&r.id)
                    .and_then(|id| state.drafts.get(id))
                    .cloned();
                (r, active)
            })
            .collect();
        rows.sort_by(|a, b| b.0.updated_at.cmp(&a.0.updated_at));
        Ok(rows)
    }

    async fn list_reviews_filtered(
        &self,
        query: ReviewListQuery,
    ) -> RepositoryResult<Vec<(Review, Option<ReplyDraft>)>> {
        let rows = self.list_reviews().await?;
        Ok(crate::list_filters::filter_sort_reviews(rows, &query))
    }

    async fn get_review(&self, id: Uuid) -> RepositoryResult<(Review, Option<ReplyDraft>)> {
        let state = self.0.lock().await;
        let review = state
            .reviews
            .get(&id)
            .cloned()
            .ok_or(RepositoryError::NotFound)?;
        let active = state
            .review_to_active_draft
            .get(&id)
            .and_then(|draft_id| state.drafts.get(draft_id))
            .cloned();
        Ok((review, active))
    }

    async fn ingest_review(&self, review: Review) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        let existing_id = state.reviews.iter().find_map(|(id, r)| {
            (r.platform == review.platform && r.source_review_id == review.source_review_id)
                .then_some(*id)
        });

        if let Some(existing_id) = existing_id {
            let Some(existing) = state.reviews.get_mut(&existing_id) else {
                return Ok(());
            };

            // Storage-level upsert semantics:
            // - always preserve the canonical UUID primary key
            // - update when the incoming review is newer or materially changed
            let newer = review.updated_at > existing.updated_at;
            let changed = existing.body_text != review.body_text
                || existing.rating != review.rating
                || existing.existing_reply_text != review.existing_reply_text
                || existing.existing_reply_updated_at != review.existing_reply_updated_at;

            if newer || changed {
                let drift = existing.body_text != review.body_text;
                let mut incoming = review;
                incoming.id = existing_id;
                *existing = incoming;

                state.audit_events.push(AuditEvent::new(
                    ActorType::System,
                    None,
                    "review",
                    existing_id,
                    if drift {
                        EventType::DriftDetected
                    } else {
                        EventType::ReviewIngested
                    },
                    serde_json::json!({ "updated": true }),
                ));
            }
            return Ok(());
        }

        let review_id = review.id;
        state.reviews.insert(review_id, review);
        state.audit_events.push(AuditEvent::new(
            ActorType::System,
            None,
            "review",
            review_id,
            EventType::ReviewIngested,
            serde_json::json!({ "created": true }),
        ));
        Ok(())
    }

    async fn upsert_review_with_draft(
        &self,
        review: Review,
        draft: ReplyDraft,
    ) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        state.review_to_active_draft.insert(review.id, draft.id);
        state.reviews.insert(review.id, review);
        state.drafts.insert(draft.id, draft);
        Ok(())
    }

    async fn get_reviews_sync_state(
        &self,
        platform: domain::Platform,
    ) -> RepositoryResult<Option<time::OffsetDateTime>> {
        let state = self.0.lock().await;
        Ok(state.reviews_sync_state.get(&platform).copied())
    }

    async fn set_reviews_sync_state(
        &self,
        platform: domain::Platform,
        last_seen_update_time: time::OffsetDateTime,
    ) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        state
            .reviews_sync_state
            .insert(platform, last_seen_update_time);
        Ok(())
    }

    async fn register_webhook_event(
        &self,
        platform: domain::Platform,
        event_id: &str,
        received_at: time::OffsetDateTime,
    ) -> RepositoryResult<bool> {
        let mut state = self.0.lock().await;

        // Simple TTL cleanup (24h) to keep memory bounded in tests/dev.
        let cutoff = received_at - time::Duration::hours(24);
        state.webhook_events.retain(|_, t| *t >= cutoff);

        let key = (platform, event_id.to_string());
        if state.webhook_events.contains_key(&key) {
            return Ok(false);
        }
        state.webhook_events.insert(key, received_at);
        Ok(true)
    }

    async fn list_audit_events(
        &self,
        entity_type: &str,
        entity_id: Uuid,
    ) -> RepositoryResult<Vec<domain::AuditEvent>> {
        let state = self.0.lock().await;
        Ok(state
            .audit_events
            .iter()
            .filter(|e| e.entity_type == entity_type && e.entity_id == entity_id)
            .cloned()
            .collect())
    }

    async fn get_idempotency_response(
        &self,
        idempotency_key: &str,
    ) -> RepositoryResult<Option<(u16, serde_json::Value)>> {
        let state = self.0.lock().await;
        Ok(state
            .idempotency_responses
            .get(idempotency_key)
            .map(|(status, body, _)| (*status, body.clone())))
    }

    async fn put_idempotency_response(
        &self,
        idempotency_key: &str,
        status: u16,
        body_json: serde_json::Value,
        created_at: time::OffsetDateTime,
    ) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        state
            .idempotency_responses
            .entry(idempotency_key.to_string())
            .or_insert((status, body_json, created_at));
        Ok(())
    }

    async fn transition_review_to_drafting(&self, review_id: Uuid) -> RepositoryResult<Review> {
        let mut state = self.0.lock().await;
        let review = state
            .reviews
            .get_mut(&review_id)
            .ok_or(RepositoryError::NotFound)?;

        let fsm = ReviewFsm::new(review.status)
            .apply(ReviewEvent::StartDrafting)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        review.status = fsm.state();
        let result = review.clone();

        state.review_to_active_draft.remove(&review_id);
        Ok(result)
    }

    async fn skip_review(&self, review_id: Uuid) -> RepositoryResult<Review> {
        let mut state = self.0.lock().await;
        let review = state
            .reviews
            .get_mut(&review_id)
            .ok_or(RepositoryError::NotFound)?;
        let fsm = ReviewFsm::new(review.status)
            .apply(ReviewEvent::Skip)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        review.status = fsm.state();
        let out = review.clone();
        state.audit_events.push(AuditEvent::new(
            ActorType::System,
            None,
            "review",
            review_id,
            EventType::ReviewSkipped,
            serde_json::json!({}),
        ));
        Ok(out)
    }

    async fn unskip_review(&self, review_id: Uuid) -> RepositoryResult<Review> {
        let mut state = self.0.lock().await;
        let review = state
            .reviews
            .get_mut(&review_id)
            .ok_or(RepositoryError::NotFound)?;
        let fsm = ReviewFsm::new(review.status)
            .apply(ReviewEvent::Unskip)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        review.status = fsm.state();
        let out = review.clone();
        state.audit_events.push(AuditEvent::new(
            ActorType::System,
            None,
            "review",
            review_id,
            EventType::ReviewUnskipped,
            serde_json::json!({}),
        ));
        Ok(out)
    }

    async fn list_drafts(&self) -> RepositoryResult<Vec<ReplyDraft>> {
        let state = self.0.lock().await;
        let mut out: Vec<ReplyDraft> = state.drafts.values().cloned().collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    async fn list_drafts_filtered(
        &self,
        query: DraftListQuery,
    ) -> RepositoryResult<Vec<ReplyDraft>> {
        let drafts = self.list_drafts().await?;
        let state = self.0.lock().await;
        let pairs: Vec<(ReplyDraft, Review)> = drafts
            .into_iter()
            .filter_map(|d| state.reviews.get(&d.review_id).cloned().map(|r| (d, r)))
            .collect();
        drop(state);
        Ok(crate::list_filters::filter_sort_drafts(pairs, &query))
    }

    async fn store_agent_draft(&self, draft: ReplyDraft) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        let review_id = draft.review_id;
        let draft_id = draft.id;
        state.review_to_active_draft.insert(review_id, draft.id);
        state.drafts.insert(draft.id, draft);
        if let Some(review) = state.reviews.get_mut(&review_id) {
            review.status = ReviewStatus::AwaitingHuman;
        }
        state.audit_events.push(AuditEvent::new(
            ActorType::Agent,
            None,
            "draft",
            draft_id,
            EventType::DraftCreated,
            serde_json::json!({ "review_id": review_id }),
        ));
        Ok(())
    }

    async fn store_agent_run(&self, run: domain::AgentRun) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        state.agent_runs.entry(run.review_id).or_default().push(run);
        Ok(())
    }

    async fn list_agent_runs(&self, review_id: Uuid) -> RepositoryResult<Vec<domain::AgentRun>> {
        let state = self.0.lock().await;
        let mut out = state
            .agent_runs
            .get(&review_id)
            .cloned()
            .unwrap_or_default();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    async fn approve_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        new_text: Option<String>,
    ) -> RepositoryResult<ReplyDraft> {
        let mut state = self.0.lock().await;
        let was_edited = new_text.is_some();
        let (review_id, updated) = {
            let draft = state
                .drafts
                .get_mut(&draft_id)
                .ok_or(RepositoryError::NotFound)?;
            let review_id = draft.review_id;

            let mut fsm = DraftFsm::new(draft.state);
            if let Some(text) = new_text {
                fsm = fsm
                    .apply(DraftEvent::Edit)
                    .map_err(|_| RepositoryError::InvalidTransition)?;
                draft.text = text;
                draft.char_count = u32::try_from(draft.text.chars().count()).unwrap_or(u32::MAX);
                draft.state = fsm.state();
                draft.generated_by = Generator::HumanEdit;
            }

            fsm = fsm
                .apply(DraftEvent::Approve)
                .map_err(|_| RepositoryError::InvalidTransition)?;
            draft.state = fsm.state();
            draft.reviewed_by = Some(reviewed_by);
            draft.reviewed_at = Some(OffsetDateTime::now_utc());
            draft.post_eligible_at = Some(OffsetDateTime::now_utc());

            (review_id, draft.clone())
        };

        if let Some(review) = state.reviews.get_mut(&review_id) {
            review.status = ReviewStatus::AwaitingHuman;
        }

        state.audit_events.push(AuditEvent::new(
            ActorType::User,
            Some(reviewed_by),
            "draft",
            draft_id,
            if was_edited {
                EventType::DraftEdited
            } else {
                EventType::DraftApproved
            },
            serde_json::json!({ "review_id": review_id }),
        ));

        Ok(updated)
    }

    async fn reject_draft(
        &self,
        draft_id: Uuid,
        reviewed_by: Uuid,
        reason: String,
    ) -> RepositoryResult<ReplyDraft> {
        let mut state = self.0.lock().await;
        let draft = state
            .drafts
            .get_mut(&draft_id)
            .ok_or(RepositoryError::NotFound)?;

        let fsm = DraftFsm::new(draft.state)
            .apply(DraftEvent::Reject)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        draft.state = fsm.state();
        draft.reviewed_by = Some(reviewed_by);
        draft.reviewed_at = Some(OffsetDateTime::now_utc());
        draft.rejection_reason = Some(reason);
        let out = draft.clone();
        state.audit_events.push(AuditEvent::new(
            ActorType::User,
            Some(reviewed_by),
            "draft",
            draft_id,
            EventType::DraftRejected,
            serde_json::json!({}),
        ));
        Ok(out)
    }

    async fn bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
    ) -> RepositoryResult<Vec<ReplyDraft>> {
        let mut state = self.0.lock().await;
        let mut results = Vec::with_capacity(draft_ids.len());
        let now = OffsetDateTime::now_utc();
        let post_eligible_at = now + time::Duration::seconds(10);

        for &draft_id in draft_ids {
            let draft = state
                .drafts
                .get(&draft_id)
                .ok_or(RepositoryError::NotFound)?;

            if !draft.guardrail_warnings.is_empty() {
                return Err(RepositoryError::Conflict("draft_has_guardrail_warnings"));
            }

            let review = state
                .reviews
                .get(&draft.review_id)
                .ok_or(RepositoryError::NotFound)?;
            if review.rating != 5 {
                return Err(RepositoryError::Conflict("bulk_approve_requires_5_star"));
            }

            let fsm = DraftFsm::new(draft.state)
                .apply(DraftEvent::BulkApprove)
                .map_err(|_| RepositoryError::InvalidTransition)?;

            let draft = state
                .drafts
                .get_mut(&draft_id)
                .ok_or(RepositoryError::NotFound)?;
            draft.state = fsm.state();
            draft.reviewed_by = Some(reviewed_by);
            draft.reviewed_at = Some(now);
            draft.post_eligible_at = Some(post_eligible_at);
            results.push(draft.clone());

            state.audit_events.push(AuditEvent::new(
                ActorType::User,
                Some(reviewed_by),
                "draft",
                draft_id,
                EventType::DraftApproved,
                serde_json::json!({ "bulk": true }),
            ));
        }

        Ok(results)
    }

    async fn undo_bulk_approve(
        &self,
        draft_ids: &[Uuid],
        reviewed_by: Uuid,
        now: OffsetDateTime,
    ) -> RepositoryResult<Vec<ReplyDraft>> {
        let mut state = self.0.lock().await;
        let mut results = Vec::with_capacity(draft_ids.len());

        for &draft_id in draft_ids {
            let draft = state
                .drafts
                .get_mut(&draft_id)
                .ok_or(RepositoryError::NotFound)?;

            if draft.reviewed_by != Some(reviewed_by) {
                return Err(RepositoryError::Conflict("undo_not_reviewer"));
            }

            let Some(eligible_at) = draft.post_eligible_at else {
                return Err(RepositoryError::Conflict("undo_not_bulk_approved"));
            };
            if now >= eligible_at {
                return Err(RepositoryError::Conflict("undo_window_elapsed"));
            }

            let fsm = DraftFsm::new(draft.state)
                .apply(DraftEvent::UndoBulkApprove)
                .map_err(|_| RepositoryError::InvalidTransition)?;
            draft.state = fsm.state();
            draft.post_eligible_at = None;
            draft.reviewed_at = Some(now);
            results.push(draft.clone());
        }

        Ok(results)
    }

    async fn mark_draft_posted(
        &self,
        draft_id: Uuid,
        posted_at: OffsetDateTime,
    ) -> RepositoryResult<ReplyDraft> {
        let mut state = self.0.lock().await;
        let draft = state
            .drafts
            .get_mut(&draft_id)
            .ok_or(RepositoryError::NotFound)?;
        if draft.state == domain::DraftState::ApprovedPendingUndo {
            if let Some(t) = draft.post_eligible_at {
                if posted_at < t {
                    return Err(RepositoryError::InvalidTransition);
                }
            } else {
                return Err(RepositoryError::InvalidTransition);
            }
        }
        let fsm = DraftFsm::new(draft.state)
            .apply(DraftEvent::MarkPosted)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        let review_id = draft.review_id;
        draft.state = fsm.state();
        draft.posted_at = Some(posted_at);
        draft.post_eligible_at = None;
        let out = draft.clone();
        if let Some(review) = state.reviews.get_mut(&review_id) {
            review.status = ReviewStatus::Replied;
        }
        state.audit_events.push(AuditEvent::new(
            ActorType::System,
            None,
            "draft",
            draft_id,
            EventType::DraftPosted,
            serde_json::json!({ "posted_at": posted_at }),
        ));
        Ok(out)
    }

    async fn mark_draft_post_failed(
        &self,
        draft_id: Uuid,
        error: String,
    ) -> RepositoryResult<ReplyDraft> {
        let mut state = self.0.lock().await;
        let draft = state
            .drafts
            .get_mut(&draft_id)
            .ok_or(RepositoryError::NotFound)?;
        if draft.state == domain::DraftState::ApprovedPendingUndo {
            let now = OffsetDateTime::now_utc();
            if let Some(t) = draft.post_eligible_at {
                if now < t {
                    return Err(RepositoryError::InvalidTransition);
                }
            } else {
                return Err(RepositoryError::InvalidTransition);
            }
        }
        let fsm = DraftFsm::new(draft.state)
            .apply(DraftEvent::MarkFailed)
            .map_err(|_| RepositoryError::InvalidTransition)?;
        draft.state = fsm.state();
        draft.platform_post_error = Some(error);
        draft.post_eligible_at = None;
        let out = draft.clone();
        state.audit_events.push(AuditEvent::new(
            ActorType::System,
            None,
            "draft",
            draft_id,
            EventType::DraftPostFailed,
            serde_json::json!({}),
        ));
        Ok(out)
    }

    async fn get_user_auth_by_email(
        &self,
        email: &str,
    ) -> RepositoryResult<Option<crate::repo::UserAuth>> {
        let state = self.0.lock().await;
        let key = email.trim().to_ascii_lowercase();
        let Some(id) = state.users_by_email.get(&key).copied() else {
            return Ok(None);
        };
        let Some(user) = state.users.get(&id).cloned() else {
            return Ok(None);
        };
        let (password_hash, totp_secret) = state
            .users_auth
            .get(&id)
            .cloned()
            .unwrap_or_else(|| (String::new(), None));
        Ok(Some(crate::repo::UserAuth {
            user,
            password_hash,
            totp_secret,
        }))
    }

    async fn get_user_by_id(&self, user_id: Uuid) -> RepositoryResult<Option<domain::User>> {
        let state = self.0.lock().await;
        Ok(state.users.get(&user_id).cloned())
    }

    async fn list_users(&self) -> RepositoryResult<Vec<domain::User>> {
        let state = self.0.lock().await;
        let mut out: Vec<domain::User> = state.users.values().cloned().collect();
        out.sort_by(|a, b| a.email.cmp(&b.email));
        Ok(out)
    }

    async fn get_restaurant_settings(&self) -> RepositoryResult<domain::RestaurantSettings> {
        let state = self.0.lock().await;
        Ok(state.restaurant_settings.clone())
    }

    async fn put_restaurant_settings(
        &self,
        settings: domain::RestaurantSettings,
    ) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        state.restaurant_settings = settings;
        Ok(())
    }

    async fn create_session(&self, session: domain::Session) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        state.sessions.insert(session.id, session);
        Ok(())
    }

    async fn get_session_user(
        &self,
        session_id: Uuid,
        now: OffsetDateTime,
    ) -> RepositoryResult<Option<(domain::Session, domain::User)>> {
        let state = self.0.lock().await;
        let Some(session) = state.sessions.get(&session_id).cloned() else {
            return Ok(None);
        };
        if now >= session.expires_at {
            return Ok(None);
        }
        let Some(user) = state.users.get(&session.user_id).cloned() else {
            return Ok(None);
        };
        Ok(Some((session, user)))
    }

    async fn touch_session(
        &self,
        session_id: Uuid,
        new_expires_at: OffsetDateTime,
    ) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        let Some(s) = state.sessions.get_mut(&session_id) else {
            return Ok(());
        };
        s.expires_at = new_expires_at;
        Ok(())
    }

    async fn delete_session(&self, session_id: Uuid) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        state.sessions.remove(&session_id);
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
        let mut state = self.0.lock().await;
        state
            .notification_outbox
            .entry(id)
            .or_insert(NotificationOutboxItem {
                id,
                occurred_at,
                notification_type,
                review_id,
                draft_id,
                payload_json,
            });
        Ok(())
    }

    async fn claim_notification_outbox_batch(
        &self,
        limit: u32,
        claimed_by: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Vec<NotificationOutboxItem>> {
        let mut state = self.0.lock().await;
        let mut items: Vec<NotificationOutboxItem> = state
            .notification_outbox
            .values()
            .filter(|i| !state.notification_outbox_sent_at.contains_key(&i.id))
            .filter(|i| !state.notification_outbox_claimed.contains_key(&i.id))
            .cloned()
            .collect();
        items.sort_by_key(|i| (i.occurred_at, i.id));
        items.truncate(limit as usize);
        for i in &items {
            state.notification_outbox_claimed.insert(
                i.id,
                (claimed_by.to_string(), now),
            );
        }
        Ok(items)
    }

    async fn mark_notification_outbox_sent(
        &self,
        id: Uuid,
        sent_at: OffsetDateTime,
    ) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        if state.notification_outbox.contains_key(&id) {
            state.notification_outbox_sent_at.insert(id, sent_at);
            state.notification_outbox_claimed.remove(&id);
        }
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
        let mut state = self.0.lock().await;
        let key = (job_type, dedupe_key.to_string());
        if state.work_jobs_by_type_dedupe.contains_key(&key) {
            return Ok(());
        }
        let job = WorkJob {
            id,
            job_type,
            dedupe_key: dedupe_key.to_string(),
            payload_json,
            state: WorkJobState::Pending,
            attempts: 0,
            max_attempts,
            run_after,
            locked_by: None,
            locked_at: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        state.work_jobs.insert(id, job);
        state.work_jobs_by_type_dedupe.insert(key, id);
        Ok(())
    }

    async fn claim_work_jobs(
        &self,
        job_type: WorkJobType,
        limit: u32,
        locked_by: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Vec<WorkJob>> {
        let mut state = self.0.lock().await;
        let mut ids = state
            .work_jobs
            .values()
            .filter(|j| j.job_type == job_type && j.state == WorkJobState::Pending && j.run_after <= now)
            .map(|j| (j.run_after, j.id))
            .collect::<Vec<_>>();
        ids.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut claimed = Vec::new();
        for (_ra, id) in ids.into_iter().take(limit as usize) {
            let Some(job) = state.work_jobs.get_mut(&id) else { continue };
            job.state = WorkJobState::Running;
            job.locked_by = Some(locked_by.to_string());
            job.locked_at = Some(now);
            job.updated_at = now;
            claimed.push(job.clone());
        }
        Ok(claimed)
    }

    async fn mark_work_job_succeeded(&self, job_id: Uuid, now: OffsetDateTime) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        let Some(job) = state.work_jobs.get_mut(&job_id) else {
            return Err(RepositoryError::NotFound);
        };
        job.state = WorkJobState::Succeeded;
        job.updated_at = now;
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
        let mut state = self.0.lock().await;
        let Some(job) = state.work_jobs.get_mut(&job_id) else {
            return Err(RepositoryError::NotFound);
        };
        job.attempts = job.attempts.saturating_add(1);
        job.last_error = Some(error.to_string());
        job.state = next_state;
        job.run_after = run_after;
        job.updated_at = now;
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
        let mut state = self.0.lock().await;
        state
            .password_reset_tokens
            .entry(token_hash.to_string())
            .or_insert((id, user_id, now, expires_at, None));
        Ok(id)
    }

    async fn consume_password_reset_token(
        &self,
        token_hash: &str,
        now: OffsetDateTime,
    ) -> RepositoryResult<Option<Uuid>> {
        let mut state = self.0.lock().await;
        let Some(entry) = state.password_reset_tokens.get_mut(token_hash) else {
            return Ok(None);
        };
        let (_, user_id, _, expires_at, used_at) = entry;
        if *expires_at <= now || used_at.is_some() {
            return Ok(None);
        }
        let uid = *user_id;
        *used_at = Some(now);
        Ok(Some(uid))
    }

    async fn update_user_password_hash(
        &self,
        user_id: Uuid,
        new_password_hash: &str,
    ) -> RepositoryResult<()> {
        let mut state = self.0.lock().await;
        if !state.users.contains_key(&user_id) {
            return Err(RepositoryError::NotFound);
        }
        state
            .users_auth
            .entry(user_id)
            .and_modify(|(hash, _)| *hash = new_password_hash.to_string())
            .or_insert_with(|| (new_password_hash.to_string(), None));
        state.audit_events.push(AuditEvent::new(
            ActorType::User,
            Some(user_id),
            "user",
            user_id,
            EventType::PasswordReset,
            serde_json::json!({}),
        ));
        Ok(())
    }

    async fn gc_delete_expired_reset_tokens(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64> {
        let mut state = self.0.lock().await;
        let before = state.password_reset_tokens.len();
        state
            .password_reset_tokens
            .retain(|_, (_, _, _, expires_at, used_at)| *expires_at >= cutoff && used_at.is_none());
        let after = state.password_reset_tokens.len();
        Ok(u64::try_from(before - after).unwrap_or(u64::MAX))
    }

    async fn gc_redact_raw_payloads(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64> {
        let mut state = self.0.lock().await;
        let empty = serde_json::json!({});
        let mut count = 0_u64;
        for review in state.reviews.values_mut() {
            if review.ingested_at < cutoff && review.raw_payload != empty {
                review.raw_payload = empty.clone();
                count += 1;
            }
        }
        Ok(count)
    }

    async fn gc_delete_agent_runs(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64> {
        let mut state = self.0.lock().await;
        let mut count = 0_u64;
        for runs in state.agent_runs.values_mut() {
            let before = runs.len();
            runs.retain(|r| r.created_at >= cutoff);
            count += u64::try_from(before - runs.len()).unwrap_or(0);
        }
        state.agent_runs.retain(|_, runs| !runs.is_empty());
        Ok(count)
    }

    async fn gc_delete_sent_notifications(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64> {
        let mut state = self.0.lock().await;
        let ids_to_remove: Vec<Uuid> = state
            .notification_outbox_sent_at
            .iter()
            .filter(|(_, &sent_at)| sent_at < cutoff)
            .map(|(&id, _)| id)
            .collect();
        let count = u64::try_from(ids_to_remove.len()).unwrap_or(0);
        for id in ids_to_remove {
            state.notification_outbox.remove(&id);
            state.notification_outbox_sent_at.remove(&id);
            state.notification_outbox_claimed.remove(&id);
        }
        Ok(count)
    }

    async fn gc_delete_old_webhook_events(&self, cutoff: OffsetDateTime) -> RepositoryResult<u64> {
        let mut state = self.0.lock().await;
        let before = state.webhook_events.len();
        state.webhook_events.retain(|_, &mut received_at| received_at >= cutoff);
        Ok(u64::try_from(before - state.webhook_events.len()).unwrap_or(0))
    }

    async fn gc_delete_expired_idempotency_keys(
        &self,
        cutoff: OffsetDateTime,
    ) -> RepositoryResult<u64> {
        let mut state = self.0.lock().await;
        let before = state.idempotency_responses.len();
        state
            .idempotency_responses
            .retain(|_, (_, _, created_at)| *created_at >= cutoff);
        Ok(u64::try_from(before - state.idempotency_responses.len()).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Platform, ReviewAuthor, ReviewStatus};
    use serde_json::json;
    use time::macros::datetime;

    fn make_review(source_id: &str) -> Review {
        Review {
            id: Uuid::new_v4(),
            platform: Platform::Google,
            source_review_id: source_id.into(),
            source_location_id: "loc-1".into(),
            author: ReviewAuthor {
                display_name: "Maria".into(),
                avatar_url: None,
            },
            rating: 5,
            body_text: Some("Great".into()),
            body_language: Some("en".into()),
            created_at: datetime!(2026-04-10 12:00:00 UTC),
            updated_at: datetime!(2026-04-10 12:00:00 UTC),
            ingested_at: datetime!(2026-04-10 12:01:00 UTC),
            existing_reply_text: None,
            existing_reply_updated_at: None,
            status: ReviewStatus::New,
            context_json: json!({}),
            raw_payload: json!({"v":1}),
        }
    }

    #[tokio::test]
    async fn ingest_review_updates_existing_when_newer() {
        let repo = InMemoryRepository::new();
        let r1 = make_review("r1");
        repo.ingest_review(r1.clone()).await.unwrap();

        let mut r2 = make_review("r1");
        r2.rating = 4;
        r2.updated_at = datetime!(2026-04-11 12:00:00 UTC);
        r2.raw_payload = json!({"v":2});

        repo.ingest_review(r2).await.unwrap();

        let items = repo.list_reviews().await.unwrap();
        assert_eq!(items.len(), 1);
        let (stored, _) = &items[0];
        assert_eq!(stored.rating, 4);
        assert_eq!(stored.raw_payload, json!({"v":2}));
        // Canonical id is preserved (upsert does not create a second review)
        assert_eq!(stored.id, r1.id);
    }

    #[tokio::test]
    async fn ingest_review_does_not_create_duplicate_for_same_platform_and_source_id() {
        let repo = InMemoryRepository::new();
        let r1 = make_review("r1");
        repo.ingest_review(r1.clone()).await.unwrap();

        let mut r2 = make_review("r1");
        r2.id = Uuid::new_v4();
        repo.ingest_review(r2).await.unwrap();

        let items = repo.list_reviews().await.unwrap();
        assert_eq!(items.len(), 1);
        let (stored, _) = &items[0];
        assert_eq!(stored.id, r1.id);
    }

    #[tokio::test]
    async fn reviews_sync_state_roundtrips() {
        let repo = InMemoryRepository::new();
        assert!(repo
            .get_reviews_sync_state(Platform::Google)
            .await
            .unwrap()
            .is_none());

        let t = datetime!(2026-04-12 08:00:00 UTC);
        repo.set_reviews_sync_state(Platform::Google, t)
            .await
            .unwrap();
        assert_eq!(
            repo.get_reviews_sync_state(Platform::Google).await.unwrap(),
            Some(t)
        );
    }

    #[tokio::test]
    async fn webhook_replay_protection_dedupes() {
        let repo = InMemoryRepository::new();
        let now = datetime!(2026-04-12 08:00:00 UTC);
        assert!(repo
            .register_webhook_event(Platform::Ubereats, "evt-1", now)
            .await
            .unwrap());
        assert!(!repo
            .register_webhook_event(Platform::Ubereats, "evt-1", now)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn audit_events_are_written_for_review_and_draft_actions() {
        let repo = InMemoryRepository::new();
        let review = make_review("r1");
        let review_id = review.id;
        repo.ingest_review(review).await.unwrap();

        let events = repo.list_audit_events("review", review_id).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, domain::EventType::ReviewIngested);

        let draft = domain::ReplyDraft {
            id: Uuid::new_v4(),
            review_id,
            generated_by: domain::Generator::AgentLlm,
            model_name: None,
            prompt_fingerprint: None,
            text: "Thanks!".into(),
            language: "en".into(),
            char_count: 7,
            state: domain::DraftState::PendingReview,
            guardrail_warnings: vec![],
            flags: vec![],
            created_at: datetime!(2026-04-12 08:00:00 UTC),
            reviewed_by: None,
            reviewed_at: None,
            rejection_reason: None,
            post_eligible_at: None,
            posted_at: None,
            platform_post_error: None,
        };
        let draft_id = draft.id;
        repo.store_agent_draft(draft).await.unwrap();

        let events = repo.list_audit_events("draft", draft_id).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, domain::EventType::DraftCreated);
    }

    #[tokio::test]
    async fn bulk_approve_undo_window_gates_posting() {
        let repo = InMemoryRepository::new();
        let review = make_review("r1");
        let review_id = review.id;
        repo.ingest_review(review).await.unwrap();

        let draft_id = Uuid::new_v4();
        let draft = domain::ReplyDraft {
            id: draft_id,
            review_id,
            generated_by: domain::Generator::AgentLlm,
            model_name: None,
            prompt_fingerprint: None,
            text: "Thanks!".into(),
            language: "en".into(),
            char_count: 7,
            state: domain::DraftState::PendingReview,
            guardrail_warnings: vec![],
            flags: vec![],
            created_at: datetime!(2026-04-12 08:00:00 UTC),
            reviewed_by: None,
            reviewed_at: None,
            rejection_reason: None,
            post_eligible_at: None,
            posted_at: None,
            platform_post_error: None,
        };
        repo.store_agent_draft(draft).await.unwrap();

        let reviewer = Uuid::new_v4();
        let approved = repo.bulk_approve(&[draft_id], reviewer).await.unwrap();
        let eligible_at = approved[0].post_eligible_at.expect("set by bulk_approve");

        // Before eligible time: cannot mark posted.
        assert!(matches!(
            repo.mark_draft_posted(draft_id, eligible_at - time::Duration::seconds(1))
                .await,
            Err(RepositoryError::InvalidTransition)
        ));

        // After eligible time: can mark posted.
        repo.mark_draft_posted(draft_id, eligible_at + time::Duration::seconds(1))
            .await
            .unwrap();
        let (review, _) = repo.get_review(review_id).await.unwrap();
        assert_eq!(review.status, ReviewStatus::Replied);
    }

    #[tokio::test]
    async fn notification_outbox_enqueues_and_marks_sent() {
        let repo = InMemoryRepository::new();
        let id = Uuid::new_v4();
        repo.enqueue_notification_outbox(
            id,
            datetime!(2026-04-10 12:00:00 UTC),
            domain::NotificationType::DraftReady,
            None,
            None,
            json!({"k":"v"}),
        )
        .await
        .unwrap();

        let batch = repo
            .claim_notification_outbox_batch(10, "test", datetime!(2026-04-10 12:00:30 UTC))
            .await
            .unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].id, id);

        // Claimed items should not be returned again until marked sent.
        let dup = repo
            .claim_notification_outbox_batch(10, "other", datetime!(2026-04-10 12:00:31 UTC))
            .await
            .unwrap();
        assert!(dup.is_empty());

        repo.mark_notification_outbox_sent(id, datetime!(2026-04-10 12:01:00 UTC))
            .await
            .unwrap();
        let batch2 = repo
            .claim_notification_outbox_batch(10, "test", datetime!(2026-04-10 12:02:00 UTC))
            .await
            .unwrap();
        assert!(batch2.is_empty());
    }

    #[tokio::test]
    async fn work_jobs_enqueue_is_idempotent_by_type_and_dedupe() {
        let repo = InMemoryRepository::new();
        let now = datetime!(2026-04-12 08:00:00 UTC);
        repo.enqueue_work_job(
            Uuid::new_v4(),
            WorkJobType::AgentDraftReview,
            "k1",
            json!({"review_id":"r1"}),
            now,
            5,
            now,
        )
        .await
        .unwrap();
        // Same type+dedupe should no-op.
        repo.enqueue_work_job(
            Uuid::new_v4(),
            WorkJobType::AgentDraftReview,
            "k1",
            json!({"review_id":"r1b"}),
            now,
            5,
            now,
        )
        .await
        .unwrap();

        let claimed = repo
            .claim_work_jobs(WorkJobType::AgentDraftReview, 10, "w1", now)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].dedupe_key, "k1");
        assert_eq!(claimed[0].state, WorkJobState::Running);
    }

    #[tokio::test]
    async fn password_reset_token_happy_path() {
        let repo = InMemoryRepository::new();
        let now = datetime!(2026-04-15 10:00:00 UTC);
        let expires_at = now + time::Duration::minutes(30);

        // The seeded owner user has a known id.
        let user_id = Uuid::from_u128(1);

        let token_hash = "abc123deadbeef".to_string();
        repo.create_password_reset_token(user_id, &token_hash, expires_at, now)
            .await
            .unwrap();

        // Consuming before expiry should return the user_id.
        let result = repo
            .consume_password_reset_token(&token_hash, now + time::Duration::seconds(60))
            .await
            .unwrap();
        assert_eq!(result, Some(user_id));

        // Consuming a second time must return None (already used).
        let second = repo
            .consume_password_reset_token(&token_hash, now + time::Duration::seconds(90))
            .await
            .unwrap();
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn password_reset_token_expired_returns_none() {
        let repo = InMemoryRepository::new();
        let now = datetime!(2026-04-15 10:00:00 UTC);
        let expires_at = now + time::Duration::minutes(30);
        let user_id = Uuid::from_u128(1);

        repo.create_password_reset_token(user_id, "tok_expired", expires_at, now)
            .await
            .unwrap();

        // Consuming after expiry should return None.
        let result = repo
            .consume_password_reset_token("tok_expired", expires_at + time::Duration::seconds(1))
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn password_reset_unknown_token_returns_none() {
        let repo = InMemoryRepository::new();
        let now = datetime!(2026-04-15 10:00:00 UTC);
        let result = repo
            .consume_password_reset_token("not_a_real_token", now)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn update_user_password_hash_succeeds_and_writes_audit() {
        let repo = InMemoryRepository::new();
        let user_id = Uuid::from_u128(1);

        repo.update_user_password_hash(user_id, "new_hash_value")
            .await
            .unwrap();

        // Verify the new hash is stored.
        let auth = repo
            .get_user_auth_by_email("owner@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(auth.password_hash, "new_hash_value");

        // Verify audit event written.
        let events = repo.list_audit_events("user", user_id).await.unwrap();
        assert!(events
            .iter()
            .any(|e| e.event_type == domain::EventType::PasswordReset));
    }

    #[tokio::test]
    async fn update_user_password_hash_unknown_user_returns_not_found() {
        let repo = InMemoryRepository::new();
        let fake_id = Uuid::new_v4();
        let result = repo.update_user_password_hash(fake_id, "hash").await;
        assert!(matches!(result, Err(RepositoryError::NotFound)));
    }

    #[tokio::test]
    async fn gc_delete_expired_reset_tokens_removes_expired() {
        let repo = InMemoryRepository::new();
        let now = datetime!(2026-04-15 10:00:00 UTC);
        let user_id = Uuid::from_u128(1);

        // Insert an already-expired token.
        let expires_past = now - time::Duration::minutes(1);
        repo.create_password_reset_token(user_id, "expired_tok", expires_past, now)
            .await
            .unwrap();

        // Insert a valid token.
        let expires_future = now + time::Duration::minutes(30);
        repo.create_password_reset_token(user_id, "valid_tok", expires_future, now)
            .await
            .unwrap();

        let deleted = repo.gc_delete_expired_reset_tokens(now).await.unwrap();
        assert_eq!(deleted, 1, "only the expired token should be deleted");

        // The valid token should still be consumable.
        let result = repo
            .consume_password_reset_token("valid_tok", now + time::Duration::seconds(10))
            .await
            .unwrap();
        assert_eq!(result, Some(user_id));
    }

    #[tokio::test]
    async fn work_jobs_claim_respects_run_after_ordering() {
        let repo = InMemoryRepository::new();
        let now = datetime!(2026-04-12 08:00:00 UTC);
        let later = now + time::Duration::seconds(10);
        repo.enqueue_work_job(
            Uuid::new_v4(),
            WorkJobType::PosterPostReply,
            "a",
            json!({"draft_id":"d1"}),
            later,
            5,
            now,
        )
        .await
        .unwrap();
        repo.enqueue_work_job(
            Uuid::new_v4(),
            WorkJobType::PosterPostReply,
            "b",
            json!({"draft_id":"d2"}),
            now,
            5,
            now,
        )
        .await
        .unwrap();

        let claimed_now = repo
            .claim_work_jobs(WorkJobType::PosterPostReply, 10, "w1", now)
            .await
            .unwrap();
        assert_eq!(claimed_now.len(), 1);
        assert_eq!(claimed_now[0].dedupe_key, "b");

        let claimed_later = repo
            .claim_work_jobs(WorkJobType::PosterPostReply, 10, "w1", later)
            .await
            .unwrap();
        assert_eq!(claimed_later.len(), 1);
        assert_eq!(claimed_later[0].dedupe_key, "a");
    }

    #[tokio::test]
    async fn gc_redact_raw_payloads_only_affects_old_reviews() {
        let repo = InMemoryRepository::new();
        let old_time = datetime!(2026-01-01 00:00:00 UTC);
        let new_time = datetime!(2026-04-10 12:00:00 UTC);
        let cutoff = datetime!(2026-03-01 00:00:00 UTC);

        let mut old_review = make_review("old");
        old_review.ingested_at = old_time;
        old_review.raw_payload = json!({"sensitive": true});
        repo.ingest_review(old_review.clone()).await.unwrap();

        let mut new_review = make_review("new");
        new_review.ingested_at = new_time;
        new_review.raw_payload = json!({"sensitive": true});
        repo.ingest_review(new_review.clone()).await.unwrap();

        let count = repo.gc_redact_raw_payloads(cutoff).await.unwrap();
        assert_eq!(count, 1);

        let reviews = repo.list_reviews().await.unwrap();
        for (r, _) in &reviews {
            if r.source_review_id == "old" {
                assert_eq!(r.raw_payload, json!({}), "old review should be redacted");
            } else {
                assert_eq!(
                    r.raw_payload,
                    json!({"sensitive": true}),
                    "new review must not be redacted"
                );
            }
        }

        // Running again should return 0 (already redacted).
        let count2 = repo.gc_redact_raw_payloads(cutoff).await.unwrap();
        assert_eq!(count2, 0);
    }

    #[tokio::test]
    async fn gc_delete_agent_runs_removes_old_entries() {
        let repo = InMemoryRepository::new();
        let review = make_review("r1");
        let review_id = review.id;
        repo.ingest_review(review).await.unwrap();

        let base_run = domain::AgentRun {
            id: Uuid::new_v4(),
            review_id,
            draft_id: None,
            model_name: None,
            prompt_fingerprint: None,
            prompt_tokens: None,
            completion_tokens: None,
            latency_ms: None,
            tool_calls_json: json!([]),
            guardrail_verdict_json: None,
            error: None,
            created_at: datetime!(2026-01-01 00:00:00 UTC),
        };
        let new_run = domain::AgentRun {
            id: Uuid::new_v4(),
            created_at: datetime!(2026-04-10 12:00:00 UTC),
            ..base_run.clone()
        };
        repo.store_agent_run(base_run).await.unwrap();
        repo.store_agent_run(new_run).await.unwrap();

        let cutoff = datetime!(2026-03-01 00:00:00 UTC);
        let deleted = repo.gc_delete_agent_runs(cutoff).await.unwrap();
        assert_eq!(deleted, 1);

        let runs = repo.list_agent_runs(review_id).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].created_at, datetime!(2026-04-10 12:00:00 UTC));
    }

    #[tokio::test]
    async fn gc_delete_sent_notifications_removes_old_sent_items() {
        let repo = InMemoryRepository::new();
        let old_id = Uuid::new_v4();
        let new_id = Uuid::new_v4();

        repo.enqueue_notification_outbox(
            old_id,
            datetime!(2026-01-01 00:00:00 UTC),
            domain::NotificationType::DraftReady,
            None,
            None,
            json!({}),
        )
        .await
        .unwrap();
        repo.enqueue_notification_outbox(
            new_id,
            datetime!(2026-04-10 12:00:00 UTC),
            domain::NotificationType::DraftReady,
            None,
            None,
            json!({}),
        )
        .await
        .unwrap();

        repo.mark_notification_outbox_sent(old_id, datetime!(2026-01-02 00:00:00 UTC))
            .await
            .unwrap();
        repo.mark_notification_outbox_sent(new_id, datetime!(2026-04-11 00:00:00 UTC))
            .await
            .unwrap();

        let cutoff = datetime!(2026-03-01 00:00:00 UTC);
        let deleted = repo.gc_delete_sent_notifications(cutoff).await.unwrap();
        assert_eq!(deleted, 1);

        let state = repo.0.lock().await;
        assert!(!state.notification_outbox.contains_key(&old_id));
        assert!(state.notification_outbox.contains_key(&new_id));
    }

    #[tokio::test]
    async fn gc_delete_old_webhook_events_removes_expired() {
        let repo = InMemoryRepository::new();
        let old_time = datetime!(2026-01-01 00:00:00 UTC);
        let new_time = datetime!(2026-04-10 12:00:00 UTC);
        let cutoff = datetime!(2026-03-01 00:00:00 UTC);

        // Insert directly into state to bypass the 24-hour auto-cleanup inside
        // `register_webhook_event`, which would discard the old entry before GC runs.
        {
            let mut state = repo.0.lock().await;
            state
                .webhook_events
                .insert((Platform::Google, "evt-old".to_string()), old_time);
            state
                .webhook_events
                .insert((Platform::Google, "evt-new".to_string()), new_time);
        }

        let deleted = repo.gc_delete_old_webhook_events(cutoff).await.unwrap();
        assert_eq!(deleted, 1);

        let state = repo.0.lock().await;
        assert!(!state
            .webhook_events
            .contains_key(&(Platform::Google, "evt-old".to_string())));
        assert!(state
            .webhook_events
            .contains_key(&(Platform::Google, "evt-new".to_string())));
    }

    #[tokio::test]
    async fn gc_delete_expired_idempotency_keys_removes_old_entries() {
        let repo = InMemoryRepository::new();
        let old_time = datetime!(2026-01-01 00:00:00 UTC);
        let new_time = datetime!(2026-04-10 12:00:00 UTC);
        let cutoff = datetime!(2026-03-01 00:00:00 UTC);

        repo.put_idempotency_response("old-key", 200, json!({"ok": true}), old_time)
            .await
            .unwrap();
        repo.put_idempotency_response("new-key", 201, json!({"ok": true}), new_time)
            .await
            .unwrap();

        let deleted = repo
            .gc_delete_expired_idempotency_keys(cutoff)
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        assert!(repo
            .get_idempotency_response("old-key")
            .await
            .unwrap()
            .is_none());
        assert!(repo
            .get_idempotency_response("new-key")
            .await
            .unwrap()
            .is_some());
    }
}
