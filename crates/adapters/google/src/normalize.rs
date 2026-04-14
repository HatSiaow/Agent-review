use domain::{Platform, Review, ReviewAuthor, ReviewStatus};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::GoogleAdapterError;

/// Normalize a raw Google Business Profile review JSON into a domain `Review`.
pub fn normalize_google_review(raw: &Value) -> Result<Review, GoogleAdapterError> {
    let review_id = raw["reviewId"]
        .as_str()
        .ok_or(GoogleAdapterError::MissingField("reviewId"))?;

    let star_rating_str = raw["starRating"]
        .as_str()
        .ok_or(GoogleAdapterError::MissingField("starRating"))?;
    let rating = crate::star_rating_to_u8(star_rating_str)
        .ok_or_else(|| GoogleAdapterError::ParseError(format!("unknown starRating: {star_rating_str}")))?;

    let display_name = raw["reviewer"]["displayName"]
        .as_str()
        .unwrap_or("Anonymous")
        .to_string();
    let avatar_url = raw["reviewer"]["profilePhotoUrl"]
        .as_str()
        .and_then(|u| url::Url::parse(u).ok());

    let body_text = raw["comment"].as_str().map(String::from);

    let created_at = parse_timestamp(raw["createTime"].as_str())
        .unwrap_or_else(OffsetDateTime::now_utc);
    let updated_at = parse_timestamp(raw["updateTime"].as_str())
        .unwrap_or(created_at);

    let existing_reply_text = raw["reviewReply"]["comment"].as_str().map(String::from);
    let existing_reply_updated_at =
        parse_timestamp(raw["reviewReply"]["updateTime"].as_str());

    let now = OffsetDateTime::now_utc();

    Ok(Review {
        id: Uuid::new_v4(),
        platform: Platform::Google,
        source_review_id: review_id.to_string(),
        source_location_id: String::new(),
        author: ReviewAuthor {
            display_name,
            avatar_url,
        },
        rating,
        body_text,
        body_language: None,
        created_at,
        updated_at,
        ingested_at: now,
        existing_reply_text,
        existing_reply_updated_at,
        status: ReviewStatus::New,
        context_json: serde_json::json!({}),
        raw_payload: raw.clone(),
    })
}

fn parse_timestamp(s: Option<&str>) -> Option<OffsetDateTime> {
    let s = s?;
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_google_payload() -> Value {
        json!({
            "reviewId": "AbCd1234",
            "reviewer": {
                "displayName": "Maria L.",
                "profilePhotoUrl": "https://example.com/photo.jpg"
            },
            "starRating": "FOUR",
            "comment": "Great pasta but the wait was long.",
            "createTime": "2026-04-10T18:22:11Z",
            "updateTime": "2026-04-10T18:22:11Z"
        })
    }

    #[test]
    fn normalizes_basic_review() {
        let review = normalize_google_review(&sample_google_payload()).unwrap();
        assert_eq!(review.platform, Platform::Google);
        assert_eq!(review.source_review_id, "AbCd1234");
        assert_eq!(review.rating, 4);
        assert_eq!(review.author.display_name, "Maria L.");
        assert!(review.author.avatar_url.is_some());
        assert_eq!(review.body_text.as_deref(), Some("Great pasta but the wait was long."));
        assert_eq!(review.status, ReviewStatus::New);
    }

    #[test]
    fn rating_only_review_has_no_body() {
        let mut payload = sample_google_payload();
        payload.as_object_mut().unwrap().remove("comment");
        let review = normalize_google_review(&payload).unwrap();
        assert!(review.body_text.is_none());
    }

    #[test]
    fn missing_review_id_errors() {
        let payload = json!({"starRating": "FIVE"});
        let err = normalize_google_review(&payload).unwrap_err();
        assert!(matches!(err, crate::GoogleAdapterError::MissingField("reviewId")));
    }

    #[test]
    fn missing_star_rating_errors() {
        let payload = json!({"reviewId": "abc"});
        let err = normalize_google_review(&payload).unwrap_err();
        assert!(matches!(err, crate::GoogleAdapterError::MissingField("starRating")));
    }

    #[test]
    fn existing_reply_parsed() {
        let mut payload = sample_google_payload();
        payload["reviewReply"] = json!({
            "comment": "Thanks for your review!",
            "updateTime": "2026-04-11T10:00:00Z"
        });
        let review = normalize_google_review(&payload).unwrap();
        assert_eq!(review.existing_reply_text.as_deref(), Some("Thanks for your review!"));
        assert!(review.existing_reply_updated_at.is_some());
    }

    #[test]
    fn anonymous_reviewer_falls_back() {
        let payload = json!({
            "reviewId": "abc",
            "starRating": "THREE",
            "reviewer": {}
        });
        let review = normalize_google_review(&payload).unwrap();
        assert_eq!(review.author.display_name, "Anonymous");
    }

    #[test]
    fn raw_payload_preserved() {
        let payload = sample_google_payload();
        let review = normalize_google_review(&payload).unwrap();
        assert_eq!(review.raw_payload, payload);
    }
}
