use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

type HmacSha256 = Hmac<Sha256>;

const CHALLENGE_EXPIRY_SECONDS: u64 = 300; // 5 minutes

#[derive(Clone)]
pub struct Pow {
    secret: String,
    difficulty: u32,
    /// Challenges already used for a successful PoW, mapped to their expiry
    /// timestamp (seconds). Keeps a signed challenge single-use so the same
    /// request body cannot be replayed within the expiry window.
    used_challenges: Arc<Mutex<HashMap<String, u64>>>,
}

#[derive(Debug)]
pub struct Challenge {
    pub prefix: String,
    pub difficulty: u32,
}

impl Pow {
    pub fn new(secret: String, difficulty: u32) -> Self {
        Self {
            secret,
            difficulty,
            used_challenges: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Generates a new PoW challenge.
    /// The challenge is a signed string containing a timestamp and random bytes.
    pub fn generate_challenge(&self) -> Challenge {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut random_bytes = [0u8; 8];
        rand::rng().fill_bytes(&mut random_bytes);

        let payload = format!("{:x}.{}", now, hex::encode(random_bytes));
        let signature = self.sign(&payload);

        let prefix = format!("{}.{}", payload, signature);

        debug!("Generated new PoW challenge: {}", prefix);
        Challenge {
            prefix,
            difficulty: self.difficulty,
        }
    }

    /// Verifies a PoW response.
    /// The response is expected to be in the format "challenge|nonce".
    pub fn verify(&self, response: &str) -> bool {
        let parts: Vec<&str> = response.split('|').collect();
        if parts.len() != 2 {
            debug!("Invalid PoW response format: expected 'challenge|nonce'");
            return false;
        }

        let challenge = parts[0];
        let nonce = parts[1];

        // 1. Verify the challenge signature and timestamp
        let challenge_parts: Vec<&str> = challenge.split('.').collect();
        if challenge_parts.len() != 3 {
            debug!("Invalid challenge format in PoW response");
            return false;
        }

        let ts_hex = challenge_parts[0];
        let rnd_hex = challenge_parts[1];
        let sig_provided = challenge_parts[2];

        // Check timestamp
        if let Ok(ts) = u64::from_str_radix(ts_hex, 16) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if now > ts + CHALLENGE_EXPIRY_SECONDS {
                debug!("Expired PoW challenge");
                return false;
            }
        } else {
            debug!("Invalid timestamp hex in challenge");
            return false;
        }

        // Verify signature
        let payload = format!("{}.{}", ts_hex, rnd_hex);
        let sig_expected = self.sign(&payload);
        if sig_provided != sig_expected {
            debug!("Invalid challenge signature");
            return false;
        }

        // 2. Check hash: Hash(challenge + nonce)
        let input = format!("{}{}", challenge, nonce);
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        let hash = hasher.finalize();

        let hash_hex = hex::encode(hash);
        let required_prefix = "0".repeat(self.difficulty as usize);

        let is_valid = hash_hex.starts_with(&required_prefix);
        if !is_valid {
            return false;
        }

        // Mark the challenge as used (single-use within its expiry window).
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut used = self
            .used_challenges
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if used.len() > 1024 {
            used.retain(|_, expiry| *expiry > now);
        }
        if let Some(expiry) = used.get(challenge)
            && *expiry > now
        {
            debug!("Rejected replayed PoW challenge");
            return false;
        }
        used.insert(challenge.to_string(), now + CHALLENGE_EXPIRY_SECONDS);

        debug!(
            "Verifying PoW: challenge='{}', hash='{}', valid={}",
            challenge, hash_hex, is_valid
        );
        true
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .expect("HMAC can take any key length");
        mac.update(payload.as_bytes());
        let result = mac.finalize().into_bytes();
        URL_SAFE_NO_PAD.encode(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_response(pow: &Pow) -> String {
        let challenge = pow.generate_challenge();
        // difficulty is 0 in tests, so any nonce passes the hash check.
        format!("{}|0", challenge.prefix)
    }

    #[test]
    fn valid_challenge_passes() {
        let pow = Pow::new("test-secret".into(), 0);
        assert!(pow.verify(&valid_response(&pow)));
    }

    #[test]
    fn replayed_challenge_is_rejected() {
        let pow = Pow::new("test-secret".into(), 0);
        let response = valid_response(&pow);
        assert!(pow.verify(&response));
        assert!(
            !pow.verify(&response),
            "the same challenge+nonce must not be accepted twice"
        );
    }

    #[test]
    fn used_challenge_with_different_nonce_is_rejected() {
        let pow = Pow::new("test-secret".into(), 0);
        let response = valid_response(&pow);
        assert!(pow.verify(&response));
        let mut parts: Vec<&str> = response.split('|').collect();
        parts[1] = "1";
        assert!(
            !pow.verify(&parts.join("|")),
            "a used challenge must stay single-use even with a different nonce"
        );
    }
}
