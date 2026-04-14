use serde::{Deserialize, Serialize};

use crate::Platform;

/// Individual warning produced by a guardrail check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardrailWarning {
    pub rule: String,
    pub message: String,
}

/// Overall verdict after running all guardrails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailVerdict {
    Pass,
    Warn,
    Fail,
}

/// Result of evaluating all guardrails on a draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardrailResult {
    pub verdict: GuardrailVerdict,
    pub warnings: Vec<GuardrailWarning>,
}

impl GuardrailResult {
    #[must_use]
    pub fn pass() -> Self {
        Self {
            verdict: GuardrailVerdict::Pass,
            warnings: Vec::new(),
        }
    }
}

type CheckFn = Box<dyn Fn(&str, &GuardrailContext) -> Option<GuardrailWarning> + Send + Sync>;

/// A single guardrail check function.
pub struct GuardrailCheck {
    pub name: &'static str,
    check: CheckFn,
}

impl GuardrailCheck {
    pub fn new(
        name: &'static str,
        check: impl Fn(&str, &GuardrailContext) -> Option<GuardrailWarning> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name,
            check: Box::new(check),
        }
    }

    pub fn run(&self, text: &str, ctx: &GuardrailContext) -> Option<GuardrailWarning> {
        (self.check)(text, ctx)
    }
}

/// Context provided to guardrail checks.
#[derive(Debug, Clone)]
pub struct GuardrailContext {
    pub platform: Platform,
    pub review_language: Option<String>,
    pub review_rating: u8,
    pub char_limit: u32,
}

/// Run all guardrails and produce a verdict.
#[must_use]
pub fn evaluate_guardrails(
    text: &str,
    ctx: &GuardrailContext,
    checks: &[GuardrailCheck],
) -> GuardrailResult {
    let warnings: Vec<GuardrailWarning> =
        checks.iter().filter_map(|c| c.run(text, ctx)).collect();

    let verdict = if warnings.is_empty() {
        GuardrailVerdict::Pass
    } else {
        GuardrailVerdict::Warn
    };

    GuardrailResult { verdict, warnings }
}

/// The standard set of guardrails from the spec.
#[must_use]
pub fn default_checks() -> Vec<GuardrailCheck> {
    vec![
        check_length(),
        check_no_refund_promises(),
        check_no_legal_liability(),
        check_no_staff_pii(),
        check_no_profanity(),
        check_language_match(),
        check_self_reference(),
    ]
}

fn check_length() -> GuardrailCheck {
    GuardrailCheck::new("length_limit", |text, ctx| {
        let count = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        if count > ctx.char_limit {
            Some(GuardrailWarning {
                rule: "length_limit".into(),
                message: format!(
                    "Draft is {count} chars, exceeds {platform} limit of {limit}",
                    platform = ctx.platform,
                    limit = ctx.char_limit,
                ),
            })
        } else {
            None
        }
    })
}

fn check_no_refund_promises() -> GuardrailCheck {
    const REFUND_PATTERNS: &[&str] = &[
        "full refund",
        "free meal",
        "complimentary meal",
        "money back",
        "we'll comp",
        "on the house",
        "free of charge",
    ];

    GuardrailCheck::new("no_refund_promises", |text, ctx| {
        if ctx.review_rating > 2 {
            let lower = text.to_lowercase();
            for pattern in REFUND_PATTERNS {
                if lower.contains(pattern) {
                    return Some(GuardrailWarning {
                        rule: "no_refund_promises".into(),
                        message: format!(
                            "Refund/comp language (\"{pattern}\") not allowed for rating > 2"
                        ),
                    });
                }
            }
        }
        None
    })
}

fn check_no_legal_liability() -> GuardrailCheck {
    const LIABILITY_PATTERNS: &[&str] = &[
        "we accept liability",
        "our fault",
        "we are liable",
        "we take responsibility for the harm",
        "we admit",
        "negligence on our part",
    ];

    GuardrailCheck::new("no_legal_liability", |text, _ctx| {
        let lower = text.to_lowercase();
        for pattern in LIABILITY_PATTERNS {
            if lower.contains(pattern) {
                return Some(GuardrailWarning {
                    rule: "no_legal_liability".into(),
                    message: format!("Legal liability language detected: \"{pattern}\""),
                });
            }
        }
        None
    })
}

fn check_no_staff_pii() -> GuardrailCheck {
    GuardrailCheck::new("no_staff_pii", |text, _ctx| {
        let email_re = regex_lite::Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}")
            .expect("valid regex");
        let phone_re =
            regex_lite::Regex::new(r"\+?\d[\d\s\-()]{7,}\d").expect("valid regex");

        if email_re.is_match(text) {
            return Some(GuardrailWarning {
                rule: "no_staff_pii".into(),
                message: "Email address detected in draft".into(),
            });
        }
        if phone_re.is_match(text) {
            return Some(GuardrailWarning {
                rule: "no_staff_pii".into(),
                message: "Phone number detected in draft".into(),
            });
        }
        None
    })
}

fn check_no_profanity() -> GuardrailCheck {
    const PROFANITY: &[&str] = &["damn", "hell", "shit", "fuck", "ass", "bastard", "crap"];

    GuardrailCheck::new("no_profanity", |text, _ctx| {
        let lower = text.to_lowercase();
        let words: Vec<&str> = lower.split_whitespace().collect();
        for word in &words {
            let cleaned: String = word.chars().filter(|c| c.is_alphabetic()).collect();
            for bad in PROFANITY {
                if cleaned == *bad {
                    return Some(GuardrailWarning {
                        rule: "no_profanity".into(),
                        message: format!("Profanity detected: \"{cleaned}\""),
                    });
                }
            }
        }
        None
    })
}

