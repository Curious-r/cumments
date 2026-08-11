//! Site identity and write-path authentication.
//!
//! This module owns the domain types and pure logic behind the two trust
//! anchors:
//!
//! - `origin`: a browser `Origin` bound to a verified domain.
//! - `secret`: an HMAC key held by the site's backend (edge function).
//!
//! HTTP fetching, DNS resolution and storage live outside this module; the
//! services here orchestrate them through the [`SiteAuthStore`] port.

use crate::models::SiteId;
use crate::ports::SiteAuthStore;
use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use subtle::ConstantTimeEq;
use url::Url;

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// HTTP protocol constants
// ---------------------------------------------------------------------------

/// Header carrying the Unix timestamp of an HMAC-signed site request.
pub const SITE_TIMESTAMP_HEADER: &str = "X-Cumments-Timestamp";
/// Header carrying the HMAC-SHA256 signature of a site request.
pub const SITE_SIGNATURE_HEADER: &str = "X-Cumments-Signature";
/// Header carrying the claim token that proves ownership of an API-registered site.
pub const CLAIM_TOKEN_HEADER: &str = "X-Cumments-Claim-Token";
/// Maximum accepted clock skew for HMAC request timestamps, in seconds.
pub const SITE_SIGNATURE_MAX_SKEW_SECONDS: i64 = 300;
/// Validity window of a verification challenge.
pub const VERIFICATION_TOKEN_TTL_HOURS: i64 = 1;
/// Minimum length of a site secret (32 random bytes, hex-encoded = 64 chars).
pub const SITE_SECRET_MIN_LENGTH: usize = 32;
/// Known placeholder values that must never be used as a real site secret.
pub const KNOWN_SECRET_PLACEHOLDERS: [&str; 2] = ["change-me", "site-secret"];

// ---------------------------------------------------------------------------
// Hashing and random generation
// ---------------------------------------------------------------------------

/// SHA-256 digest of `input`, hex-encoded.
pub fn sha256_hex(input: &[u8]) -> String {
    hex::encode(Sha256::digest(input))
}

/// SHA-256 digest of a token, used as the stored form of any secret.
pub fn token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// Constant-time comparison of two byte slices.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// Generates a cryptographically random token (32 bytes, hex-encoded).
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Generates a random `site_id` (16 bytes, hex-encoded).
///
/// Random ids make API-registered sites unguessable: a squatter cannot claim
/// someone else's id because they cannot guess it.
pub fn generate_site_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

// ---------------------------------------------------------------------------
// HMAC request signing
// ---------------------------------------------------------------------------

