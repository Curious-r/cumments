//! Building and validating the site-auth policy and PoW/admin secrets.

use super::settings::{Mode, Security, SiteConfig};
use anyhow::{Result, anyhow, bail};
use cumments_core::models::ID_REGEX;
use cumments_core::site_auth::{
    KNOWN_SECRET_PLACEHOLDERS, OriginPattern, SITE_SECRET_MIN_LENGTH, SiteAuthMode, SiteAuthPolicy,
    SitePolicyEntry, SiteVerificationPolicy,
};
use std::collections::HashMap;

/// Builds the effective site-auth policy from the configuration, validating
/// and normalizing every operator-declared entry.
pub fn build_site_auth_policy(
    security: &Security,
    sites: &HashMap<String, SiteConfig>,
) -> Result<SiteAuthPolicy> {
    let mut policy = SiteAuthPolicy {
        verification: security.site_verification,
        sites: HashMap::new(),
    };

    for (site_id, config) in sites {
        if !ID_REGEX.is_match(site_id) {
            bail!(
                "`sites.{site_id}` has an invalid site id; expected 1-64 lowercase \
                 letters, digits or hyphens"
            );
        }

        let allowed_origins = config
            .allowed_origins
            .iter()
            .map(|raw| {
                OriginPattern::parse(raw).map_err(|e| {
                    anyhow!("`sites.{site_id}.allowed_origins` entry `{raw}` is invalid: {e}")
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let secret = match (&config.auth_mode, &config.secret) {
            (Some(SiteAuthMode::Secret), Some(secret)) => {
                if secret.len() < SITE_SECRET_MIN_LENGTH {
                    bail!(
                        "`sites.{site_id}.secret` must be at least {SITE_SECRET_MIN_LENGTH} \
                         characters (it is an HMAC key, not a label)"
                    );
                }
                if KNOWN_SECRET_PLACEHOLDERS.contains(&secret.as_str()) {
                    bail!(
                        "`sites.{site_id}.secret` uses the known example value `{secret}`; \
                         generate a real random secret"
                    );
                }
                Some(secret.clone())
            }
            (Some(SiteAuthMode::Secret), None) => {
                bail!(
                    "`sites.{site_id}.auth_mode = \"secret\"` requires a secret; set \
                     `CUMMENTS__SITES__{site_id}__SECRET` or add `secret` to the site config"
                );
            }
            (None | Some(SiteAuthMode::Origin), Some(_)) => {
                bail!(
                    "`sites.{site_id}.secret` is set but `auth_mode` is not `\"secret\"`; \
                     the secret would be ignored"
                );
            }
            (None | Some(SiteAuthMode::Origin), None) => None,
        };

        if config.auth_mode == Some(SiteAuthMode::Secret) && !allowed_origins.is_empty() {
            tracing::warn!(
                "`sites.{site_id}.allowed_origins` is ignored because \
                 `auth_mode = \"secret\"` authenticates with the HMAC key"
            );
        }

        for pattern in &allowed_origins {
            warn_http_non_loopback(site_id, pattern);
        }

        if config.auth_mode != Some(SiteAuthMode::Secret)
            && allowed_origins.is_empty()
            && security.site_verification == SiteVerificationPolicy::Required
        {
            tracing::warn!(
                "`sites.{site_id}` has no `allowed_origins`; under `required` \
                 verification, writes will be rejected unless the site verifies \
                 an origin through the API"
            );
        }

        policy.sites.insert(
            site_id.clone(),
            SitePolicyEntry {
                auth_mode: config.auth_mode,
                allowed_origins,
                secret,
            },
        );
    }

    Ok(policy)
}

/// Warns about plain-HTTP origin patterns that are not loopback addresses.
fn warn_http_non_loopback(site_id: &str, pattern: &OriginPattern) {
    let (scheme, host) = match pattern {
        OriginPattern::Exact(origin) => {
            let Ok(url) = origin.as_str().parse::<url::Url>() else {
                return;
            };
            (url.scheme().to_string(), url.host_str().map(str::to_owned))
        }
        OriginPattern::Wildcard {
            scheme,
            host_suffix,
            ..
        } => (scheme.clone(), Some(host_suffix.clone())),
    };
    if scheme == "http"
        && !matches!(
            host.as_deref(),
            Some("localhost") | Some("127.0.0.1") | Some("::1")
        )
    {
        tracing::warn!(
            "`sites.{site_id}.allowed_origins` allows plain HTTP origin `{}`; \
             use HTTPS in production",
            pattern.as_pattern_string()
        );
    }
}

/// Validates the admin token and returns its SHA-256 hash for comparison.
pub fn admin_token_hash(security: &Security) -> Result<Option<String>> {
    let Some(token) = &security.admin_token else {
        return Ok(None);
    };
    if token.trim().is_empty() {
        bail!("`security.admin_token` must not be empty");
    }
    if token.len() < 32 {
        bail!("`security.admin_token` must be at least 32 characters");
    }
    if matches!(token.as_str(), "change-me" | "admin-token") {
        bail!("`security.admin_token` uses a known example value; generate a real random token");
    }
    Ok(Some(cumments_core::site_auth::token_hash(token)))
}

/// Known example/placeholder secrets shipped in the repository. They are
/// harmless in `logging` mode but would let anyone forge PoW challenges in
/// production.
pub fn is_known_pow_placeholder(secret: &str) -> bool {
    matches!(
        secret,
        "change-me" | "pow_secret_key" | "dev-only-secret-0123456789abcdef"
    )
}

/// Validate the PoW secret: it must never be empty, and in AppService mode it
/// must not be a publicly known placeholder.
pub fn validate_pow_secret(secret: &str, mode: Mode) -> Result<()> {
    if secret.trim().is_empty() {
        bail!("`security.pow_secret` must not be empty");
    }
    if mode == Mode::AppService && is_known_pow_placeholder(secret) {
        bail!(
            "`security.pow_secret` uses the known example value `{}`; \
             set a real random secret before running in appservice mode",
            secret
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow_secret_validation_rejects_empty_and_placeholders_in_appservice() {
        assert!(validate_pow_secret("", Mode::Logging).is_err());
        assert!(validate_pow_secret("   ", Mode::AppService).is_err());
        assert!(validate_pow_secret("change-me", Mode::AppService).is_err());
        assert!(validate_pow_secret("pow_secret_key", Mode::AppService).is_err());
        // The runnable example stays usable in logging mode.
        assert!(validate_pow_secret("change-me", Mode::Logging).is_ok());
        assert!(validate_pow_secret("a-real-secret", Mode::AppService).is_ok());
    }

    #[test]
    fn site_policy_build_normalizes_origins_and_validates_secrets() {
        let security = Security {
            pow_secret: "secret".to_string(),
            pow_difficulty: 4,
            site_verification: SiteVerificationPolicy::Optional,
            admin_token: None,
            allow_private_verification_origins: false,
            preset_stickers: Vec::new(),
        };

        let mut sites = HashMap::new();
        sites.insert(
            "my-blog".to_string(),
            SiteConfig {
                auth_mode: None,
                allowed_origins: vec![
                    "https://Blog.Example.com".to_string(),
                    "https://*.example.net".to_string(),
                ],
                secret: None,
            },
        );
        let policy = build_site_auth_policy(&security, &sites).expect("valid policy");
        let entry = policy.entry("my-blog").expect("entry exists");
        assert_eq!(entry.allowed_origins.len(), 2);

        sites.insert(
            "secret-site".to_string(),
            SiteConfig {
                auth_mode: Some(SiteAuthMode::Secret),
                allowed_origins: vec![],
                secret: Some("a".repeat(SITE_SECRET_MIN_LENGTH)),
            },
        );
        let policy = build_site_auth_policy(&security, &sites).expect("secret site valid");
        assert_eq!(
            policy
                .entry("secret-site")
                .and_then(|e| e.secret.as_deref()),
            Some("a".repeat(SITE_SECRET_MIN_LENGTH).as_str())
        );
    }

    #[test]
    fn site_policy_build_rejects_invalid_site_ids_and_allows_empty_origins() {
        let security = Security {
            pow_secret: "secret".to_string(),
            pow_difficulty: 4,
            site_verification: SiteVerificationPolicy::Required,
            admin_token: None,
            allow_private_verification_origins: false,
            preset_stickers: Vec::new(),
        };

        let mut sites = HashMap::new();
        sites.insert(
            "Bad Site".to_string(),
            SiteConfig {
                auth_mode: None,
                allowed_origins: vec![],
                secret: None,
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());

        // Empty allowlists remain valid (API-verified origins can still be
        // added at runtime); the required-mode warning is emitted instead of
        // failing startup.
        sites.clear();
        sites.insert(
            "my-blog".to_string(),
            SiteConfig {
                auth_mode: None,
                allowed_origins: vec![],
                secret: None,
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_ok());
    }

    #[test]
    fn site_policy_build_rejects_invalid_combinations() {
        let security = Security {
            pow_secret: "secret".to_string(),
            pow_difficulty: 4,
            site_verification: SiteVerificationPolicy::Optional,
            admin_token: None,
            allow_private_verification_origins: false,
            preset_stickers: Vec::new(),
        };

        let mut sites = HashMap::new();
        sites.insert(
            "bad-origin".to_string(),
            SiteConfig {
                auth_mode: None,
                allowed_origins: vec!["https://example.com/path".to_string()],
                secret: None,
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());

        sites.clear();
        sites.insert(
            "no-secret".to_string(),
            SiteConfig {
                auth_mode: Some(SiteAuthMode::Secret),
                allowed_origins: vec![],
                secret: None,
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());

        sites.clear();
        sites.insert(
            "short-secret".to_string(),
            SiteConfig {
                auth_mode: Some(SiteAuthMode::Secret),
                allowed_origins: vec![],
                secret: Some("short".to_string()),
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());

        sites.clear();
        sites.insert(
            "ignored-secret".to_string(),
            SiteConfig {
                auth_mode: Some(SiteAuthMode::Origin),
                allowed_origins: vec![],
                secret: Some("a".repeat(SITE_SECRET_MIN_LENGTH)),
            },
        );
        assert!(build_site_auth_policy(&security, &sites).is_err());
    }

    #[test]
    fn admin_token_hash_validates_and_hashes() {
        let security = Security {
            pow_secret: "secret".to_string(),
            pow_difficulty: 4,
            site_verification: SiteVerificationPolicy::Optional,
            admin_token: None,
            allow_private_verification_origins: false,
            preset_stickers: Vec::new(),
        };
        assert!(
            admin_token_hash(&security)
                .expect("no token yields none")
                .is_none()
        );

        let mut security = security;
        security.admin_token = Some("short".to_string());
        assert!(admin_token_hash(&security).is_err());

        security.admin_token = Some("change-me".to_string());
        assert!(admin_token_hash(&security).is_err());

        security.admin_token = Some("a-very-long-admin-token-0123456789".to_string());
        let hash = admin_token_hash(&security)
            .expect("valid token")
            .expect("some hash");
        assert_eq!(
            hash,
            cumments_core::site_auth::token_hash("a-very-long-admin-token-0123456789")
        );
    }
}
