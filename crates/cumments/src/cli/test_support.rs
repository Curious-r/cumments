//! Shared helpers for CLI command tests.

use cumments_core::site_auth::{SiteAuthPolicy, SiteVerificationPolicy};

pub(crate) fn test_db_url(name: &str) -> String {
    let path = std::path::Path::new("/tmp").join(format!(
        "cumments-cli-test-{}-{}.db",
        name,
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    format!("sqlite://{}", path.display())
}

pub(crate) fn test_policy() -> SiteAuthPolicy {
    SiteAuthPolicy {
        verification: SiteVerificationPolicy::Disabled,
        ..Default::default()
    }
}