/// Computes the HMAC-SHA256 signature of a site request.
///
/// The canonical message is:
/// `timestamp\nMETHOD\npath\nsha256_hex(body)`
/// The body is hashed first so the signature stays stable for large payloads.
pub fn site_request_signature(
    secret: &[u8],
    timestamp: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> String {
    let body_hash = sha256_hex(body);
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts keys of any size");
    mac.update(timestamp.as_bytes());
    mac.update(b"\n");
    mac.update(method.as_bytes());
    mac.update(b"\n");
    mac.update(path.as_bytes());
    mac.update(b"\n");
    mac.update(body_hash.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Verifies the HMAC signature of a site request in constant time.
pub fn verify_site_request_signature(
    secret: &[u8],
    timestamp: &str,
    method: &str,
    path: &str,
    body: &[u8],
    signature_hex: &str,
) -> bool {
    let Ok(expected) = hex::decode(signature_hex) else {
        return false;
    };
    let body_hash = sha256_hex(body);
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts keys of any size");
    mac.update(timestamp.as_bytes());
    mac.update(b"\n");
    mac.update(method.as_bytes());
    mac.update(b"\n");
    mac.update(path.as_bytes());
    mac.update(b"\n");
    mac.update(body_hash.as_bytes());
    mac.verify_slice(&expected).is_ok()
}

/// Whether a request timestamp is within `max_skew_seconds` of `now`.
pub fn is_timestamp_fresh(timestamp: &str, now: DateTime<Utc>, max_skew_seconds: i64) -> bool {
    let Ok(ts) = timestamp.parse::<i64>() else {
        return false;
    };
    (now.timestamp() - ts).abs() <= max_skew_seconds
}

// ---------------------------------------------------------------------------
// Origins
// ---------------------------------------------------------------------------

/// A canonical browser origin: `scheme://host[:port]`, with the default port
/// omitted and the host normalized (lowercase, punycode).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Origin(String);

impl Origin {
    /// Parses and canonicalizes an origin string.
    ///
    /// Accepts `http`/`https` origins only, without userinfo, path, query or
    /// fragment. The canonical form matches the browser `Origin` serialization
    /// (default ports omitted).
    pub fn parse(value: &str) -> Result<Self> {
        let url =
            Url::parse(value).map_err(|e| anyhow::anyhow!("invalid origin `{value}`: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("origin `{value}` must use http or https");
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("origin `{value}` must not contain userinfo");
        }
        if !matches!(url.path(), "" | "/") {
            bail!("origin `{value}` must not contain a path");
        }
        if url.query().is_some() || url.fragment().is_some() {
            bail!("origin `{value}` must not contain a query or fragment");
        }
        Ok(Self(url.origin().ascii_serialization()))
    }

    /// The canonical origin string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The host part of the origin, if it is a domain or IP literal.
    pub fn host(&self) -> Option<String> {
        Url::parse(&self.0)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
    }
}

/// A matching rule for allowed origins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginPattern {
    /// An exact canonical origin.
    Exact(Origin),
    /// `scheme://*.suffix[:port]`: matches every subdomain of `suffix`.
    Wildcard {
        scheme: String,
        host_suffix: String,
        port: Option<u16>,
    },
}

impl OriginPattern {
    /// Parses an exact origin or a subdomain wildcard pattern.
    pub fn parse(value: &str) -> Result<Self> {
        let Some((scheme, rest)) = value.split_once("://") else {
            bail!("origin pattern `{value}` must include a scheme");
        };
        if !matches!(scheme, "http" | "https") {
            bail!("origin pattern `{value}` must use http or https");
        }

        if let Some(host_and_port) = rest.strip_prefix("*.") {
            let (host_suffix, port) = split_port(host_and_port)
                .ok_or_else(|| anyhow::anyhow!("invalid port in origin pattern `{value}`"))?;
            if host_suffix.is_empty() {
                bail!("origin pattern `{value}` needs a suffix after `*.`");
            }
            // Validate the suffix by parsing a synthetic concrete origin.
            let synthetic = match port {
                Some(p) => format!("{scheme}://placeholder.{host_suffix}:{p}"),
                None => format!("{scheme}://placeholder.{host_suffix}"),
            };
            let parsed = Origin::parse(&synthetic)?;
            let host = parsed
                .host()
                .ok_or_else(|| anyhow::anyhow!("origin pattern `{value}` has no host"))?;
            let host_suffix = host
                .strip_prefix("placeholder.")
                .ok_or_else(|| anyhow::anyhow!("invalid wildcard suffix in `{value}`"))?
                .to_string();
            return Ok(Self::Wildcard {
                scheme: scheme.to_string(),
                host_suffix,
                port,
            });
        }

        Ok(Self::Exact(Origin::parse(value)?))
    }

    /// Whether a concrete origin matches this pattern.
    pub fn matches(&self, origin: &Origin) -> bool {
        match self {
            Self::Exact(exact) => exact == origin,
            Self::Wildcard {
                scheme,
                host_suffix,
                port,
            } => {
                let Ok(url) = Url::parse(origin.as_str()) else {
                    return false;
                };
                if url.scheme() != scheme {
                    return false;
                }
                let Some(host) = url.host_str() else {
                    return false;
                };
                if host == host_suffix || !host.ends_with(&format!(".{host_suffix}")) {
                    return false;
                }
                let expected_port = port.or_else(|| match scheme.as_str() {
                    "https" => Some(443),
                    _ => Some(80),
                });
                url.port_or_known_default() == expected_port
            }
        }
    }
}

fn split_port(host_and_port: &str) -> Option<(String, Option<u16>)> {
    match host_and_port.rsplit_once(':') {
        Some((host, port)) => port
            .parse::<u16>()
            .ok()
            .map(|p| (host.to_string(), Some(p))),
        None => Some((host_and_port.to_string(), None)),
    }
}

// ---------------------------------------------------------------------------
// Site auth model
// ---------------------------------------------------------------------------

/// How a site authenticates write requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteAuthMode {
    /// Requests are accepted from verified browser origins.
    Origin,
    /// Requests must carry a valid HMAC signature.
    Secret,
}

impl SiteAuthMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Secret => "secret",
        }
    }
}

/// Verification state of an API-registered site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteVerificationStatus {
    Unverified,
    Verified,
}

impl SiteVerificationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Verified => "verified",
        }
    }
}

/// Instance-wide policy for unverified sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteVerificationPolicy {
    /// Site auth is turned off entirely (local development and tests).
    Disabled,
    /// Unverified sites keep working (migration default); verified sites are
    /// enforced.
    #[default]
    Optional,
    /// Unverified sites are rejected.
    Required,
}