fn check_language_match() -> GuardrailCheck {
    GuardrailCheck::new("language_match", |text, ctx| {
        let review_lang = ctx.review_language.as_ref()?;

        if review_lang.starts_with("en") && !looks_english(text) {
            return Some(GuardrailWarning {
                rule: "language_match".into(),
                message: "Draft may not match the review's language (en)".into(),
            });
        }
        None
    })
}

fn looks_english(text: &str) -> bool {
    let ascii_alpha = text.chars().filter(char::is_ascii_alphabetic).count();
    let total_alpha = text.chars().filter(|c| c.is_alphabetic()).count();
    if total_alpha == 0 {
        return true;
    }
    // For short review replies, precision loss from usize->f64 is harmless.
    #[allow(clippy::cast_precision_loss)]
    let ratio = ascii_alpha as f64 / total_alpha as f64;
    ratio > 0.7
}

fn check_self_reference() -> GuardrailCheck {
    GuardrailCheck::new("self_reference", |text, _ctx| {
        let lower = format!(" {} ", text.to_lowercase());
        let markers = [" we ", " our ", " us ", "we're", "we've", "we'd", "we'll"];
        for marker in markers {
            if lower.contains(marker) {
                return None;
            }
        }
        Some(GuardrailWarning {
            rule: "self_reference".into(),
            message: "Draft should contain a first-person-plural marker (we/our/us)".into(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(platform: Platform, rating: u8) -> GuardrailContext {
        GuardrailContext {
            platform,
            review_language: Some("en".into()),
            review_rating: rating,
            char_limit: match platform {
                Platform::Google => 1000,
                Platform::Ubereats => 500,
            },
        }
    }

    #[test]
    fn length_within_limit_passes() {
        let checks = vec![check_length()];
        let result = evaluate_guardrails("Short reply from us.", &ctx(Platform::Google, 5), &checks);
        assert_eq!(result.verdict, GuardrailVerdict::Pass);
    }

    #[test]
    fn length_over_limit_warns() {
        let long_text = "a".repeat(501);
        let checks = vec![check_length()];
        let result = evaluate_guardrails(&long_text, &ctx(Platform::Ubereats, 5), &checks);
        assert_eq!(result.verdict, GuardrailVerdict::Warn);
        assert_eq!(result.warnings[0].rule, "length_limit");
    }

    #[test]
    fn refund_promise_blocked_for_high_rating() {
        let checks = vec![check_no_refund_promises()];
        let result = evaluate_guardrails(
            "We'd love to offer you a full refund!",
            &ctx(Platform::Google, 4),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Warn);
    }

    #[test]
    fn refund_language_allowed_for_low_rating() {
        let checks = vec![check_no_refund_promises()];
        let result = evaluate_guardrails(
            "We'd love to offer you a full refund.",
            &ctx(Platform::Google, 1),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Pass);
    }

    #[test]
    fn legal_liability_detected() {
        let checks = vec![check_no_legal_liability()];
        let result = evaluate_guardrails(
            "We accept liability for this incident.",
            &ctx(Platform::Google, 1),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Warn);
    }

    #[test]
    fn email_pii_detected() {
        let checks = vec![check_no_staff_pii()];
        let result = evaluate_guardrails(
            "Please contact john@restaurant.com for help.",
            &ctx(Platform::Google, 3),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Warn);
        assert_eq!(result.warnings[0].rule, "no_staff_pii");
    }

    #[test]
    fn phone_pii_detected() {
        let checks = vec![check_no_staff_pii()];
        let result = evaluate_guardrails(
            "Call us at +1 555-123-4567 for help.",
            &ctx(Platform::Google, 3),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Warn);
    }

    #[test]
    fn profanity_detected() {
        let checks = vec![check_no_profanity()];
        let result = evaluate_guardrails(
            "That's damn right, we are glad!",
            &ctx(Platform::Google, 5),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Warn);
    }

    #[test]
    fn clean_text_passes_profanity() {
        let checks = vec![check_no_profanity()];
        let result = evaluate_guardrails(
            "We are delighted you enjoyed your visit!",
            &ctx(Platform::Google, 5),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Pass);
    }

    #[test]
    fn self_reference_present() {
        let checks = vec![check_self_reference()];
        let result =
            evaluate_guardrails("We hope to see you again!", &ctx(Platform::Google, 5), &checks);
        assert_eq!(result.verdict, GuardrailVerdict::Pass);
    }

    #[test]
    fn self_reference_missing() {
        let checks = vec![check_self_reference()];
        let result = evaluate_guardrails(
            "Thanks for the kind words!",
            &ctx(Platform::Google, 5),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Warn);
        assert_eq!(result.warnings[0].rule, "self_reference");
    }

    #[test]
    fn all_default_checks_pass_for_good_reply() {
        let checks = default_checks();
        let result = evaluate_guardrails(
            "Thank you so much! We are thrilled you enjoyed the pasta. Hope to see you again soon!",
            &ctx(Platform::Google, 5),
            &checks,
        );
        assert_eq!(result.verdict, GuardrailVerdict::Pass);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn multiple_violations_collected() {
        let checks = default_checks();
        let text = "damn, here's a full refund: john@example.com +1-555-0000000";
        let result = evaluate_guardrails(text, &ctx(Platform::Google, 4), &checks);
        assert_eq!(result.verdict, GuardrailVerdict::Warn);
        assert!(result.warnings.len() >= 3);
    }
}
