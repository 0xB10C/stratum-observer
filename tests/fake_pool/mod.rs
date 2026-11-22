//! A fake stratum pool to run the client against.
//!
//! It listens on a loopback port, answers the handshake the way a real pool
//! does and then pushes a difficulty and a job. Everything the client sent is
//! handed to the test, so it can check what the client said and in which order.

#![allow(dead_code)]

use async_channel::{unbounded, Receiver, Sender};
use async_std::io::BufReader;
use async_std::net::{Shutdown, TcpListener};
use async_std::prelude::*;
use async_std::sync::Arc;
use async_std::task;
use sv1_api::json_rpc;

/// The extranonce1 the fake pool hands out when a client subscribes.
pub const EXTRANONCE1: &str = "f11f9113";
/// The size of the extranonce2 the fake pool expects a client to pick.
pub const EXTRANONCE2_SIZE: usize = 4;
/// The subscription id the fake pool hands out.
pub const SUBSCRIPTION_ID: &str = "abc";

/// The job the fake pool sends. The fields belong to mainnet block 856740,
/// with the coinbase transaction split so that the extranonce1 above and the
/// four extranonce2 bytes a miner picks sit between the two coinbase parts.
pub const JOB_ID: &str = "6a1f2f3c";
/// The hash of block 856739, in the byte order stratum sends it in.
pub const PREV_HASH: &str = "9fd65203a7a1642aa5516c3cf2cd81fc6204ab560002c64b0000000000000000";
pub const HEADER_VERSION: u32 = 0x21048000;
pub const HEADER_BITS: u32 = 0x17031abe;
pub const HEADER_TIME: u32 = 0x66bca309;
pub const MERKLE_BRANCH: [&str; 2] = [
    "00112233445566778899aabbccddeeff0f1e2d3c4b5a69788796a5b4c3d2e1f0",
    "1a2b3c4d5e6f708192a3b4c5d6e7f8090a1b2c3d4e5f60718293a4b5c6d7e8f9",
];
/// The height block 856740 was mined at, as its coinbase states it.
pub const COINBASE_HEIGHT: u32 = 856740;
/// The tag the miner of block 856740 put into its coinbase.
pub const COINBASE_TAG: &str = "f| MARA Pool | Made in USA  | (v031924)<9";
/// What the coinbase of block 856740 pays out, in satoshi.
pub const COINBASE_VALUE: u64 = 316990004;
pub const COINBASE_OUTPUT_COUNT: i32 = 2;

const COINBASE_1: &str = "010000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff5503a4120d04b5a2bc667c204d41524120506f6f6c207c204d61646520696e2055534120f09f87baf09f87b8207c202876303331393234293c39dbd326a3c601527b7112d4";
const COINBASE_2: &str = "0000000000ffffffffffffffff0234e2e412000000001976a9142fc701e2049ee4957b07134b6c1d771dd5a96b2188ac0000000000000000266a24aa21a9edd611b073de70673be0fb5a77d929872ad76c718b73c314ad7e8d6e6c71c7712301200000000000000000000000000000000000000000000000000000000000000000aff4fb79";

/// The difficulty the fake pool asks for before it sends the job.
pub const DIFFICULTY: f64 = 1024.0;

/// What the fake pool does with the `mining.configure` a client sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Configure {
    /// Answer it with an empty result, the way a pool that has nothing to
    /// configure does.
    Answer,
    /// Say nothing at all. `mining.configure` is optional and some pools
    /// simply ignore it.
    Ignore,
}

/// How the fake pool behaves once a client has connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behaviour {
    /// Speak stratum: answer the handshake, then send a difficulty and a job.
    Stratum(Configure),
    /// Close the connection again right after accepting it.
    CloseImmediately,
}

pub struct FakePool {
    /// The address the pool listens on.
    pub address: String,
    /// Every line the pool read from the client, in the order they arrived.
    pub requests: Receiver<String>,
    /// Gets a message once the client closed its end of the connection.
    pub client_gone: Receiver<()>,
    /// Lines for the pool to send to the client. Each one needs its own
    /// newline at the end.
    pub to_client: Sender<String>,
}

