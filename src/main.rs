use async_channel::unbounded;
use async_std::task;
use env_logger::Env;
use log::{debug, error, info, warn};
use std::process::exit;
use stratum_observer::client::Client;
use stratum_observer::config;
use stratum_observer::database::{db_writer_task, DEFAULT_RETRY_DELAY};
use stratum_observer::websocket::websocket_sender_task;

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
        warn!("Neither postgresql_url nor websocket_address are set: nothing to do");
        exit(3)
    }

    let (db_sender, db_receiver) = unbounded();
    let (websocket_sender, websocket_receiver) = unbounded();

    // websocket sender task
    if let Some(ws_addr) = config.websocket_address.clone() {
        task::spawn(async move {
            if let Err(e) = websocket_sender_task(websocket_receiver, &ws_addr).await {
                error!("Could not start the websocket server on {}: {}", ws_addr, e);
            }
            exit(1)
        });
    } else {
        websocket_sender.close();
        websocket_receiver.close();
    }

    // database writer task
    if let Some(db_url) = config.postgresql_url.clone() {
        task::spawn(async move {
            db_writer_task(db_receiver, &db_url, DEFAULT_RETRY_DELAY).await;
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