/// Operator-declared trust for one site (the config overlay).
#[derive(Debug, Clone, Default)]
pub struct SitePolicyEntry {
    pub auth_mode: Option<SiteAuthMode>,
    pub allowed_origins: Vec<OriginPattern>,
    /// The HMAC secret itself, when configured by the operator. Held in
    /// memory only; never logged or serialized.
    pub secret: Option<String>,
}

/// The effective write-path policy: instance-wide verification policy plus
/// the operator-declared per-site overlay.
#[derive(Debug, Clone, Default)]
pub struct SiteAuthPolicy {
    pub verification: SiteVerificationPolicy,
    pub sites: HashMap<String, SitePolicyEntry>,
}

impl SiteAuthPolicy {
    pub fn entry(&self, site_id: &str) -> Option<&SitePolicyEntry> {
        self.sites.get(site_id)
    }
}

/// Full authentication state of a site as stored in the database.
#[derive(Debug, Clone)]
pub struct SiteAuthInfo {
    pub site_id: String,
    pub auth_mode: SiteAuthMode,
    pub verification_status: SiteVerificationStatus,
    pub verified_origins: Vec<Origin>,
    pub verified_at: Option<DateTime<Utc>>,
    /// The site's HMAC key. Stored in the local database (the HMAC verifier
    /// needs the key itself); never logged or returned by the API.
    pub secret: Option<String>,
}

/// A site registered through the API, with its one-time claim token.
#[derive(Debug, Clone, Serialize)]
pub struct RegisteredSite {
    pub site_id: String,
    pub claim_token: String,
}

/// The proof locations accepted for one verification challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationMethod {
    /// `/.well-known/cumments.json` on the site's origin.
    WellKnown,
    /// `_cumments.<host>` TXT record.
    Dns,
}

impl VerificationMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WellKnown => "well-known",
            Self::Dns => "dns",
        }
    }
}

/// A single origin proof published by the site owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteProof {
    pub site_id: String,
    pub token: String,
}

/// A pending verification challenge issued by `start`.
#[derive(Debug, Clone)]
pub struct VerificationChallenge {
    pub site_id: String,
    pub token: String,
    pub methods: Vec<VerificationMethod>,
    pub origins: Vec<Origin>,
    pub expires_at: DateTime<Utc>,
}

/// A stored verification token row.
#[derive(Debug, Clone)]
pub struct VerificationToken {
    pub id: i64,
    pub site_id: String,
    pub origin: Origin,
    pub token_hash: String,
    pub methods: Vec<VerificationMethod>,
    pub expires_at: DateTime<Utc>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Insert payload for a verification token row.
#[derive(Debug, Clone)]
pub struct NewVerificationToken {
    pub site_id: String,
    pub origin: Origin,
    pub token_hash: String,
    pub methods: Vec<VerificationMethod>,
    pub expires_at: DateTime<Utc>,
}

/// A freshly issued HMAC secret. The raw value is returned exactly once.
#[derive(Debug, Clone, Serialize)]
pub struct IssuedSiteSecret {
    pub site_id: String,
    pub secret: String,
}

// ---------------------------------------------------------------------------
// Proof parsing
// ---------------------------------------------------------------------------

/// Parses the site proofs from a `/.well-known/cumments.json` document.
///
/// Accepted shapes:
/// - `{ "site_id": "...", "token": "..." }`
/// - `{ "sites": [ { "site_id": "...", "token": "..." }, ... ] }`
pub fn parse_well_known_proofs(content: &str) -> Result<Vec<SiteProof>> {
    let value: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| anyhow::anyhow!("invalid well-known document: {e}"))?;

    let entries: Vec<&serde_json::Value> = match &value {
        serde_json::Value::Object(map) if map.contains_key("sites") => map
            .get("sites")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("`sites` must be an array"))?
            .iter()
            .collect(),
        serde_json::Value::Object(_) => vec![&value],
        _ => bail!("well-known document must be a JSON object"),
    };

    entries
        .iter()
        .map(|entry| {
            let site_id = entry
                .get("site_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("proof entry is missing `site_id`"))?;
            let token = entry
                .get("token")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("proof entry is missing `token`"))?;
            Ok(SiteProof {
                site_id: site_id.to_string(),
                token: token.to_string(),
            })
        })
        .collect()
}

