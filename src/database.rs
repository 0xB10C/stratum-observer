use crate::schema::job_updates;
use crate::types::JobUpdate;
use crate::types::NewJobUpdate;
use async_channel::Receiver;
use async_std::task;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use log::{debug, error, info};
use std::time::Duration;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations/");

pub async fn db_writer_task(receiver: Receiver<JobUpdate<'_>>, db_url: &str) {
    info!("Started database writer task with database at {}", db_url);
    let mut first = true;
    loop {
        if !first {
            let mut discarded_jobs = 0;
            while !receiver.is_empty() {
                discarded_jobs += 1;
                let _ = receiver.recv().await;
            }
            debug!("discarded {} jobs from the database writer channel to avoid it becoming too full..", discarded_jobs);
            task::sleep(Duration::from_secs(3)).await;
        }
        first = false;

        let mut conn = match PgConnection::establish(&db_url) {
            Ok(conn) => conn,
            Err(e) => {
                error!("Could not connect to database: {}", e);
                continue;
            }
        };

        match conn.run_pending_migrations(MIGRATIONS) {
            Ok(_) => debug!("Ran database migrations"),
            Err(e) => {
                error!("Could not run database migrations: {}", e);
                continue;
            }
        }

        loop {
            let update = match receiver.recv().await {
                Ok(update) => update,
                Err(e) => {
                    panic!("Could not receive job from database receiver: {}", e);
                }
            };

            let v: NewJobUpdate = update.into();
            if let Err(e) = diesel::insert_into(job_updates::table)
                .values(&vec![v])
                .execute(&mut conn)
            {
                error!("Could not insert job into database: {}", e);
                break;
            }
        }
    }
}
