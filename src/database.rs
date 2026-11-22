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
pub const DISCARD_BACKLOG_AFTER_ATTEMPTS: u32 = 5;

/// Tells whether the error means the connection to the database is gone. A new
/// connection can fix that, while all other errors are about the row itself.
fn is_connection_error(e: &DieselError) -> bool {
    matches!(
        e,
        DieselError::DatabaseError(DatabaseErrorKind::ClosedConnection, _)
            | DieselError::DatabaseError(DatabaseErrorKind::UnableToSendCommand, _)
    )
}

/// Throws away everything that waits in the channel and says how much that
/// was.
async fn discard_backlog<T>(receiver: &Receiver<T>) -> u32 {
    let mut discarded = 0;
    while !receiver.is_empty() {
        discarded += 1;
        let _ = receiver.recv().await;
    }
    discarded
}

/// Opens a connection to the database and brings its tables up to date. Says
/// what went wrong to the log and hands back nothing when either of the two
/// does not work: both mean we have no database to write to for now.
fn connect_and_migrate(db_url: &str) -> Option<PgConnection> {
    let mut conn = match PgConnection::establish(db_url) {
        Ok(conn) => conn,
        Err(e) => {
            error!("Could not connect to database: {}", e);
            return None;
        }
    };
    match conn.run_pending_migrations(MIGRATIONS) {
        Ok(_) => debug!("Ran database migrations"),
        Err(e) => {
            error!("Could not run database migrations: {}", e);
            return None;
        }
    }
    Some(conn)
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
                let discarded_jobs = discard_backlog(&receiver).await;
                debug!("discarded {} jobs from the database writer channel to avoid it becoming too full..", discarded_jobs);
            }
            task::sleep(retry_delay).await;
        }

        if receiver.is_closed() && receiver.is_empty() {
            info!("Nobody sends jobs to the database anymore, stopping");
            return;
        }

        let mut conn = match connect_and_migrate(db_url) {
            Some(conn) => conn,
            None => {
                failed_attempts += 1;
                continue;
            }
        };

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
                    // The connection worked until now, so this is the first
                    // failed attempt of a new row of them: we wait once before
                    // opening the next connection, and the counting above
                    // takes over if that one fails too.
                    failed_attempts = 1;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An error of the given kind, the way diesel hands on what PostgreSQL
    /// reported. The message is the one a terminated connection gives.
    fn database_error(kind: DatabaseErrorKind) -> DieselError {
        DieselError::DatabaseError(
            kind,
            Box::new("server closed the connection unexpectedly".to_string()),
        )
    }

    /// Only a connection that is gone is worth opening again. Mistaking a row
    /// the database refuses for a lost connection would mean reconnecting over
    /// and over, and taking a lost connection for a bad row would mean
    /// throwing away every job from then on.
    #[test]
    fn a_lost_connection_is_told_apart_from_a_refused_row() {
        assert!(is_connection_error(&database_error(
            DatabaseErrorKind::ClosedConnection
        )));
        assert!(is_connection_error(&database_error(
            DatabaseErrorKind::UnableToSendCommand
        )));

        assert!(!is_connection_error(&database_error(
            DatabaseErrorKind::UniqueViolation
        )));
        assert!(!is_connection_error(&database_error(
            DatabaseErrorKind::NotNullViolation
        )));
        assert!(!is_connection_error(&database_error(
            DatabaseErrorKind::Unknown
        )));
        assert!(!is_connection_error(&DieselError::NotFound));
    }

    #[async_std::test]
    async fn throwing_away_the_backlog_empties_the_channel() {
        let (sender, receiver) = async_channel::unbounded();
        for job in 0..3 {
            sender.send(job).await.expect("the channel takes the job");
        }

        assert_eq!(discard_backlog(&receiver).await, 3);
        assert!(receiver.is_empty(), "nothing should be left in the channel");
        // Nothing waiting is nothing to throw away.
        assert_eq!(discard_backlog(&receiver).await, 0);
    }
}