/// Parses one DNS TXT record value in the format
/// `site_id=<id>,token=<token>`.
pub fn parse_dns_proof_record(value: &str) -> Option<SiteProof> {
    let mut site_id = None;
    let mut token = None;
    for part in value.split(',') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("site_id=") {
            site_id = Some(rest.trim().to_string());
        } else if let Some(rest) = part.strip_prefix("token=") {
            token = Some(rest.trim().to_string());
        }
    }
    match (site_id, token) {
        (Some(site_id), Some(token)) if !site_id.is_empty() && !token.is_empty() => {
            Some(SiteProof { site_id, token })
        }
        _ => None,
    }
}

/// Whether any TXT record matches the expected `(site_id, token)` proof.
pub fn dns_proofs_match(records: &[String], site_id: &str, token: &str) -> bool {
    records.iter().any(|record| {
        parse_dns_proof_record(record)
            .is_some_and(|proof| proof.site_id == site_id && proof.token == token)
    })
}

/// Whether a parsed proof list contains the expected `(site_id, token)` pair.
pub fn well_known_proofs_match(proofs: &[SiteProof], site_id: &str, token: &str) -> bool {
    proofs
        .iter()
        .any(|proof| proof.site_id == site_id && proof.token == token)
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

/// Registers a new site and returns its random id and one-time claim token.
pub async fn register_site(store: &dyn SiteAuthStore) -> Result<RegisteredSite> {
    let site_id = generate_site_id();
    let claim_token = generate_token();
    store
        .register_site(&site_id, &token_hash(&claim_token))
        .await?;
    Ok(RegisteredSite {
        site_id,
        claim_token,
    })
}

/// Starts a verification challenge for `site_id`, bound to the claim token.
///
/// One raw token is issued and stored (hashed) once per origin; the owner
/// publishes it in every requested location.
pub async fn start_site_verification(
    store: &dyn SiteAuthStore,
    site_id: &SiteId,
    origins: &[Origin],
    methods: &[VerificationMethod],
    claim_token: &str,
) -> Result<VerificationChallenge> {
    if origins.is_empty() {
        bail!("at least one origin is required");
    }
    if methods.is_empty() {
        bail!("at least one verification method is required");
    }

    let stored_hash = store
        .get_claim_token_hash(site_id.as_str())
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "site `{}` is not API-registered; operator-configured sites cannot be \
                 verified through the API",
                site_id.as_str()
            )
        })?;
    if !constant_time_eq(stored_hash.as_bytes(), token_hash(claim_token).as_bytes()) {
        bail!("invalid claim token");
    }

    let raw_token = generate_token();
    let hash = token_hash(&raw_token);
    let expires_at = Utc::now() + Duration::hours(VERIFICATION_TOKEN_TTL_HOURS);
    let tokens = origins
        .iter()
        .map(|origin| NewVerificationToken {
            site_id: site_id.as_str().to_string(),
            origin: origin.clone(),
            token_hash: hash.clone(),
            methods: methods.to_vec(),
            expires_at,
        })
        .collect::<Vec<_>>();
    store.insert_verification_tokens(&tokens).await?;

    Ok(VerificationChallenge {
        site_id: site_id.as_str().to_string(),
        token: raw_token,
        methods: methods.to_vec(),
        origins: origins.to_vec(),
        expires_at,
    })
}

