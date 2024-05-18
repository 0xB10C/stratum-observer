use crate::schema::job_updates;
use crate::types::JobUpdate;
use crate::types::JobUpdateJson;
use crate::types::NewJobUpdate;
use async_broadcast::broadcast;
use async_channel::{unbounded, Receiver};
use async_std::task;
use client::Client;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use env_logger::Env;
use log::{debug, error, info, warn};
use std::net::TcpListener;
use std::process::exit;
use tungstenite::accept;

mod client;
mod config;
mod schema;
mod types;
mod utils;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations/");

fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let config = match config::load_config() {
        Ok(c) => c,
        Err(e) => panic!("could not load config: {}", e),
    };

    let (job_sender, job_receiver) = unbounded();

    for pool in config.pools.clone() {
        let js = job_sender.clone();
        task::spawn(async move {
            // reopen clients if a client looses or closes the connection
            loop {
                Client::run(&pool, js.clone()).await;
            }
        });
    }

    let enable_database = config.database_path.is_some();
    let enable_websocket = config.websocket_address.is_some();
    if !enable_database && !enable_websocket {
        warn!("Neither database_path nor websocket_address are set: nothing to do");
        return;
    }

    let (sqlite_sender, sqlite_receiver) = unbounded();
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
    if let Some(db_path) = config.database_path.clone() {
        task::spawn(async move {
            sqlite_writer_task(sqlite_receiver, &db_path).await;
            exit(2)
        });
    } else {
        sqlite_sender.close();
        sqlite_receiver.close();
    }

    task::block_on(async {
        loop {
            match job_receiver.recv().await {
                Ok(job) => {
                    if enable_database {
                        if let Err(e) = sqlite_sender.send(job.clone()).await {
                            error!("could not send a job to the sqlite task: {}", e);
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
    });
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

    task::spawn(async move {
        loop {
            let job = receiver.recv().await.unwrap();
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
                task::spawn(async move {
                    match accept(stream) {
                        Ok(mut websocket) => {
                            debug!(
                                "Accepted new websocket connection: connections={}",
                                r.receiver_count()
                            );
                            loop {
                                let job = r.recv().await.unwrap();
                                match serde_json::to_string::<JobUpdateJson>(&job.clone().into()) {
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
                                        warn!("Could not serialize JobUpdateJson to JSON: {}", e)
                                    }
                                }
                            }
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

async fn sqlite_writer_task(receiver: Receiver<JobUpdate<'_>>, db_path: &str) {
    info!("Opening database at {}", db_path);
    let mut conn = SqliteConnection::establish(db_path).unwrap(); // TODO
    conn.run_pending_migrations(MIGRATIONS).unwrap(); // TODO

    loop {
        let update = receiver.recv().await.unwrap();
        let v: NewJobUpdate = update.into();
        diesel::insert_into(job_updates::table)
            .values(&vec![v])
            .execute(&mut conn)
            .expect("Error saving new job update");
    }
}
