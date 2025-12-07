use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

#[derive(Clone)]
pub struct PowGuard {
    secrets: Arc<Mutex<HashMap<String, SystemTime>>>,
}

impl PowGuard {
    pub fn new() -> Self {
        Self {
            secrets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn generate_challenge(&self) -> String {
        let secret = format!("{:x}", rand::random::<u128>());
        let mut map = self.secrets.lock().unwrap();
        map.insert(secret.clone(), SystemTime::now() + Duration::from_secs(300));
        secret
    }

    pub fn verify(&self, secret: &str, nonce: &str) -> bool {
        {
            let mut map = self.secrets.lock().unwrap();
            if let Some(expiry) = map.remove(secret) {
                if SystemTime::now() > expiry {
                    return false;
                }
            } else {
                return false;
            }
        }

        let input = format!("{}{}", secret, nonce);
        let mut hasher = Sha256::new();
        hasher.update(input);
        let result = hex::encode(hasher.finalize());

        result.starts_with("0000")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pow_flow() {
        let guard = PowGuard::new();

        let secret = guard.generate_challenge();
        assert!(!secret.is_empty());

        let difficulty = 4;
        let prefix = "0".repeat(difficulty);
        let mut nonce = 0;
        loop {
            let input = format!("{}{}", secret, nonce);
            let hash = hex::encode(sha2::Sha256::digest(input));
            if hash.starts_with(&prefix) {
                break;
            }
            nonce += 1;
        }

        let nonce_str = nonce.to_string();
        assert!(guard.verify(&secret, &nonce_str));

        assert!(!guard.verify(&secret, "999999999999"));

        assert!(!guard.verify(&secret, &nonce_str));
    }
}