impl FakePool {
    /// Starts a pool on a loopback port that serves a single connection.
    pub async fn start(behaviour: Behaviour) -> FakePool {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the fake pool should be able to listen on a loopback port");
        let address = listener
            .local_addr()
            .expect("the fake pool should have an address")
            .to_string();
        let (request_sender, requests) = unbounded();
        let (gone_sender, client_gone) = unbounded();
        let (to_client, outgoing) = unbounded::<String>();
        let answer_sender = to_client.clone();

        task::spawn(async move {
            let (stream, _) = listener
                .accept()
                .await
                .expect("the fake pool should be able to accept a connection");
            let stream = Arc::new(stream);

            let configure = match behaviour {
                Behaviour::CloseImmediately => {
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
                Behaviour::Stratum(configure) => configure,
            };

            // Everything the pool sends goes through one task, so the client
            // sees the lines in the order they were queued.
            let writer = stream.clone();
            task::spawn(async move {
                while let Ok(line) = outgoing.recv().await {
                    if (&*writer).write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });

            let mut lines = BufReader::new(&*stream).lines();
            while let Some(line) = lines.next().await {
                let line = match line {
                    Ok(line) => line,
                    Err(_) => break,
                };
                if request_sender.send(line.clone()).await.is_err() {
                    break;
                }
                for answer in answers(&line, configure) {
                    if answer_sender.send(answer).await.is_err() {
                        break;
                    }
                }
            }
            let _ = gone_sender.send(()).await;
        });

        FakePool {
            address,
            requests,
            client_gone,
            to_client,
        }
    }
}

/// What the pool answers a request with. A request it does not know gets no
/// answer at all.
fn answers(line: &str, configure: Configure) -> Vec<String> {
    let request: json_rpc::StandardRequest = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(_) => return vec![],
    };
    let id = request.id;
    match request.method.as_str() {
        "mining.configure" => match configure {
            Configure::Answer => vec![line_of(&format!(
                r#"{{"id":{},"error":null,"result":{{}}}}"#,
                id
            ))],
            Configure::Ignore => vec![],
        },
        "mining.subscribe" => vec![line_of(&format!(
            r#"{{"id":{},"error":null,"result":[[["mining.notify","{}"]],"{}",{}]}}"#,
            id, SUBSCRIPTION_ID, EXTRANONCE1, EXTRANONCE2_SIZE
        ))],
        // Once the client is authorized, it gets a difficulty and a job.
        "mining.authorize" => vec![
            line_of(&format!(r#"{{"id":{},"error":null,"result":true}}"#, id)),
            line_of(&set_difficulty(DIFFICULTY)),
            line_of(&notify()),
        ],
        _ => vec![],
    }
}

fn line_of(message: &str) -> String {
    format!("{}\n", message)
}

/// A `mining.set_difficulty` as a pool sends it.
pub fn set_difficulty(value: f64) -> String {
    format!(
        r#"{{"id":null,"method":"mining.set_difficulty","params":[{}]}}"#,
        value
    )
}

/// A `mining.set_extranonce`, which a pool sends when it hands out a new
/// extranonce1 in the middle of a session.
pub fn set_extranonce(extranonce1: &str, extranonce2_size: usize) -> String {
    format!(
        r#"{{"id":null,"method":"mining.set_extranonce","params":["{}",{}]}}"#,
        extranonce1, extranonce2_size
    )
}

/// The `mining.notify` the fake pool sends.
pub fn notify() -> String {
    format!(
        r#"{{"id":null,"method":"mining.notify","params":["{}","{}","{}","{}",["{}","{}"],"{:08x}","{:08x}","{:08x}",true]}}"#,
        JOB_ID,
        PREV_HASH,
        COINBASE_1,
        COINBASE_2,
        MERKLE_BRANCH[0],
        MERKLE_BRANCH[1],
        HEADER_VERSION,
        HEADER_BITS,
        HEADER_TIME,
    )
}

/// An address on the loopback interface that nothing listens on: a port is
/// taken and given up again right away.
pub async fn closed_address() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("we should be able to take a loopback port");
    listener
        .local_addr()
        .expect("the listener should have an address")
        .to_string()
}
