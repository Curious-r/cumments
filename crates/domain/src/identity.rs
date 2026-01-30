use sha2::{Digest, Sha256};

pub fn compute_fingerprint(email: Option<&str>, guest_token: &str, salt: &str) -> String {
    let seed = if let Some(e) = email {
        format!("email:{}", e.trim().to_lowercase())
    } else {
        format!("token:{}", guest_token)
    };

    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    hasher.update(salt.as_bytes());
    let result = hasher.finalize();

    hex::encode(&result[..6])
}
