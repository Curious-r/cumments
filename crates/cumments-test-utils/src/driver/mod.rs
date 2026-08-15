//! The shared in-memory [`MatrixDriver`] fake.
//!
//! The type and its constructors live here; the single required trait
//! implementation lives in [`matrix`].

mod matrix;

use tokio::sync::Mutex;

/// In-memory [`MatrixDriver`] double that records the calls each test
/// asserts.
///
/// Methods outside the exercised surface panic with `unimplemented!()` so a
/// test that accidentally depends on untracked driver behavior fails loudly
/// instead of silently passing.
pub struct TestDriver {
    pub joined: Mutex<Vec<String>>,
    pub joined_members: Mutex<Vec<String>>,
    pub left: Mutex<Vec<String>>,
    pub left_as: Mutex<Vec<(String, String)>>,
    pub replies: Mutex<Vec<(String, String)>>,
    pub deleted: Mutex<Vec<(String, String)>>,
}

impl TestDriver {
    pub fn new() -> Self {
        Self {
            joined: Mutex::new(Vec::new()),
            joined_members: Mutex::new(Vec::new()),
            left: Mutex::new(Vec::new()),
            left_as: Mutex::new(Vec::new()),
            replies: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
        }
    }

    pub fn with_joined_members(members: Vec<String>) -> Self {
        Self {
            joined_members: Mutex::new(members),
            ..Self::new()
        }
    }
}

impl Default for TestDriver {
    fn default() -> Self {
        Self::new()
    }
}
