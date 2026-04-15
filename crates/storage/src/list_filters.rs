//! In-memory filtering/sorting for review and draft list queries.
//!
//! Used by both `InMemoryRepository` and `PgRepository` so API semantics stay consistent.
//! **Why tests matter:** queue tabs (`NeedsYouNow` / `ReadyToSend` / `History`) are product-critical;
//! diverging SQL vs memory behavior would hide bugs in CI.

use crate::repo::{DraftListQuery, QueueTab, ReviewListQuery, ReviewSort};
use domain::{DraftState, ReplyDraft, Review, ReviewStatus};

fn draft_has_queue_risk(d: &ReplyDraft, review: &Review) -> bool {
    if d.has_warnings() {
        return true;
    }
    let sensitive = d.flags.iter().any(|f| f == "sensitive");
    let needs_attention = d.flags.iter().any(|f| f == "needs_attention");
    sensitive || needs_attention || review.rating <= 2
}

/// `Needs You Now` / `Ready To Send` / `History` per `specs/coder/06-human-in-the-loop-workflow.md`.
fn matches_queue_tab(
    review: &Review,
    active: Option<&ReplyDraft>,
    tab: QueueTab,
) -> bool {
    match tab {
        QueueTab::History => {
            matches!(
                review.status,
                ReviewStatus::Replied | ReviewStatus::Skipped | ReviewStatus::Withdrawn
            ) || active.is_some_and(|d| {
                matches!(
                    d.state,
                    DraftState::Posted | DraftState::Rejected | DraftState::Failed
                )
            })
        }
        QueueTab::NeedsYouNow => {
            if review.status != ReviewStatus::AwaitingHuman {
                return false;
            }
            let Some(d) = active else {
                return false;
            };
            if !d.is_active() {
                return false;
            }
            draft_has_queue_risk(d, review)
        }
        QueueTab::ReadyToSend => {
            if review.status != ReviewStatus::AwaitingHuman {
                return false;
            }
            let Some(d) = active else {
                return false;
            };
            if !d.is_active() {
                return false;
            }
            if !(4..=5).contains(&review.rating) {
                return false;
            }
            if draft_has_queue_risk(d, review) {
                return false;
            }
            true
        }
    }
}

#[must_use]
pub fn filter_sort_reviews(
    mut rows: Vec<(Review, Option<ReplyDraft>)>,
    query: &ReviewListQuery,
) -> Vec<(Review, Option<ReplyDraft>)> {
    rows.retain(|(r, d)| {
        if let Some(p) = query.platform {
            if r.platform != p {
                return false;
            }
        }
        if let Some(st) = query.status {
            if r.status != st {
                return false;
            }
        }
        if let Some(rating) = query.rating {
            if r.rating != rating {
                return false;
            }
        }
        if let Some(ref q) = query.q {
            let q = q.to_lowercase();
            let author = r.author.display_name.to_lowercase();
            let body = r
                .body_text
                .as_ref()
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            if !author.contains(&q) && !body.contains(&q) {
                return false;
            }
        }
        if let Some(tab) = query.queue {
            if !matches_queue_tab(r, d.as_ref(), tab) {
                return false;
            }
        }
        true
    });

    if let Some(tab) = query.queue {
        match tab {
            QueueTab::NeedsYouNow => rows.sort_by(|a, b| a.0.created_at.cmp(&b.0.created_at)),
            QueueTab::ReadyToSend => rows.sort_by(|a, b| {
                match b.0.rating.cmp(&a.0.rating) {
                    std::cmp::Ordering::Equal => {
                        let ac = a.1.as_ref().map(|d| d.created_at).unwrap_or(a.0.created_at);
                        let bc = b.1.as_ref().map(|d| d.created_at).unwrap_or(b.0.created_at);
                        bc.cmp(&ac)
                    }
                    o => o,
                }
            }),
            QueueTab::History => rows.sort_by(|a, b| b.0.updated_at.cmp(&a.0.updated_at)),
        }
    } else {
        rows.sort_by(|a, b| match query.sort {
            ReviewSort::UpdatedAtDesc => b.0.updated_at.cmp(&a.0.updated_at),
            ReviewSort::UpdatedAtAsc => a.0.updated_at.cmp(&b.0.updated_at),
            ReviewSort::RatingDesc => b.0.rating.cmp(&a.0.rating),
            ReviewSort::CreatedAtDesc => b.0.created_at.cmp(&a.0.created_at),
        });
    }

    rows
}

#[must_use]
pub fn filter_sort_drafts(
    mut rows: Vec<(ReplyDraft, Review)>,
    query: &DraftListQuery,
) -> Vec<ReplyDraft> {
    rows.retain(|(d, r)| {
        if let Some(st) = query.state {
            if d.state != st {
                return false;
            }
        }
        if let Some(rating) = query.rating {
            if r.rating != rating {
                return false;
            }
        }
        if let Some(true) = query.flag_warnings {
            if !d.has_warnings() {
                return false;
            }
        }
        if let Some(false) = query.flag_warnings {
            if d.has_warnings() {
                return false;
            }
        }
        true
    });

    rows.sort_by(|a, b| b.0.created_at.cmp(&a.0.created_at));
    rows.into_iter().map(|(d, _)| d).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Platform, ReplyDraft, Review, ReviewAuthor, ReviewStatus};
    use time::macros::datetime;
    use uuid::Uuid;

    fn sample_review(status: ReviewStatus, rating: u8) -> Review {
        Review {
            id: Uuid::new_v4(),
            platform: Platform::Google,
            source_review_id: "s1".into(),
            source_location_id: "loc".into(),
            author: ReviewAuthor {
                display_name: "Pat".into(),
                avatar_url: None,
            },
            rating,
            body_text: Some("Hi".into()),
            body_language: Some("en".into()),
            created_at: datetime!(2026-04-10 12:00:00 UTC),
            updated_at: datetime!(2026-04-10 12:00:00 UTC),
            ingested_at: datetime!(2026-04-10 12:01:00 UTC),
            existing_reply_text: None,
            existing_reply_updated_at: None,
            status,
            context_json: serde_json::json!({}),
            raw_payload: serde_json::json!({}),
        }
    }

    #[test]
    fn needs_you_now_matches_sensitive_flag() {
        let r = sample_review(ReviewStatus::AwaitingHuman, 5);
        let mut d = ReplyDraft::new_pending(r.id, "Thanks".into(), "en".into());
        d.flags.push("sensitive".into());
        let rows = vec![(r.clone(), Some(d))];
        let q = ReviewListQuery {
            queue: Some(QueueTab::NeedsYouNow),
            ..Default::default()
        };
        let out = filter_sort_reviews(rows, &q);
        assert_eq!(out.len(), 1);

        let rows2 = vec![(r, None)];
        assert!(filter_sort_reviews(rows2, &q).is_empty());
    }
}
