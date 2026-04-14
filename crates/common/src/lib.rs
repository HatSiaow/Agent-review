//! Shared bootstrap utilities (tracing, config) used by binaries.

use anyhow::Context as _;

/// Initialize the tracing subscriber with env-based filtering.
///
/// Reads `RUST_LOG` for filter directives; falls back to `info` level.
pub fn init_tracing() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .or_else(|_| tracing_subscriber::EnvFilter::try_new("info"))
        .context("build tracing filter")?;

    tracing_subscriber::fmt().with_env_filter(filter).init();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_tracing_succeeds() {
        let result = init_tracing();
        assert!(result.is_ok());
    }
}

