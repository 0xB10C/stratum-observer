use async_channel::{bounded, unbounded, Receiver, Sender};
use async_std::task;
use client::{initialize_client, Client};
use env_logger::Env;
use std::collections::BTreeMap;

mod client;
mod config;
mod types;
mod utils;

use crate::types::JobUpdate;
use crate::utils::bip34_coinbase_block_height;

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
            let client = Client::new(&pool, js).await;
            initialize_client(client).await;
        });
    }

    let (visualization_sender, visulization_receiver) = unbounded();
    task::spawn(async move {
        terminal_visualization_task(visulization_receiver).await;
    });

    task::block_on(async {
        loop {
            let job = job_receiver.recv().await.unwrap();
            println!("new job");
            visualization_sender.send(job).await.unwrap();
        }
    });
}

async fn terminal_visualization_task(receiver: Receiver<JobUpdate<'_>>) {
    let mut last_job: BTreeMap<String, JobUpdate> = BTreeMap::new();
    loop {
        let job = receiver.recv().await.unwrap();
        last_job.insert(job.pool.clone().name, job);

        for (_, j) in last_job.iter() {
            let cb = j.clone().coinbase();
            println!(
                "{: <18} {} {: >6}   {:2.8} BTC",
                j.pool.name,
                j.prev_block_hash().to_string(),
                bip34_coinbase_block_height(
                    &cb.input
                        .first()
                        .expect("coinbase should only have one input")
                        .script_sig
                )
                .unwrap_or_default(),
                cb.output.iter().map(|o| o.value.to_btc()).sum::<f64>()
            );
        }
    }
}
