use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HmacVerificationError {
    #[error("missing signature header")]
    MissingSignature,

    #[error("invalid signature format")]
    InvalidFormat,

    #[error("signature mismatch")]
    Mismatch,
}

/// Verify an UberEats webhook HMAC-SHA256 signature.
///
/// `signature_header` is the value of the `x-uber-signature` header.
/// `secret` is the shared HMAC secret.
/// `body` is the raw request body bytes.
pub fn verify_webhook_signature(
    signature_header: &str,
    secret: &[u8],
    body: &[u8],
) -> Result<(), HmacVerificationError> {
    if signature_header.is_empty() {
        return Err(HmacVerificationError::MissingSignature);
    }

    let expected_bytes =
        hex::decode(signature_header).map_err(|_| HmacVerificationError::InvalidFormat)?;

    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| HmacVerificationError::InvalidFormat)?;
    mac.update(body);
    let computed = mac.finalize().into_bytes();

    if computed.ct_eq(&expected_bytes).into() {
        Ok(())
    } else {
        Err(HmacVerificationError::Mismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute_signature(secret: &[u8], body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn valid_signature_accepted() {
        let secret = b"test-webhook-secret";
        let body = b"{\"event_type\":\"store.review_created\"}";
        let sig = compute_signature(secret, body);
        assert!(verify_webhook_signature(&sig, secret, body).is_ok());
    }

    #[test]
    fn wrong_signature_rejected() {
        let secret = b"test-webhook-secret";
        let body = b"some body";
        let wrong_sig = compute_signature(b"wrong-secret", body);
        let err = verify_webhook_signature(&wrong_sig, secret, body).unwrap_err();
        assert_eq!(err, HmacVerificationError::Mismatch);
    }

    #[test]
    fn empty_signature_header() {
        let err = verify_webhook_signature("", b"secret", b"body").unwrap_err();
        assert_eq!(err, HmacVerificationError::MissingSignature);
    }

    #[test]
    fn invalid_hex_rejected() {
        let err = verify_webhook_signature("not-hex-zz!", b"secret", b"body").unwrap_err();
        assert_eq!(err, HmacVerificationError::InvalidFormat);
    }

    #[test]
    fn tampered_body_rejected() {
        let secret = b"secret";
        let body = b"original body";
        let sig = compute_signature(secret, body);
        let err = verify_webhook_signature(&sig, secret, b"tampered body").unwrap_err();
        assert_eq!(err, HmacVerificationError::Mismatch);
    }
}
