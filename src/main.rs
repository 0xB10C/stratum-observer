use crate::schema::job_updates;
use crate::types::JobUpdate;
use crate::types::JobUpdateJson;
use crate::types::NewJobUpdate;
use async_broadcast::broadcast;
use async_channel::{unbounded, Receiver};
use async_std::sync::Arc;
use async_std::sync::RwLock;
use async_std::task;
use client::Client;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use env_logger::Env;
use log::{debug, error, info, warn};
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::process::exit;
use std::time::Duration;
use tungstenite::accept;

mod client;
mod config;
mod schema;
mod types;
mod utils;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations/");

#[async_std::main]
async fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let config = match config::load_config() {
        Ok(c) => c,
        Err(e) => panic!("could not load config: {}", e),
    };

    let (job_sender, job_receiver) = unbounded();

    for pool in config.pools.clone() {
        let js = job_sender.clone();
        task::spawn(async move {
            debug!("Spawned task for pool: '{}'", pool.name);
            // reopen the connection when a client or we close the connection
            loop {
                Client::run(&pool, js.clone()).await;
            }
        });
    }

    let enable_database = config.postgresql_url.is_some();
    let enable_websocket = config.websocket_address.is_some();
    if !enable_database && !enable_websocket {
        warn!("Neither database_path nor websocket_address are set: nothing to do");
        exit(3)
    }

    let (db_sender, db_receiver) = unbounded();
    let (websocket_sender, websocket_receiver) = unbounded();

    // websocket sender task
    if let Some(ws_addr) = config.websocket_address.clone() {
        task::spawn(async move {
            websocket_sender_task(websocket_receiver, &ws_addr).await;
            exit(1)
        });
    } else {
        websocket_sender.close();
        websocket_receiver.close();
    }

    // database writer task
    if let Some(db_url) = config.postgresql_url.clone() {
        task::spawn(async move {
            db_writer_task(db_receiver, &db_url).await;
            exit(2)
        });
    } else {
        db_sender.close();
        db_receiver.close();
    }

    // main task
    // handles new jobs
    task::spawn(async move {
        loop {
            match job_receiver.recv().await {
                Ok(job) => {
                    if enable_database {
                        if let Err(e) = db_sender.send(job.clone()).await {
                            error!("could not send a job to the database task: {}", e);
                            break;
                        }
                    }
                    if enable_websocket {
                        if let Err(e) = websocket_sender.send(job).await {
                            error!("could not send a job to the websocket task: {}", e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("could not receive a new job in main thread: {}", e);
                    break;
                }
            }
        }
        info!("main loop exited..")
    })
    .await;
}

async fn websocket_sender_task(receiver: Receiver<JobUpdate<'static>>, ws_addr: &str) {
    info!("Starting websocket server on {}", ws_addr);
    let server = match TcpListener::bind(ws_addr) {
        Ok(s) => s,
        Err(e) => {
            error!("Could not start websocket server on {}: {}", ws_addr, e);
            return;
        }
    };

    // a broadcast channel is used to fan-out the jobs to all websocket
    // subscribers. One 'inactive' receiver is just used for cloning new
    // broadcast receivers. The sender overflows to make sure we allways
    // empty the websocket_receiver channel, even when no active websocket
    // connections are avaliable.
    let (mut sender, broadcast_receiver) = broadcast(10);
    let inactive_broadcast_receiver = broadcast_receiver.deactivate();
    sender.set_overflow(true);
    let recent_jobs: BTreeMap<String, JobUpdate> = BTreeMap::new();
    let recent_jobs_rwl_arc = Arc::new(RwLock::new(recent_jobs));

    let recent_jobs_write = recent_jobs_rwl_arc.clone();
    task::spawn(async move {
        loop {
            let job = receiver.recv().await.unwrap();
            {
                // store the most recent job in a map to be able to send
                // it to newly connecting clients
                let mut recent_jobs_ = recent_jobs_write.write().await;
                recent_jobs_.insert(job.pool.name.clone(), job.clone());
            }
            if sender.receiver_count() > 0 {
                sender.broadcast(job.clone()).await.unwrap();
                debug!(
                    "broadcast new job by '{}' to {} websocket thread(s)",
                    job.pool.name,
                    sender.receiver_count()
                );
            }
        }
    });

    for stream in server.incoming() {
        match stream {
            Ok(stream) => {
                let mut r = inactive_broadcast_receiver.clone().activate();
                let recent_jobs_read = recent_jobs_rwl_arc.clone();
                task::spawn(async move {
                    match accept(stream) {
                        Ok(mut websocket) => {
                            debug!(
                                "Accepted new websocket connection: connections={}",
                                r.receiver_count()
                            );

                            // send recent jobs when websocket is opened
                            {
                                let recent_jobs_ = recent_jobs_read.read().await;
                                for job in recent_jobs_.values() {
                                    // TODO: deduplicate sending jobs into websocket code
                                    match serde_json::to_string::<JobUpdateJson>(
                                        &job.clone().into(),
                                    ) {
                                        Ok(msg) => {
                                            if let Err(e) =
                                                websocket.send(tungstenite::Message::Text(msg))
                                            {
                                                debug!("Could not send '{}' job update to websocket: {}. Connection probably closed.", job.pool.name, e);
                                                // Try our best to close and flush the websocket. If we can't,
                                                // we can't..
                                                let _ = websocket.close(None);
                                                let _ = websocket.flush();
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Could not serialize JobUpdateJson to JSON: {}",
                                                e
                                            )
                                        }
                                    }
                                }
                            }

                            // continuesly send new jobs
                            loop {
                                let job = match r.recv().await {
                                    Ok(j) => j,
                                    Err(e) => {
                                        warn!(
                                            "Could not receive broadcast-job for websocket: {}",
                                            e
                                        );
                                        break;
                                    }
                                };
                                // TODO: deduplicate sending jobs into websocket code
                                match serde_json::to_string::<JobUpdateJson>(&job.clone().into()) {
                                    Ok(msg) => {
                                        if let Err(e) =
                                            websocket.send(tungstenite::Message::Text(msg))
                                        {
                                            debug!("Could not send '{}' job update to websocket: {}. Connection probably closed.", job.pool.name, e);
                                            // Try our best to close and flush the websocket. If we can't,
                                            // we can't..
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Could not serialize JobUpdateJson to JSON: {}", e)
                                    }
                                }
                            }
                            let _ = websocket.close(None);
                            let _ = websocket.flush();
                        }
                        Err(e) => {
                            warn!("Failed to open websocket on incoming connection: {}", e);
                        }
                    }
                });
            }
            Err(e) => {
                warn!("Failed to accept incomming TCP connection: {}", e);
            }
        }
    }
}

async fn db_writer_task(receiver: Receiver<JobUpdate<'_>>, db_url: &str) {
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
