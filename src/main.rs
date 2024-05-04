use async_std::task;

use std::net::ToSocketAddrs;

mod client;
mod utils;

use client::{initialize_client, Client};

fn main() {
    let addr = "stratum.braiins.com:3333"
        .to_socket_addrs()
        .unwrap()
        .next()
        .unwrap();

    task::block_on(async {
        let client = Client::new(80, addr).await;
        initialize_client(client).await;
    });
}
