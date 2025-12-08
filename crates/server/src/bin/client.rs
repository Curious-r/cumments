use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Instant;

use domain::Comment;

const BASE_URL: &str = "http://127.0.0.1:3000";
const SITE_ID: &str = "demo.example";
const SLUG: &str = "hello_cumments";

#[derive(Serialize)]
struct CreateCommentRequest {
    post_slug: String,
    content: String,
    nickname: String,
    challenge_response: String,
    reply_to: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    println!("Starting Cumments test client...");

    println!("\n[1/4] Fetching PoW challenge...");
    let challenge_url = format!("{}/api/challenge", BASE_URL);
    let resp: Value = client.get(&challenge_url).send().await?.json().await?;

    let secret = resp["secret"].as_str().expect("Missing secret");
    let difficulty = resp["difficulty"].as_u64().expect("Missing difficulty") as usize;
    println!("   -> Secret: {}", secret);
    println!("   -> Difficulty: {} leading zeros", difficulty);

    println!("\n[2/4] Mining (computing SHA256)...");
    let start = Instant::now();
    let (nonce, hash) = solve_pow(secret, difficulty);
    let duration = start.elapsed();
    println!("   -> Success! Nonce: {}", nonce);
    println!("   -> Hash: {}", hash);
    println!("   -> Duration: {:.2?}", duration);

    println!("\n[3/4] Submitting comment...");
    let proof = format!("{}|{}", secret, nonce);

    let payload = CreateCommentRequest {
        post_slug: SLUG.to_string(),
        content: "This is a message from Cumments Test Client!".to_string(),
        nickname: "Ferris".to_string(),
        challenge_response: proof,
        reply_to: None,
    };

    let post_url = format!("{}/api/{}/comments", BASE_URL, SITE_ID);
    let resp = client.post(&post_url).json(&payload).send().await?;

    if resp.status().is_success() {
        println!("   -> ✅ Sent successfully!");
    } else {
        println!("   -> ❌ Failed to send: {:?}", resp.text().await?);
        return Ok(());
    }

    println!("\n[4/4] Waiting 3 second before fetching comments list...");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let list_url = format!("{}/api/{}/comments/{}", BASE_URL, SITE_ID, SLUG);
    let comments: Vec<Comment> = client.get(&list_url).send().await?.json().await?;

    println!("   -> Retrieved {} comment(s):", comments.len());
    for c in comments {
        println!(
            "      - [{}] {}: {}",
            c.created_at, c.author_name, c.content
        );
    }

    Ok(())
}

fn solve_pow(secret: &str, difficulty: usize) -> (u64, String) {
    let prefix = "0".repeat(difficulty);
    let mut nonce = 0u64;
    let mut hasher = Sha256::new();

    loop {
        hasher.update(format!("{}{}", secret, nonce));
        let result = hasher.finalize_reset();
        let hex_string = hex::encode(result);

        if hex_string.starts_with(&prefix) {
            return (nonce, hex_string);
        }
        nonce += 1;
    }
}
