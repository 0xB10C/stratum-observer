use async_std::task;
use client::{initialize_client, Client};
use core::time::Duration;
use env_logger::Env;

mod client;
mod config;
mod types;
mod utils;

fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("debug")).init();
    let config = match config::load_config() {
        Ok(c) => c,
        Err(e) => panic!("could not load config: {}", e),
    };

    for pool in config.pools.clone() {
        task::spawn(async move {
            let client = Client::new(&pool).await;
            initialize_client(client).await;
        });
    }

    task::block_on(async {
        loop {
            task::sleep(Duration::from_secs(1)).await;
        }
    });
}
