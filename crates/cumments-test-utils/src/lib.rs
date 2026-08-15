//! Shared test doubles for workspace tests.
//!
//! Kept in a dedicated crate so unit and integration tests across the
//! workspace exercise the same fakes instead of maintaining near-identical
//! copies per test module.

mod driver;

pub use driver::TestDriver;
