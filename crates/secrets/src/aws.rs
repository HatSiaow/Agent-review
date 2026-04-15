use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};

use crate::{Secrets, SecretsError, SecretsResult};

/// AWS Secrets Manager-backed secrets provider.
///
/// Keys are mapped to secret ids by prefixing with `secret_prefix`:
/// - key `google.refresh_token` → secret id `{secret_prefix}/google.refresh_token`
pub struct AwsSecretsManagerSecrets {
    client: aws_sdk_secretsmanager::Client,
    secret_prefix: String,
}

impl AwsSecretsManagerSecrets {
    pub async fn new(secret_prefix: impl Into<String>) -> SecretsResult<Self> {
        let cfg = aws_config::load_from_env().await;
        Ok(Self {
            client: aws_sdk_secretsmanager::Client::new(&cfg),
            secret_prefix: secret_prefix.into(),
        })
    }

    fn secret_id(&self, key: &str) -> String {
        format!("{}/{}", self.secret_prefix.trim_end_matches('/'), key)
    }
}

#[async_trait]
impl Secrets for AwsSecretsManagerSecrets {
    async fn get(&self, key: &str) -> SecretsResult<SecretString> {
        let id = self.secret_id(key);
        let out = self
            .client
            .get_secret_value()
            .secret_id(id.clone())
            .send()
            .await
            .map_err(|e| SecretsError::Backend(format!("aws get_secret_value {id}: {e}")))?;

        if let Some(s) = out.secret_string() {
            return Ok(SecretString::new(s.to_string()));
        }
        if let Some(blob) = out.secret_binary() {
            // Best effort: treat as UTF-8.
            let bytes = blob.as_ref();
            let s = String::from_utf8(bytes.to_vec()).map_err(|_| {
                SecretsError::Backend(format!("aws secret {id} is not valid utf-8"))
            })?;
            return Ok(SecretString::new(s));
        }
        Err(SecretsError::NotFound(key.to_string()))
    }

    async fn put(&self, key: &str, value: SecretString) -> SecretsResult<()> {
        let id = self.secret_id(key);
        // Create or update.
        let res = self
            .client
            .put_secret_value()
            .secret_id(id.clone())
            .secret_string(value.expose_secret())
            .send()
            .await;
        match res {
            Ok(_) => Ok(()),
            Err(e) => Err(SecretsError::Backend(format!(
                "aws put_secret_value {id}: {e}"
            ))),
        }
    }
}

