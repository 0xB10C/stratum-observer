use crate::schema::job_updates;
use crate::types::JobUpdate;
use crate::types::JobUpdateJson;
use crate::types::NewJobUpdate;
use async_broadcast::broadcast;
use async_channel::{unbounded, Receiver};
use async_std::task;
use client::{initialize_client, Client};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use env_logger::Env;
use log::{debug, warn};
use std::net::TcpListener;
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
                let client = Client::new(&pool, js.clone()).await;
                initialize_client(client).await;
            }
        });
    }

    let (sqlite_sender, sqlite_receiver) = unbounded();
    task::spawn(async move {
        sqlite_writer_task(sqlite_receiver).await;
    });

    let (websocket_sender, websocket_receiver) = unbounded();
    task::spawn(async move {
        websocket_sender_task(websocket_receiver).await;
    });

    task::block_on(async {
        loop {
            let job = job_receiver.recv().await.unwrap();
            sqlite_sender.send(job.clone()).await.unwrap();
            websocket_sender.send(job).await.unwrap();
        }
    });
}

async fn websocket_sender_task(receiver: Receiver<JobUpdate<'static>>) {
    let server = TcpListener::bind("127.0.0.1:9555").unwrap();

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
                                        if let Err(e) = websocket.send(tungstenite::Message::Text(msg))
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

async fn sqlite_writer_task(receiver: Receiver<JobUpdate<'_>>) {
    let mut conn = SqliteConnection::establish("dev-stratum-observer.sqlite").unwrap();
    conn.run_pending_migrations(MIGRATIONS).unwrap();
    loop {
        let update = receiver.recv().await.unwrap();
        let v: NewJobUpdate = update.into();
        diesel::insert_into(job_updates::table)
            .values(&vec![v])
            .execute(&mut conn)
            .expect("Error saving new job update");
    }
}
