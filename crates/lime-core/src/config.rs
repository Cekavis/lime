use lime_protocol::{Config, ConfigSnapshot, ErrorCode};
use serde::{Deserialize, Serialize};

/// Inclusive bounds used by Phase 0 configuration validation.
///
/// The design document specifies defaults but not numeric limits. These conservative
/// limits keep accidental pathological values out of the service while leaving room for
/// normal desktop use; they are public so a UI can render the same schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub preceding_text_char_limit: (u32, u32),
    pub context_preview_char_limit: (u32, u32),
    pub page_size: (u32, u32),
    pub llm_rerank_count: (u32, u32),
    pub llm_effective_count: (u32, u32),
    pub llm_context_token_limit: (u32, u32),
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            preceding_text_char_limit: (1, 4096),
            context_preview_char_limit: (0, 1024),
            page_size: (1, 20),
            llm_rerank_count: (1, 128),
            llm_effective_count: (1, 32),
            llm_context_token_limit: (1, 4096),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigValidationError {
    pub field: &'static str,
    pub value: u32,
    pub reason: &'static str,
}

impl std::fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}={} {}", self.field, self.value, self.reason)
    }
}

impl std::error::Error for ConfigValidationError {}

impl ConfigValidationError {
    pub const fn error_code(&self) -> ErrorCode {
        ErrorCode::ConfigValidationFailed
    }
}

/// Validate a wire configuration using the default Phase 0 limits.
pub fn validate(config: &Config) -> Result<(), ConfigValidationError> {
    validate_with(config, &Limits::default())
}

/// Validate a wire configuration using explicit limits.
pub fn validate_with(config: &Config, limits: &Limits) -> Result<(), ConfigValidationError> {
    check(
        "preceding_text_char_limit",
        config.preceding_text_char_limit,
        limits.preceding_text_char_limit,
    )?;
    check(
        "context_preview_char_limit",
        config.context_preview_char_limit,
        limits.context_preview_char_limit,
    )?;
    check("page_size", config.page_size, limits.page_size)?;
    check(
        "llm_rerank_count",
        config.llm_rerank_count,
        limits.llm_rerank_count,
    )?;
    check(
        "llm_effective_count",
        config.llm_effective_count,
        limits.llm_effective_count,
    )?;
    check(
        "llm_context_token_limit",
        config.llm_context_token_limit,
        limits.llm_context_token_limit,
    )?;
    if config.context_preview_char_limit > config.preceding_text_char_limit {
        return Err(ConfigValidationError {
            field: "context_preview_char_limit",
            value: config.context_preview_char_limit,
            reason: "must not exceed preceding_text_char_limit",
        });
    }
    if config.llm_effective_count > config.llm_rerank_count {
        return Err(ConfigValidationError {
            field: "llm_effective_count",
            value: config.llm_effective_count,
            reason: "must not exceed llm_rerank_count",
        });
    }
    Ok(())
}

fn check(field: &'static str, value: u32, bounds: (u32, u32)) -> Result<(), ConfigValidationError> {
    if (bounds.0..=bounds.1).contains(&value) {
        Ok(())
    } else {
        Err(ConfigValidationError {
            field,
            value,
            reason: "is outside the supported range",
        })
    }
}

/// In-memory revisioned owner for service configuration.
#[derive(Clone, Debug)]
pub struct ConfigStore {
    config: Config,
    revision: u64,
    limits: Limits,
}

impl Default for ConfigStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigStore {
    pub fn new() -> Self {
        Self {
            config: Config::default(),
            revision: 0,
            limits: Limits::default(),
        }
    }

    pub fn with_limits(limits: Limits) -> Result<Self, ConfigValidationError> {
        let config = Config::default();
        validate_with(&config, &limits)?;
        Ok(Self {
            config,
            revision: 0,
            limits,
        })
    }

    pub fn snapshot(&self) -> ConfigSnapshot {
        ConfigSnapshot {
            revision: self.revision,
            config: self.config.clone(),
        }
    }

    pub fn replace(&mut self, config: Config) -> Result<ConfigSnapshot, ConfigValidationError> {
        validate_with(&config, &self.limits)?;
        self.config = config;
        self.revision = self.revision.saturating_add(1);
        Ok(self.snapshot())
    }

    pub fn error_code(error: &ConfigValidationError) -> ErrorCode {
        error.error_code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_revision_starts_at_zero() {
        let store = ConfigStore::new();
        assert_eq!(store.snapshot().revision, 0);
        assert!(validate(&store.snapshot().config).is_ok());
    }

    #[test]
    fn invalid_update_is_rejected_atomically() {
        let mut store = ConfigStore::new();
        let before = store.snapshot();
        let mut invalid = before.config.clone();
        invalid.llm_effective_count = invalid.llm_rerank_count + 1;
        assert!(store.replace(invalid).is_err());
        assert_eq!(store.snapshot(), before);
    }

    #[test]
    fn valid_update_increments_revision() {
        let mut store = ConfigStore::new();
        let mut next = store.snapshot().config;
        next.page_size = 12;
        let snapshot = store.replace(next).expect("valid config");
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.config.page_size, 12);
    }
}
