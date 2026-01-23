use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
type HmacSha256 = Hmac<Sha256>;
#[derive(Clone)]
pub struct PowGuard {
    secret: String,
}
impl PowGuard {
    pub fn new(secret: String) -> Self {
        Self { secret }
    }
    pub fn generate_challenge(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("SystemTime should be available since UNIX_EPOCH")
            .as_secs();
        let mut random_bytes = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut random_bytes);
        let payload = format!("{:x}.{}", now, hex::encode(random_bytes));
        let signature = self.sign(&payload);
        format!("{}.{}", payload, signature)
    }
    pub fn verify(&self, secret_token: &str, nonce: &str) -> bool {
        let parts: Vec<&str> = secret_token.split('.').collect();
        if parts.len() != 3 {
            return false;
        }
        let (ts_hex, rnd_hex, sig_provided) = (parts[0], parts[1], parts[2]);
        if let Ok(ts) = u64::from_str_radix(ts_hex, 16) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("SystemTime should be available since UNIX_EPOCH")
                .as_secs();
            if ts > now + 30 || now - ts > 300 {
                return false;
            }
        } else {
            return false;
        }
        let payload = format!("{}.{}", ts_hex, rnd_hex);
        let sig_expected = self.sign(&payload);
        if sig_provided != sig_expected {
            return false;
        }
        let input = format!("{}{}", secret_token, nonce);
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        let result = hex::encode(hasher.finalize());
        result.starts_with("0000")
    }
    fn sign(&self, payload: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .expect("HMAC can take any key length for the secret");
        mac.update(payload.as_bytes());
        let result = mac.finalize().into_bytes();
        URL_SAFE_NO_PAD.encode(result)
    }
}
