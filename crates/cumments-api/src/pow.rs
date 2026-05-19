use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

type HmacSha256 = Hmac<Sha256>;

const CHALLENGE_EXPIRY_SECONDS: u64 = 300; // 5 minutes

#[derive(Clone)]
pub struct Pow {
    secret: String,
    difficulty: u32,
}

#[derive(Debug)]
pub struct Challenge {
    pub prefix: String,
    pub difficulty: u32,
}

impl Pow {
    pub fn new(secret: String, difficulty: u32) -> Self {
        Self { secret, difficulty }
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
        debug!(
            "Verifying PoW: challenge='{}', hash='{}', valid={}",
            challenge, hash_hex, is_valid
        );
        is_valid
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .expect("HMAC can take any key length");
        mac.update(payload.as_bytes());
        let result = mac.finalize().into_bytes();
        URL_SAFE_NO_PAD.encode(result)
    }
}