/// Issues (or rotates) the HMAC secret for a verified, API-registered site.
pub async fn issue_site_secret(
    store: &dyn SiteAuthStore,
    site_id: &SiteId,
    claim_token: &str,
    rotate: bool,
) -> Result<IssuedSiteSecret> {
    let stored_hash = store
        .get_claim_token_hash(site_id.as_str())
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "site `{}` is not API-registered; operator-configured sites manage \
                 their secret through configuration",
                site_id.as_str()
            )
        })?;
    if !constant_time_eq(stored_hash.as_bytes(), token_hash(claim_token).as_bytes()) {
        bail!("invalid claim token");
    }

    let auth = store
        .get_site_auth(site_id.as_str())
        .await?
        .ok_or_else(|| anyhow::anyhow!("site `{}` not found", site_id.as_str()))?;
    if auth.verification_status != SiteVerificationStatus::Verified {
        bail!(
            "site `{}` must be verified before a secret can be issued",
            site_id.as_str()
        );
    }
    if auth.secret.is_some() && !rotate {
        bail!(
            "site `{}` already has a secret; set `rotate` to replace it",
            site_id.as_str()
        );
    }

    let secret = generate_token();
    store.store_site_secret(site_id.as_str(), &secret).await?;
    Ok(IssuedSiteSecret {
        site_id: site_id.as_str().to_string(),
        secret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_parse_canonicalizes_and_rejects_garbage() {
        assert_eq!(
            Origin::parse("https://Blog.Example.com").unwrap().as_str(),
            "https://blog.example.com"
        );
        assert_eq!(
            Origin::parse("https://example.com:443").unwrap().as_str(),
            "https://example.com"
        );
        assert_eq!(
            Origin::parse("http://localhost:8080").unwrap().as_str(),
            "http://localhost:8080"
        );
        assert!(Origin::parse("https://example.com/path").is_err());
        assert!(Origin::parse("https://user:pass@example.com").is_err());
        assert!(Origin::parse("ftp://example.com").is_err());
        assert!(Origin::parse("example.com").is_err());
    }

    #[test]
    fn wildcard_pattern_matches_subdomains_but_not_apex() {
        let pattern = OriginPattern::parse("https://*.example.com").unwrap();
        assert!(pattern.matches(&Origin::parse("https://a.example.com").unwrap()));
        assert!(pattern.matches(&Origin::parse("https://a.b.example.com").unwrap()));
        assert!(!pattern.matches(&Origin::parse("https://example.com").unwrap()));
        assert!(!pattern.matches(&Origin::parse("https://a.example.net").unwrap()));
        assert!(!pattern.matches(&Origin::parse("http://a.example.com").unwrap()));
        assert!(!pattern.matches(&Origin::parse("https://a.example.com:8443").unwrap()));

        let with_port = OriginPattern::parse("https://*.example.com:8443").unwrap();
        assert!(with_port.matches(&Origin::parse("https://a.example.com:8443").unwrap()));
        assert!(!with_port.matches(&Origin::parse("https://a.example.com").unwrap()));
    }

    #[test]
    fn hmac_signature_round_trips_and_rejects_tampering() {
        let secret = b"a-site-secret";
        let body = br#"{"content":"hi"}"#;
        let signature = site_request_signature(
            secret,
            "1723456789",
            "POST",
            "/api/v1/sites/x/posts/y/comments",
            body,
        );
        assert!(verify_site_request_signature(
            secret,
            "1723456789",
            "POST",
            "/api/v1/sites/x/posts/y/comments",
            body,
            &signature
        ));
        assert!(!verify_site_request_signature(
            secret,
            "1723456789",
            "POST",
            "/api/v1/sites/x/posts/y/comments",
            br#"{"content":"tampered"}"#,
            &signature
        ));
        assert!(!verify_site_request_signature(
            b"another-secret",
            "1723456789",
            "POST",
            "/api/v1/sites/x/posts/y/comments",
            body,
            &signature
        ));
    }

    #[test]
    fn timestamp_freshness_bounds_skew() {
        let now = Utc::now();
        let ts = now.timestamp().to_string();
        assert!(is_timestamp_fresh(&ts, now, 300));
        assert!(is_timestamp_fresh(
            &(now.timestamp() - 299).to_string(),
            now,
            300
        ));
        assert!(!is_timestamp_fresh(
            &(now.timestamp() - 301).to_string(),
            now,
            300
        ));
        assert!(!is_timestamp_fresh("not-a-number", now, 300));
    }

    #[test]
    fn well_known_parsing_accepts_single_and_list_shapes() {
        let single = r#"{"site_id":"abc","token":"tok"}"#;
        assert_eq!(
            parse_well_known_proofs(single).unwrap(),
            vec![SiteProof {
                site_id: "abc".to_string(),
                token: "tok".to_string()
            }]
        );
        let list =
            r#"{"sites":[{"site_id":"abc","token":"tok"},{"site_id":"def","token":"tok2"}]}"#;
        assert_eq!(parse_well_known_proofs(list).unwrap().len(), 2);
        assert!(parse_well_known_proofs("not json").is_err());
    }

    #[test]
    fn dns_record_parsing_accepts_both_field_orders() {
        let record = "site_id=abc,token=tok";
        assert_eq!(
            parse_dns_proof_record(record),
            Some(SiteProof {
                site_id: "abc".to_string(),
                token: "tok".to_string()
            })
        );
        assert_eq!(
            parse_dns_proof_record("token=tok, site_id=abc"),
            parse_dns_proof_record(record)
        );
        assert_eq!(parse_dns_proof_record("spf=include:example.com"), None);
        assert!(!dns_proofs_match(&[record.to_string()], "abc", "wrong"));
        assert!(dns_proofs_match(&[record.to_string()], "abc", "tok"));
    }

    #[test]
    fn known_secret_placeholders_are_rejected_by_length() {
        for placeholder in KNOWN_SECRET_PLACEHOLDERS {
            assert!(placeholder.len() < SITE_SECRET_MIN_LENGTH);
        }
    }
}
