use async_std::channel::unbounded;
use async_std::task;
use client::{initialize_client, Client};
use env_logger::Env;

mod client;
mod config;
mod types;
mod utils;

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

    task::block_on(async {
        loop {
            let job = job_receiver.recv().await;
            println!("job {:?}", job);
        }
    });
}
