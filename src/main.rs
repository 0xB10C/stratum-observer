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
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use env_logger::Env;
use log::{debug, error, info, warn};
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::process::exit;
use tungstenite::accept;
use std::time::Duration;

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

    let enable_database = config.database_path.is_some();
    let enable_websocket = config.websocket_address.is_some();
    if !enable_database && !enable_websocket {
        warn!("Neither database_path nor websocket_address are set: nothing to do");
        exit(3)
    }

    let (sqlite_sender, sqlite_receiver) = unbounded();
    let (websocket_sender, websocket_receiver) = unbounded();
    let (smd_sender, smd_receiver) = unbounded();

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
    
    // selfish mining detection task
    task::spawn(async move {
        selfish_mining_detection_task(smd_receiver).await;
        exit(4)
    });

    // main task
    // handles new jobs
    task::spawn(async move {
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
                        if let Err(e) = websocket_sender.send(job.clone()).await {
                            error!("could not send a job to the websocket task: {}", e);
                            break;
                        }
                    }
                    // TODO: enable smd 
                    if let Err(e) = smd_sender.send(job).await {
                        error!("could not send a job to the selfish mining task: {}", e);
                        break;
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
                                let job = r.recv().await.unwrap(); // TODO
                                                                   // TODO: deduplicate sending jobs into websocket code
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

async fn selfish_mining_detection_task(receiver: Receiver<JobUpdate<'static>>) {
    info!("Starting selfish mining detection task");

    // we only want to consider jobs that are older than `min_job_age` seconds
    // to make sure we don't consider someone who just found a block as selfish
    // mining when other pools haven't had a chance to learn about it.
    // Assumption: The majority of pools should switch to a new block in 5 seconds.
    let min_job_age: i64 = 5;

    let recent_jobs: BTreeMap<String, JobUpdate> = BTreeMap::new();
    let recent_jobs_rwl_arc = Arc::new(RwLock::new(recent_jobs));

    let recent_jobs_write = recent_jobs_rwl_arc.clone();
    task::spawn(async move {
        loop {
            let job = receiver.recv().await.unwrap();
            {
                let mut recent_jobs_ = recent_jobs_write.write().await;
                recent_jobs_.insert(job.pool.name.clone(), job.clone());
            }
        }
    });

    let recent_jobs_read = recent_jobs_rwl_arc.clone();
    loop {
        task::sleep(Duration::from_secs(2)).await;
        {        
            let recent_jobs_ = recent_jobs_read.read().await;
            let mut max_height = 0;
            let mut height_occurences: BTreeMap<u32, u16> = BTreeMap::new();
            for height in recent_jobs_.values().filter(|job| job.age() > min_job_age).map(|job_update| job_update.coinbase_info().height) {
                if height > max_height {
                    max_height = height;
                }
                height_occurences.entry(height).and_modify(|occurences| {*occurences += 1} ).or_insert(1);
            }
        
            println!("occurences={:?}, max={}", height_occurences, max_height);
            // find out what the majority of pools mines on
            let mut majority_size = 0;
            let mut majority_height = 0;
            for (height, size) in height_occurences.iter() {
                if *size > majority_size {
                    majority_height = *height;
                    majority_size = *size;
                }
            }
            if majority_height < max_height {
                warn!("max_height={} while majority_height={}", max_height, majority_height);
            }
        }        
    }
}


async fn selfish_mining_detection_task2(receiver: Receiver<JobUpdate<'static>>) {
    info!("Starting selfish mining detection task");

    // We want to detect selfish mining. A pool or a group of pools are
    // selfish mining when they don't publish recently mined blocks and
    // keep building on top of the unpublished blocks.
    //
    // To detect this, we can look for pools that mine blocks on a 
    // slighly higher height than other pools. We don't care about pools
    // mining on the same height but a different prev_hash. While this
    // might happen during selfish mining (a competing pool catches up),
    // the selfish-mining pool usualy mined on a higher height before.
    // Starting selfish mining on an older block does not make sense. 
    
    // Keep track of which height and prev_hash pools are mining on.
    
        


// SELECT 
// 	min(timestamp) as start, pool_name, height, prev_hash
// FROM
// 	job_updates
// WHERE 
// 	height > 0 AND pool_name != 'NiceHash ASICBoost'
// GROUP BY
// 	pool_name, height, prev_hash
// ORDER BY
// 	start ASC;





// WITH ValueChanges AS (
//     SELECT
//         timestamp,
//         pool_name,
//         height,
// 	prev_hash,
//         LAG(height) OVER (PARTITION BY pool_name ORDER BY timestamp) AS previous_height
//     FROM
//         job_updates
//     WHERE pool_name != 'NiceHash ASICBoost'
// )
// SELECT
//     timestamp,
//     pool_name,
//     height,
//     prev_hash
// FROM
//     ValueChanges
// WHERE
//     height <> previous_height OR previous_height IS NULL
// ORDER BY
//     timestamp ASC;


// WITH dup AS (
// 	WITH HeightHashPairs AS (
// 	    	SELECT
// 			height, prev_hash
// 		FROM
// 			job_updates
// 		WHERE
// 				height > 0
// 			AND 
// 				pool_name != 'NiceHash ASICBoost'
// 		GROUP BY
// 			height, prev_hash
// 	)
// 	SELECT height
// 	FROM HeightHashPairs
// 	GROUP BY height
// 	HAVING count(height) > 1
// )
// SELECT DISTINCT height, prev_hash, pool_name
// FROM job_updates
// WHERE height IN dup;





    // we only want to consider jobs that are older than `min_job_age` seconds
    // to make sure we don't consider someone who just found a block as selfish
    // mining when other pools haven't had a chance to learn about it.
    // Assumption: The majority of pools should switch to a new block in 5 seconds.
    let min_job_age: i64 = 5;

    let recent_jobs: BTreeMap<String, JobUpdate> = BTreeMap::new();
    let recent_jobs_rwl_arc = Arc::new(RwLock::new(recent_jobs));

    let recent_jobs_write = recent_jobs_rwl_arc.clone();
    task::spawn(async move {
        loop {
            let job = receiver.recv().await.unwrap();
            {
                let mut recent_jobs_ = recent_jobs_write.write().await;
                recent_jobs_.insert(job.pool.name.clone(), job.clone());
            }
        }
    });

    let recent_jobs_read = recent_jobs_rwl_arc.clone();
    loop {
        task::sleep(Duration::from_secs(2)).await;
        {        
            let recent_jobs_ = recent_jobs_read.read().await;
            let mut max_height = 0;
            let mut height_occurences: BTreeMap<u32, u16> = BTreeMap::new();
            for height in recent_jobs_.values().filter(|job| job.age() > min_job_age).map(|job_update| job_update.coinbase_info().height) {
                if height > max_height {
                    max_height = height;
                }
                height_occurences.entry(height).and_modify(|occurences| {*occurences += 1} ).or_insert(1);
            }
        
            println!("occurences={:?}, max={}", height_occurences, max_height);
            // find out what the majority of pools mines on
            let mut majority_size = 0;
            let mut majority_height = 0;
            for (height, size) in height_occurences.iter() {
                if *size > majority_size {
                    majority_height = *height;
                    majority_size = *size;
                }
            }
            if majority_height < max_height {
                warn!("max_height={} while majority_height={}", max_height, majority_height);
            }
        }        
    }
}
