use rand::Rng;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

const CHALLENGE_PREFIX_BYTES: usize = 8;
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
    pub fn generate_challenge(&self) -> Challenge {
        let mut rng = rand::rng();
        let prefix: String = (0..CHALLENGE_PREFIX_BYTES)
            .map(|_| rng.random::<char>())
            .collect();
        debug!("Generated new PoW challenge with prefix '{}'", prefix);
        Challenge {
            prefix,
            difficulty: self.difficulty,
        }
    }

    /// Verifies a PoW response.
    /// The response is expected to be in the format "timestamp|prefix|nonce".
    pub fn verify(&self, response: &str) -> bool {
        let parts: Vec<&str> = response.split('|').collect();
        if parts.len() != 3 {
            debug!("Invalid PoW response format");
            return false;
        }

        let timestamp_str = parts[0];
        let prefix = parts[1];
        let nonce = parts[2];

        // 1. Check timestamp to prevent replay attacks
        let timestamp = match timestamp_str.parse::<u64>() {
            Ok(ts) => ts,
            Err(_) => {
                debug!("Invalid timestamp in PoW response");
                return false;
            }
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if now > timestamp + CHALLENGE_EXPIRY_SECONDS {
            debug!("Expired PoW challenge");
            return false;
        }

        // 2. Check hash
        let mut hasher = Sha256::new();
        hasher.update(self.secret.as_bytes());
        hasher.update(timestamp_str.as_bytes());
        hasher.update(prefix.as_bytes());
        hasher.update(nonce.as_bytes());
        let hash = hasher.finalize();

        let hash_hex = hex::encode(hash);
        let required_prefix = "0".repeat(self.difficulty as usize);

        let is_valid = hash_hex.starts_with(&required_prefix);
        debug!(
            "Verifying PoW: response='{}', hash='{}', valid={}",
            response, hash_hex, is_valid
        );
        is_valid
    }
}
