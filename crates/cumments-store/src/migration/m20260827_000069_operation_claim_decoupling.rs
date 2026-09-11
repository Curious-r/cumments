use sea_orm_migration::prelude::*;

use crate::migration::column_exists;

const OPERATION_CLAIMS: &str = "operation_claims";
const POST_SUBMISSIONS: &str = "post_submissions";
/// Name of the unique index on the durable submission's operation reference.
const POST_OPERATION_INDEX: &str = "idx-post_submissions-operation_id";

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Decouple operation identity from durable submissions.
///
/// An operation claim is a server-wide logical identity and must not require a
/// `post_submissions` row: Create Poll creates a submission, while Vote/End
/// will claim operations that have none. The previous schema stored
/// `operation_claims.submission_id`, which coupled the two. This migration
/// moves that reference onto the durable submission
/// (`post_submissions.operation_id`) and drops the claim's column, preserving
/// all existing claims, submission ids, and the server-wide unique invariant.
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // 1. The durable submission now references its operation.
        if !column_exists(manager, POST_SUBMISSIONS, "operation_id").await? {
            db.execute_unprepared("ALTER TABLE post_submissions ADD COLUMN operation_id TEXT")
                .await?;
        }

        // 2. Preserve the existing coupling as a submission-side reference
        // before removing it from the claim.
        if column_exists(manager, OPERATION_CLAIMS, "submission_id").await? {
            db.execute_unprepared(
                "UPDATE post_submissions \
                 SET operation_id = ( \
                     SELECT operation_id FROM operation_claims \
                     WHERE operation_claims.submission_id = post_submissions.id \
                 ) \
                 WHERE id IN (SELECT submission_id FROM operation_claims)",
            )
            .await?;
            db.execute_unprepared("ALTER TABLE operation_claims DROP COLUMN submission_id")
                .await?;
        }

        // 3. One durable submission per claimed operation, and a lookup path
        // for replay resolution. Multiple `NULL`s (comment submissions) are
        // distinct under a SQLite unique index, so this does not affect them.
        db.execute_unprepared(&format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS \"{POST_OPERATION_INDEX}\" \
             ON post_submissions(operation_id)"
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(&format!("DROP INDEX IF EXISTS \"{POST_OPERATION_INDEX}\""))
            .await?;

        if !column_exists(manager, OPERATION_CLAIMS, "submission_id").await? {
            db.execute_unprepared("ALTER TABLE operation_claims ADD COLUMN submission_id INTEGER")
                .await?;
            if column_exists(manager, POST_SUBMISSIONS, "operation_id").await? {
                db.execute_unprepared(
                    "UPDATE operation_claims \
                     SET submission_id = ( \
                         SELECT id FROM post_submissions \
                         WHERE post_submissions.operation_id = operation_claims.operation_id \
                     )",
                )
                .await?;
            }
        }
        if column_exists(manager, POST_SUBMISSIONS, "operation_id").await? {
            db.execute_unprepared("ALTER TABLE post_submissions DROP COLUMN operation_id")
                .await?;
        }
        Ok(())
    }
}
