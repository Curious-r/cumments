use sea_orm_migration::prelude::*;

pub use sea_orm_migration::MigratorTrait;

pub mod m20260531_000001_initial_schema;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260531_000001_initial_schema::Migration)]
    }
}
