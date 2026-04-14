use domain::{Platform, Review, ReviewAuthor, ReviewStatus};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::UberEatsAdapterError;

/// Normalize a raw UberEats review JSON into a domain `Review`.
pub fn normalize_ubereats_review(raw: &Value) -> Result<Review, UberEatsAdapterError> {
    let review_uuid = raw["review_uuid"]
        .as_str()
        .ok_or(UberEatsAdapterError::MissingField("review_uuid"))?;

    let rating = raw["rating"]["overall"]
        .as_u64()
        .and_then(|r| u8::try_from(r).ok())
        .ok_or(UberEatsAdapterError::MissingField("rating.overall"))?;

    if !(1..=5).contains(&rating) {
        return Err(UberEatsAdapterError::ParseError(format!(
            "rating out of range: {rating}"
        )));
    }

    let first_name = raw["eater"]["first_name"].as_str().unwrap_or("there");
    let display_name = format!("{first_name}.");

    let body_text = raw["comment"]["text"].as_str().map(String::from);
    let body_language = raw["comment"]["language"].as_str().map(String::from);

    let store_uuid = raw["store_uuid"].as_str().unwrap_or_default().to_string();

    let created_at = parse_timestamp(raw["created_at"].as_str())
        .unwrap_or_else(OffsetDateTime::now_utc);

    let order_id = raw["order_uuid"].as_str().map(String::from);
    let ordered_items: Vec<String> = raw["items"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let context = serde_json::json!({
        "order_id": order_id,
        "ordered_items": ordered_items,
    });

    let now = OffsetDateTime::now_utc();

    Ok(Review {
        id: Uuid::new_v4(),
        platform: Platform::Ubereats,
        source_review_id: review_uuid.to_string(),
        source_location_id: store_uuid,
        author: ReviewAuthor {
            display_name,
            avatar_url: None,
        },
        rating,
        body_text,
        body_language,
        created_at,
        updated_at: created_at,
        ingested_at: now,
        existing_reply_text: None,
        existing_reply_updated_at: None,
        status: ReviewStatus::New,
        context_json: context,
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

    fn sample_ubereats_payload() -> Value {
        json!({
            "review_uuid": "ue-review-123",
            "store_uuid": "store-abc",
            "eater": { "first_name": "Marco" },
            "rating": { "overall": 5 },
            "comment": { "text": "Amazing biryani!", "language": "en" },
            "created_at": "2026-04-10T14:30:00Z",
            "order_uuid": "order-xyz",
            "items": [
                { "name": "Lamb Biryani" },
                { "name": "Garlic Naan" }
            ]
        })
    }

    #[test]
    fn normalizes_basic_review() {
        let review = normalize_ubereats_review(&sample_ubereats_payload()).unwrap();
        assert_eq!(review.platform, Platform::Ubereats);
        assert_eq!(review.source_review_id, "ue-review-123");
        assert_eq!(review.source_location_id, "store-abc");
        assert_eq!(review.rating, 5);
        assert_eq!(review.author.display_name, "Marco.");
        assert_eq!(review.body_text.as_deref(), Some("Amazing biryani!"));
        assert_eq!(review.body_language.as_deref(), Some("en"));
    }

    #[test]
    fn context_includes_order_and_items() {
        let review = normalize_ubereats_review(&sample_ubereats_payload()).unwrap();
        let items = review.context_json["ordered_items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].as_str().unwrap(), "Lamb Biryani");
    }

    #[test]
    fn anonymous_eater_fallback() {
        let mut payload = sample_ubereats_payload();
        payload["eater"] = json!({});
        let review = normalize_ubereats_review(&payload).unwrap();
        assert_eq!(review.author.display_name, "there.");
    }

    #[test]
    fn missing_review_uuid_errors() {
        let payload = json!({"rating": {"overall": 5}});
        let err = normalize_ubereats_review(&payload).unwrap_err();
        assert!(matches!(err, UberEatsAdapterError::MissingField("review_uuid")));
    }

    #[test]
    fn missing_rating_errors() {
        let payload = json!({"review_uuid": "abc"});
        let err = normalize_ubereats_review(&payload).unwrap_err();
        assert!(matches!(err, UberEatsAdapterError::MissingField("rating.overall")));
    }

    #[test]
    fn rating_out_of_range_errors() {
        let payload = json!({"review_uuid": "abc", "rating": {"overall": 7}});
        let err = normalize_ubereats_review(&payload).unwrap_err();
        assert!(matches!(err, UberEatsAdapterError::ParseError(_)));
    }

    #[test]
    fn raw_payload_preserved() {
        let payload = sample_ubereats_payload();
        let review = normalize_ubereats_review(&payload).unwrap();
        assert_eq!(review.raw_payload, payload);
    }
}
