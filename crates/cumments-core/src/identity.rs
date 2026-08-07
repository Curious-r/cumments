//! Visitor identity derivation.
//!
//! A visitor holds a **secret token** (the `author_fingerprint` sent in API
//! requests). From that token we derive two values:
//!
//! - `visitor_id`: a short public identifier, safe to expose in API responses
//!   and Matrix events. It matches the hash segment of the virtual user ID
//!   (`@_cumments_{site}_{visitor_id}:...`).
//! - `token_hash`: a non-reversible verifier keyed by the server's identity
//!   salt, stored locally so edit/delete requests can be authorized without
//!   keeping or exposing the raw token.

use sha2::{Digest, Sha256};

/// Public, stable identifier derived from a visitor's secret token.
///
/// First 4 bytes of SHA-256, hex-encoded (8 chars) – same derivation used for
/// the virtual user ID.
pub fn derive_visitor_id(token: &str) -> String {
    let hash = Sha256::digest(token.as_bytes());
    hex::encode(&hash[..4])
}

/// Non-reversible verifier for a visitor token, keyed by `identity_salt`.
///
/// SHA-256 over `"{salt}:{token}"`. The raw token must never be stored or
/// exposed; only this hash is persisted (and optionally transported).
pub fn derive_token_hash(identity_salt: &str, token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity_salt.as_bytes());
    hasher.update(b":");
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visitor_id_is_stable_and_short() {
        let a = derive_visitor_id("some-random-token");
        let b = derive_visitor_id("some-random-token");
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        assert_ne!(a, derive_visitor_id("some-other-token"));
    }

    #[test]
    fn token_hash_is_keyed_by_salt() {
        let token = "some-random-token";
        assert_eq!(
            derive_token_hash("salt-a", token),
            derive_token_hash("salt-a", token)
        );
        assert_ne!(
            derive_token_hash("salt-a", token),
            derive_token_hash("salt-b", token)
        );
    }
}
