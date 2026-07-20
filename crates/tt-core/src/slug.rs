//! Stream slug format validation.
//!
//! Slugs are short, stable, kebab-case identifiers: `[a-z0-9]+(-[a-z0-9]+)*`, max 32 chars.

use thiserror::Error;

/// Maximum slug length in bytes (slugs are ASCII, so bytes == chars).
pub const MAX_SLUG_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SlugError {
    #[error("slug '{slug}' is invalid: expected lowercase kebab-case ([a-z0-9]+(-[a-z0-9]+)*)")]
    InvalidFormat { slug: String },
    #[error("slug '{slug}' is too long: {len} chars (max {MAX_SLUG_LEN})")]
    TooLong { slug: String, len: usize },
}

/// Validates a stream slug: `[a-z0-9]+(-[a-z0-9]+)*`, max 32 chars.
pub fn validate_slug(slug: &str) -> Result<(), SlugError> {
    if slug.len() > MAX_SLUG_LEN {
        return Err(SlugError::TooLong {
            slug: slug.to_string(),
            len: slug.len(),
        });
    }
    let valid = !slug.is_empty()
        && slug.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(SlugError::InvalidFormat {
            slug: slug.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_slugs() {
        for slug in ["a", "watcher-rewrite", "eval3-moto", "a1-b2-c3"] {
            assert!(validate_slug(slug).is_ok(), "{slug} should be valid");
        }
    }

    #[test]
    fn rejects_invalid_slugs() {
        for slug in [
            "",
            "-leading",
            "trailing-",
            "double--dash",
            "UPPER",
            "has space",
            "has_underscore",
            "ünïcode",
        ] {
            assert!(validate_slug(slug).is_err(), "{slug} should be invalid");
        }
    }

    #[test]
    fn rejects_slugs_over_32_chars() {
        let long = "a".repeat(33);
        assert!(matches!(
            validate_slug(&long),
            Err(SlugError::TooLong { .. })
        ));
        assert!(validate_slug(&"a".repeat(32)).is_ok());
    }
}
