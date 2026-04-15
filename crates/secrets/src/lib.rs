//! Secrets backend abstraction.
//!
//! ## Why this exists
//! Specs require a dedicated secrets backend so credentials and keys do not live in plaintext
//! environment variables or config files. This crate provides:
//! - A `Secrets` trait returning `secrecy::SecretString` (redacted `Debug`).
//! - A local `EnvFileSecrets` backend using an age-encrypted dotenv-style file.
//! - An optional AWS Secrets Manager backend behind the `aws` Cargo feature.
//!
//! Consumers should treat secrets as sensitive by default and must never log them.

use async_trait::async_trait;
use secrecy::SecretString;

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("secret not found: {0}")]
    NotFound(String),
    #[error("invalid secrets backend configuration: {0}")]
    InvalidConfig(String),
    #[error("backend error: {0}")]
    Backend(String),
}

pub type SecretsResult<T> = Result<T, SecretsError>;

#[async_trait]
pub trait Secrets: Send + Sync {
    async fn get(&self, key: &str) -> SecretsResult<SecretString>;
    async fn put(&self, key: &str, value: SecretString) -> SecretsResult<()>;
}

/// A simple in-memory secrets backend for tests/dev.
#[derive(Default)]
pub struct InMemorySecrets {
    inner: tokio::sync::RwLock<std::collections::HashMap<String, SecretString>>,
}

#[async_trait]
impl Secrets for InMemorySecrets {
    async fn get(&self, key: &str) -> SecretsResult<SecretString> {
        let map = self.inner.read().await;
        map.get(key)
            .cloned()
            .ok_or_else(|| SecretsError::NotFound(key.to_string()))
    }

    async fn put(&self, key: &str, value: SecretString) -> SecretsResult<()> {
        let mut map = self.inner.write().await;
        map.insert(key.to_string(), value);
        Ok(())
    }
}

mod env_file;
pub use env_file::EnvFileSecrets;

#[cfg(feature = "aws")]
mod aws;
#[cfg(feature = "aws")]
pub use aws::AwsSecretsManagerSecrets;

