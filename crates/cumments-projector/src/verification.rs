//! Verification of Cumments identity and delete claims embedded in
//! Matrix events.

use cumments_core::identity::{
    derive_visitor_id_from_public_key, signature_message, verify_signature,
};
use cumments_core::protocol::REDACTION_PROOF_KEY;

/// Verify a visitor event's identity claims.
///
/// The sender must be exactly the virtual user derived from the embedded
/// public key for this site, and the Ed25519 signature must cover the
/// canonical Cumments message. Matrix-native senders never pass through this
/// path; `server_name` is required in AppService mode.
pub(crate) fn verify_visitor_event(
    server_name: Option<&str>,
    sender: &str,
    site_id: &str,
    public_key: &str,
    signature: &str,
    message: &str,
) -> bool {
    let Some(visitor_id) = derive_visitor_id_from_public_key(public_key) else {
        return false;
    };
    let Some(server_name) = server_name else {
        return false;
    };
    let expected_sender = format!("@_cumments_{}_{}:{}", site_id, visitor_id, server_name);
    if sender != expected_sender {
        return false;
    }
    verify_signature(public_key, message, signature)
}

/// Verify a Cumments delete proof embedded in a redaction's `reason`.
///
/// The proof is the JSON object the API stores under
/// `host.curious.cumments` when a visitor requests deletion: site/page/target
/// must match the comment being redacted and the Ed25519 signature must cover
/// the canonical DELETE message. Returns `false` for missing or malformed
/// proofs so callers can reject the redaction.
pub(crate) fn verify_delete_proof(
    proof: &serde_json::Value,
    target_event_id: &str,
    site_id: &str,
    page_slug: &str,
    author_public_key: Option<&str>,
) -> bool {
    let Some(block) = proof.get(REDACTION_PROOF_KEY) else {
        return false;
    };
    let field = |key: &str| block.get(key).and_then(|v| v.as_str());
    let (
        Some(proof_site),
        Some(proof_slug),
        Some(proof_target),
        Some(public_key),
        Some(signature),
        Some(challenge),
    ) = (
        field("site_id"),
        field("page_slug"),
        field("target_event_id"),
        field("public_key"),
        field("signature"),
        field("challenge"),
    )
    else {
        return false;
    };

    if proof_site != site_id || proof_slug != page_slug || proof_target != target_event_id {
        return false;
    }
    match author_public_key {
        // A delete proof is only meaningful for visitor comments with a stored
        // public key; without one there is nothing to bind the signature to.
        Some(stored) if stored == public_key => {}
        _ => return false,
    }

    let message = signature_message(&[
        Some("DELETE"),
        Some(site_id),
        Some(page_slug),
        Some(target_event_id),
        Some(challenge),
    ]);
    verify_signature(public_key, &message, signature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visitor_event_verification_accepts_only_expected_virtual_sender() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use cumments_core::identity::post_signature_message;
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let visitor_id = derive_visitor_id_from_public_key(&public_key).expect("visitor id");
        let sender = format!("@_cumments_my-blog_{}:example.com", visitor_id);
        let challenge = "challenge";
        let message = post_signature_message("my-blog", "hello", "content", None, None, challenge);
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());

        assert!(verify_visitor_event(
            Some("example.com"),
            &sender,
            "my-blog",
            &public_key,
            &signature,
            &message,
        ));
        // Wrong server name must fail the sender check.
        assert!(!verify_visitor_event(
            Some("other.example.com"),
            &sender,
            "my-blog",
            &public_key,
            &signature,
            &message,
        ));
        // A sender that does not match the derived virtual user must fail.
        assert!(!verify_visitor_event(
            Some("example.com"),
            "@_cumments_my-blog_0000000000000000:example.com",
            "my-blog",
            &public_key,
            &signature,
            &message,
        ));
        // Without a configured server name there is nothing to bind to.
        assert!(!verify_visitor_event(
            None,
            &sender,
            "my-blog",
            &public_key,
            &signature,
            &message,
        ));
        // Tampered signature must fail.
        assert!(!verify_visitor_event(
            Some("example.com"),
            &sender,
            "my-blog",
            &public_key,
            "AAAA",
            &message,
        ));
    }

    #[test]
    fn delete_proof_verifies_valid_signature_and_rejects_tampering() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        use ed25519_dalek::{Signer, SigningKey};

        let signing_key = SigningKey::from_bytes(&[11u8; 32]);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes());
        let challenge = "challenge";
        let message = signature_message(&[
            Some("DELETE"),
            Some("my-blog"),
            Some("hello"),
            Some("$target:hs"),
            Some(challenge),
        ]);
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(message.as_bytes()).to_bytes());

        let proof = serde_json::json!({
            "host.curious.cumments.redaction": {
                "site_id": "my-blog",
                "page_slug": "hello",
                "target_event_id": "$target:hs",
                "public_key": public_key,
                "signature": signature,
                "challenge": challenge,
            }
        });

        assert!(verify_delete_proof(
            &proof,
            "$target:hs",
            "my-blog",
            "hello",
            Some(&public_key),
        ));
        // Wrong target, site, or stored author key must fail.
        assert!(!verify_delete_proof(
            &proof,
            "$other:hs",
            "my-blog",
            "hello",
            Some(&public_key),
        ));
        assert!(!verify_delete_proof(
            &proof,
            "$target:hs",
            "other",
            "hello",
            Some(&public_key),
        ));
        assert!(!verify_delete_proof(
            &proof,
            "$target:hs",
            "my-blog",
            "hello",
            Some("some-other-key"),
        ));
        // Missing or malformed proofs are rejected.
        assert!(!verify_delete_proof(
            &serde_json::json!({}),
            "$target:hs",
            "my-blog",
            "hello",
            Some(&public_key),
        ));
        assert!(!verify_delete_proof(
            &serde_json::json!({ "host.curious.cumments.redaction": { "site_id": "my-blog" } }),
            "$target:hs",
            "my-blog",
            "hello",
            Some(&public_key),
        ));
    }
}
