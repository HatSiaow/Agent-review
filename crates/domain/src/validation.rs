use thiserror::Error;

/// Errors arising from domain-level validation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error("rating {0} out of range, must be 1..=5")]
    RatingOutOfRange(u8),

    #[error("source_review_id must not be empty")]
    EmptySourceReviewId,

    #[error("source_location_id must not be empty")]
    EmptySourceLocationId,

    #[error("author display_name must not be empty")]
    EmptyAuthorName,

    #[error("draft text must not be empty")]
    EmptyDraftText,

    #[error("draft language must not be empty")]
    EmptyDraftLanguage,
}

/// Validate that a rating is in the 1..=5 range.
pub fn validate_rating(rating: u8) -> Result<(), ValidationError> {
    if (1..=5).contains(&rating) {
        Ok(())
    } else {
        Err(ValidationError::RatingOutOfRange(rating))
    }
}

/// Validate the minimum required fields for a review on ingestion.
pub fn validate_review_fields(
    source_review_id: &str,
    source_location_id: &str,
    author_name: &str,
    rating: u8,
) -> Result<(), ValidationError> {
    validate_rating(rating)?;
    if source_review_id.trim().is_empty() {
        return Err(ValidationError::EmptySourceReviewId);
    }
    if source_location_id.trim().is_empty() {
        return Err(ValidationError::EmptySourceLocationId);
    }
    if author_name.trim().is_empty() {
        return Err(ValidationError::EmptyAuthorName);
    }
    Ok(())
}

/// Validate that a draft has the bare minimum required content.
pub fn validate_draft_fields(text: &str, language: &str) -> Result<(), ValidationError> {
    if text.trim().is_empty() {
        return Err(ValidationError::EmptyDraftText);
    }
    if language.trim().is_empty() {
        return Err(ValidationError::EmptyDraftLanguage);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ratings() {
        for r in 1..=5 {
            assert!(validate_rating(r).is_ok());
        }
    }

    #[test]
    fn rating_zero_invalid() {
        assert_eq!(
            validate_rating(0),
            Err(ValidationError::RatingOutOfRange(0))
        );
    }

    #[test]
    fn rating_six_invalid() {
        assert_eq!(
            validate_rating(6),
            Err(ValidationError::RatingOutOfRange(6))
        );
    }

    #[test]
    fn rating_255_invalid() {
        assert_eq!(
            validate_rating(255),
            Err(ValidationError::RatingOutOfRange(255))
        );
    }

    #[test]
    fn valid_review_fields() {
        assert!(validate_review_fields("rev-1", "loc-1", "Maria", 5).is_ok());
    }

    #[test]
    fn empty_source_review_id() {
        let err = validate_review_fields("", "loc-1", "Maria", 5).unwrap_err();
        assert_eq!(err, ValidationError::EmptySourceReviewId);
    }

    #[test]
    fn whitespace_author_name() {
        let err = validate_review_fields("rev-1", "loc-1", "   ", 4).unwrap_err();
        assert_eq!(err, ValidationError::EmptyAuthorName);
    }

    #[test]
    fn valid_draft_fields() {
        assert!(validate_draft_fields("Thanks!", "en").is_ok());
    }

    #[test]
    fn empty_draft_text() {
        let err = validate_draft_fields("", "en").unwrap_err();
        assert_eq!(err, ValidationError::EmptyDraftText);
    }

    #[test]
    fn empty_draft_language() {
        let err = validate_draft_fields("Thanks!", "").unwrap_err();
        assert_eq!(err, ValidationError::EmptyDraftLanguage);
    }
}
