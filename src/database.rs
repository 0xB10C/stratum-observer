use crate::schema::job_updates;
use crate::types::JobUpdate;
use crate::types::NewJobUpdate;
use async_channel::Receiver;
use async_std::task;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::result::{DatabaseErrorKind, Error as DieselError};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use log::{debug, error, info};
use std::time::Duration;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations/");

/// How long to wait before trying to reach the database again.
pub const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(3);

/// After this many failed attempts to reach the database, the jobs waiting in
/// the channel are thrown away. The database has been gone for a while by
/// then and the channel would otherwise grow without bounds.
const DISCARD_BACKLOG_AFTER_ATTEMPTS: u32 = 5;

/// Tells whether the error means the connection to the database is gone. A new
/// connection can fix that, while all other errors are about the row itself.
fn is_connection_error(e: &DieselError) -> bool {
    matches!(
        e,
        DieselError::DatabaseError(DatabaseErrorKind::ClosedConnection, _)
            | DieselError::DatabaseError(DatabaseErrorKind::UnableToSendCommand, _)
    )
}

pub async fn db_writer_task(
    receiver: Receiver<JobUpdate<'_>>,
    db_url: &str,
    retry_delay: Duration,
) {
    info!("Started database writer task with database at {}", db_url);
    let mut failed_attempts = 0;
    loop {
        if failed_attempts > 0 {
            if failed_attempts >= DISCARD_BACKLOG_AFTER_ATTEMPTS {
                let mut discarded_jobs = 0;
                while !receiver.is_empty() {
                    discarded_jobs += 1;
                    let _ = receiver.recv().await;
                }
                debug!("discarded {} jobs from the database writer channel to avoid it becoming too full..", discarded_jobs);
            }
            task::sleep(retry_delay).await;
        }

        if receiver.is_closed() && receiver.is_empty() {
            info!("Nobody sends jobs to the database anymore, stopping");
            return;
        }

        let mut conn = match PgConnection::establish(db_url) {
            Ok(conn) => conn,
            Err(e) => {
                failed_attempts += 1;
                error!("Could not connect to database: {}", e);
                continue;
            }
        };

        match conn.run_pending_migrations(MIGRATIONS) {
            Ok(_) => debug!("Ran database migrations"),
            Err(e) => {
                failed_attempts += 1;
                error!("Could not run database migrations: {}", e);
                continue;
            }
        }
        failed_attempts = 0;

        loop {
            let update = match receiver.recv().await {
                Ok(update) => update,
                Err(e) => {
                    info!("Stopped writing jobs to the database: {}", e);
                    return;
                }
            };

            let v: NewJobUpdate = update.into();
            if let Err(e) = diesel::insert_into(job_updates::table)
                .values(&vec![v])
                .execute(&mut conn)
            {
                if is_connection_error(&e) {
                    error!("Lost the connection to the database while inserting a job, reconnecting: {}", e);
                    failed_attempts += 1;
                    break;
                }
                // Something is wrong with this job, so it is the only one we
                // give up on. The jobs waiting behind it are still fine.
                error!(
                    "Could not insert a job into the database, dropping it: {}",
                    e
                );
            }
        }
    }
}
