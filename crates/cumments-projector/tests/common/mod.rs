//! Shared test double for projector integration tests.

#[allow(unused_imports)]
pub use cumments_test_utils::TestDriver;

// Shared test support: not every test binary uses every helper.
#[allow(dead_code)]
pub fn test_policy() -> std::sync::Arc<cumments_core::site_auth::SiteAuthPolicy> {
    std::sync::Arc::new(cumments_core::site_auth::SiteAuthPolicy {
        verification: cumments_core::site_auth::SiteVerificationPolicy::Optional,
        sites: Default::default(),
    })
}
