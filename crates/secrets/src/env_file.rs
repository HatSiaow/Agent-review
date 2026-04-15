use std::path::PathBuf;

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};

use crate::{Secrets, SecretsError, SecretsResult};

/// Secrets backend backed by an age-encrypted dotenv file.
///
/// The encrypted file contains lines like:
/// `KEY=value`
///
/// ### Configuration
/// - `path`: location of the encrypted file (recommended: `./secrets.env.age`)
/// - `identity_file`: age identity file path (recommended: `~/.config/rr-agent/age.key`)
///
/// ### Notes
/// - This backend is designed for single-host deployments.
/// - Writes are atomic: the file is rewritten and replaced.
pub struct EnvFileSecrets {
    path: PathBuf,
    identity_file: PathBuf,
}

impl EnvFileSecrets {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, identity_file: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            identity_file: identity_file.into(),
        }
    }

    async fn load_identity(&self) -> SecretsResult<Vec<Box<dyn age::Identity>>> {
        let raw = tokio::fs::read_to_string(&self.identity_file)
            .await
            .map_err(|e| {
                SecretsError::InvalidConfig(format!(
                    "failed to read age identity file {}: {e}",
                    self.identity_file.display()
                ))
            })?;
        let line = raw
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or_else(|| SecretsError::InvalidConfig("age identity file is empty".into()))?;
        let id = line
            .parse::<age::x25519::Identity>()
            .map_err(|e| SecretsError::InvalidConfig(format!("invalid age identity: {e}")))?;
        Ok(vec![Box::new(id)])
    }

    async fn decrypt_file(&self) -> SecretsResult<String> {
        if !self.path.exists() {
            return Ok(String::new());
        }
        let bytes = tokio::fs::read(&self.path)
            .await
            .map_err(|e| SecretsError::Backend(format!("read secrets file: {e}")))?;
        let ids = self.load_identity().await?;
        let decryptor = age::Decryptor::new_buffered(bytes.as_slice())
            .map_err(|e| SecretsError::Backend(format!("age decryptor: {e}")))?;
        let mut reader = decryptor
            .decrypt(ids.iter().map(|i| i.as_ref() as &dyn age::Identity))
            .map_err(|e| SecretsError::Backend(format!("age decrypt: {e}")))?;
        let mut out = String::new();
        use std::io::Read as _;
        reader
            .read_to_string(&mut out)
            .map_err(|e| SecretsError::Backend(format!("read decrypted: {e}")))?;
        Ok(out)
    }

    fn parse_dotenv(contents: &str) -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
        map
    }

    fn render_dotenv(map: &std::collections::HashMap<String, String>) -> String {
        // Stable output to reduce churn.
        let mut keys = map.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        let mut out = String::new();
        for k in keys {
            let v = map.get(&k).expect("key exists");
            out.push_str(&k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
        out
    }

    async fn encrypt_to_file(&self, plaintext: &str) -> SecretsResult<()> {
        // Store the recipient in a public file? v0.1 chooses env-driven recipient string to avoid
        // having to parse the public key from the identity file.
        let raw = std::env::var("APP_AGE_RECIPIENT").map_err(|_| {
            SecretsError::InvalidConfig("APP_AGE_RECIPIENT must be set for EnvFileSecrets writes".into())
        })?;
        let r = raw.parse::<age::x25519::Recipient>().map_err(|e| {
            SecretsError::InvalidConfig(format!("invalid APP_AGE_RECIPIENT: {e}"))
        })?;
        let encryptor = age::Encryptor::with_recipients(std::iter::once(&r as &dyn age::Recipient))
            .map_err(|e| SecretsError::Backend(format!("age encryptor init: {e}")))?;
        let mut out = vec![];
        {
            let mut writer = encryptor
                .wrap_output(&mut out)
                .map_err(|e| SecretsError::Backend(format!("age wrap output: {e}")))?;
            use std::io::Write as _;
            writer
                .write_all(plaintext.as_bytes())
                .map_err(|e| SecretsError::Backend(format!("write encrypted: {e}")))?;
            writer
                .finish()
                .map_err(|e| SecretsError::Backend(format!("finish encrypted: {e}")))?;
        }

        let tmp = self.path.with_extension("tmp");
        tokio::fs::write(&tmp, out)
            .await
            .map_err(|e| SecretsError::Backend(format!("write tmp: {e}")))?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| SecretsError::Backend(format!("atomic replace: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl Secrets for EnvFileSecrets {
    async fn get(&self, key: &str) -> SecretsResult<SecretString> {
        let contents = self.decrypt_file().await?;
        let map = Self::parse_dotenv(&contents);
        map.get(key)
            .cloned()
            .map(SecretString::new)
            .ok_or_else(|| SecretsError::NotFound(key.to_string()))
    }

    async fn put(&self, key: &str, value: SecretString) -> SecretsResult<()> {
        let contents = self.decrypt_file().await?;
        let mut map = Self::parse_dotenv(&contents);
        map.insert(key.to_string(), value.expose_secret().to_string());
        let rendered = Self::render_dotenv(&map);
        self.encrypt_to_file(&rendered).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemorySecrets;

    #[tokio::test]
    async fn in_memory_secrets_roundtrip() {
        let s = InMemorySecrets::default();
        s.put("a", SecretString::new("b".to_string())).await.unwrap();
        let v = s.get("a").await.unwrap();
        assert_eq!(v.expose_secret(), "b");
    }

    #[test]
    fn dotenv_parser_ignores_comments_and_whitespace() {
        let raw = r#"
            # comment
            A=1

            B = two
        "#;
        let map = EnvFileSecrets::parse_dotenv(raw);
        assert_eq!(map.get("A").unwrap(), "1");
        assert_eq!(map.get("B").unwrap(), "two");
    }
}

