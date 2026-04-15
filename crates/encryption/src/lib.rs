//! Envelope encryption helpers for encrypting sensitive values at rest.
//!
//! ## Why this exists
//! The application stores sensitive material (e.g. session secrets, OAuth refresh tokens, TOTP
//! seeds). Even though some values are already hashed (like password hashes), the database should
//! still be treated as a high-value target. Encrypting sensitive columns at rest reduces the blast
//! radius of accidental DB snapshots/backups leakage and makes it harder to exfiltrate secrets
//! without also compromising the secrets backend.
//!
//! This crate implements:
//! - AES-256-GCM authenticated encryption for values (the "data key" is the master key for v0.1).
//! - A compact, versioned payload format suitable for storage in text columns.
//! - Dual-key rotation support: decrypt with either key; lazily re-encrypt with the primary key.

use aes_gcm::aead::{Aead as _, KeyInit as _};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use rand_core::RngCore as _;
use secrecy::{ExposeSecret as _, SecretString};

#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("unsupported encrypted payload version")]
    UnsupportedVersion,
    #[error("invalid encrypted payload format")]
    InvalidPayload,
    #[error("unknown key id")]
    UnknownKeyId,
    #[error("cryptographic operation failed")]
    Crypto,
}

/// Master key id used for selecting the correct key during decryption.
pub type MasterKeyId = String;

/// 32-byte master key for AES-256-GCM.
#[derive(Clone)]
pub struct MasterKey {
    id: MasterKeyId,
    bytes: [u8; 32],
}

