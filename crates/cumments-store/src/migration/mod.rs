use sea_orm_migration::prelude::*;

pub use sea_orm_migration::MigratorTrait;

pub mod m20260531_000001_initial_schema;
pub mod m20260619_000002_virtual_users;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260531_000001_initial_schema::Migration),
            Box::new(m20260619_000002_virtual_users::Migration),
        ]
    }
}