impl MasterKey {
    #[must_use]
    pub fn new(id: impl Into<String>, bytes: [u8; 32]) -> Self {
        Self {
            id: id.into(),
            bytes,
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Key ring for rotation: decrypt accepts both primary and (optional) secondary.
#[derive(Clone)]
pub struct MasterKeyRing {
    primary: MasterKey,
    secondary: Option<MasterKey>,
}

impl MasterKeyRing {
    #[must_use]
    pub fn new(primary: MasterKey, secondary: Option<MasterKey>) -> Self {
        Self { primary, secondary }
    }

    #[must_use]
    pub fn primary_id(&self) -> &str {
        self.primary.id()
    }
}

/// Versioned encrypted payload.
///
/// Text format:
/// `enc:v1:<key_id>:<base64(nonce||ciphertext)>`
///
/// The ciphertext includes the AEAD tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedText {
    pub key_id: MasterKeyId,
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

impl EncryptedText {
    const PREFIX: &'static str = "enc:v1:";

    /// Serialize to a stable text format suitable for storage in `text` columns.
    #[must_use]
    pub fn to_storage_string(&self) -> String {
        let mut buf = Vec::with_capacity(12 + self.ciphertext.len());
        buf.extend_from_slice(&self.nonce);
        buf.extend_from_slice(&self.ciphertext);
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(buf);
        format!("{}{}:{}", Self::PREFIX, self.key_id, b64)
    }

    /// Parse a payload previously created by `to_storage_string`.
    pub fn from_storage_string(s: &str) -> Result<Self, EncryptionError> {
        let rest = s.strip_prefix(Self::PREFIX).ok_or(EncryptionError::InvalidPayload)?;
        let (key_id, b64) = rest.split_once(':').ok_or(EncryptionError::InvalidPayload)?;
        let raw = base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(b64)
            .map_err(|_| EncryptionError::InvalidPayload)?;
        if raw.len() < 12 {
            return Err(EncryptionError::InvalidPayload);
        }
        let mut nonce = [0_u8; 12];
        nonce.copy_from_slice(&raw[..12]);
        Ok(Self {
            key_id: key_id.to_string(),
            nonce,
            ciphertext: raw[12..].to_vec(),
        })
    }
}

/// Envelope encryptor used at storage boundaries.
#[derive(Clone)]
pub struct EnvelopeEncryptor {
    keys: MasterKeyRing,
}

impl EnvelopeEncryptor {
    #[must_use]
    pub fn new(keys: MasterKeyRing) -> Self {
        Self { keys }
    }

    /// Encrypt a secret string into an `EncryptedText` payload.
    pub fn encrypt(&self, plaintext: &SecretString) -> Result<EncryptedText, EncryptionError> {
        let cipher = Aes256Gcm::new_from_slice(&self.keys.primary.bytes)
            .map_err(|_| EncryptionError::Crypto)?;
        let mut nonce = [0_u8; 12];
        rand_core::OsRng.fill_bytes(&mut nonce);
        let nonce_gcm = Nonce::from_slice(&nonce);
        let ct = cipher
            .encrypt(nonce_gcm, plaintext.expose_secret().as_bytes())
            .map_err(|_| EncryptionError::Crypto)?;
        Ok(EncryptedText {
            key_id: self.keys.primary.id.clone(),
            nonce,
            ciphertext: ct,
        })
    }

    /// Decrypt an `EncryptedText` payload. Returns the plaintext and, if rotation is active,
    /// an optional re-encrypted payload under the primary key.
    pub fn decrypt_and_maybe_reencrypt(
        &self,
        payload: &EncryptedText,
    ) -> Result<(SecretString, Option<EncryptedText>), EncryptionError> {
        let (key, needs_reencrypt) = if payload.key_id == self.keys.primary.id {
            (&self.keys.primary, false)
        } else if let Some(secondary) = &self.keys.secondary {
            if payload.key_id == secondary.id {
                (secondary, true)
            } else {
                return Err(EncryptionError::UnknownKeyId);
            }
        } else {
            return Err(EncryptionError::UnknownKeyId);
        };

        let cipher =
            Aes256Gcm::new_from_slice(&key.bytes).map_err(|_| EncryptionError::Crypto)?;
        let nonce = Nonce::from_slice(&payload.nonce);
        let pt = cipher
            .decrypt(nonce, payload.ciphertext.as_slice())
            .map_err(|_| EncryptionError::Crypto)?;
        let secret = SecretString::new(
            String::from_utf8(pt).map_err(|_| EncryptionError::InvalidPayload)?,
        );

        if needs_reencrypt {
            let new_payload = self.encrypt(&secret)?;
            Ok((secret, Some(new_payload)))
        } else {
            Ok((secret, None))
        }
    }

    /// Convenience: decrypt from a storage string payload.
    pub fn decrypt_storage_string(
        &self,
        s: &str,
    ) -> Result<(SecretString, Option<String>), EncryptionError> {
        let payload = EncryptedText::from_storage_string(s)?;
        let (pt, maybe_new) = self.decrypt_and_maybe_reencrypt(&payload)?;
        Ok((pt, maybe_new.map(|p| p.to_storage_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_key(id: &str) -> MasterKey {
        let mut bytes = [0_u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        MasterKey::new(id.to_string(), bytes)
    }

    #[test]
    fn roundtrip_encrypt_decrypt_primary() {
        let ring = MasterKeyRing::new(random_key("k1"), None);
        let enc = EnvelopeEncryptor::new(ring);
        let secret = SecretString::new("hello".to_string());
        let payload = enc.encrypt(&secret).expect("encrypt");
        let (pt, rotated) = enc.decrypt_and_maybe_reencrypt(&payload).expect("decrypt");
        assert_eq!(pt.expose_secret(), "hello");
        assert!(rotated.is_none());
    }

    #[test]
    fn rotation_decrypts_secondary_and_reencrypts_primary() {
        let primary = random_key("k2");
        let secondary = random_key("k1");
        let ring = MasterKeyRing::new(primary.clone(), Some(secondary.clone()));
        let enc = EnvelopeEncryptor::new(ring.clone());

        let old_enc = EnvelopeEncryptor::new(MasterKeyRing::new(secondary, None));
        let secret = SecretString::new("rotate-me".to_string());
        let old_payload = old_enc.encrypt(&secret).expect("encrypt old");

        let (pt, maybe_new) = enc
            .decrypt_and_maybe_reencrypt(&old_payload)
            .expect("decrypt");
        assert_eq!(pt.expose_secret(), "rotate-me");
        let new_payload = maybe_new.expect("should reencrypt");
        assert_eq!(new_payload.key_id, primary.id);
    }

    #[test]
    fn storage_string_format_is_parseable() {
        let ring = MasterKeyRing::new(random_key("k1"), None);
        let enc = EnvelopeEncryptor::new(ring);
        let secret = SecretString::new("abc".to_string());
        let payload = enc.encrypt(&secret).expect("encrypt");
        let s = payload.to_storage_string();
        let parsed = EncryptedText::from_storage_string(&s).expect("parse");
        assert_eq!(parsed.key_id, payload.key_id);
        assert_eq!(parsed.nonce, payload.nonce);
        assert_eq!(parsed.ciphertext, payload.ciphertext);
    }
}

