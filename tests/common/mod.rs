//! Helpers shared by the integration tests.
//!
//! The helpers in `src/types.rs` are only built for the unit tests, so the
//! integration tests here bring their own copy.

#![allow(dead_code)]

use chrono::prelude::*;
use stratum_observer::types::{JobUpdate, Pool};
use stratum_observer::utils::decode_hex;
use sv1_api::json_rpc;
use sv1_api::server_to_client;
use sv1_api::utils::Extranonce;

// The values below belong to mainnet block 856740, hash
// 00000000000000000002bc7c54cd5df36e432e93a5ad102344db9ad876f0311a. The
// header fields come from the mempool.space API endpoint /api/block/<hash>,
// the raw coinbase transaction from /api/tx/<txid>/hex.
pub const BLOCK_HEIGHT: u32 = 856740;
pub const COINBASE_RAW: &str = "010000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff5503a4120d04b5a2bc667c204d41524120506f6f6c207c204d61646520696e2055534120f09f87baf09f87b8207c202876303331393234293c39dbd326a3c601527b7112d4f11f911356a5e9450000000000ffffffffffffffff0234e2e412000000001976a9142fc701e2049ee4957b07134b6c1d771dd5a96b2188ac0000000000000000266a24aa21a9edd611b073de70673be0fb5a77d929872ad76c718b73c314ad7e8d6e6c71c7712301200000000000000000000000000000000000000000000000000000000000000000aff4fb79";
pub const COINBASE_TAG: &str = "f| MARA Pool | Made in USA  | (v031924)<9";
pub const COINBASE_VALUE: u64 = 316990004;
pub const COINBASE_OUTPUT_COUNT: i32 = 2;

// The raw coinbase above, split the way a pool splits it: the extranonce1 the
// pool hands out and the extranonce2 the miner picks both sit in the input
// script, between the two coinbase parts.
const COINBASE_1: &str = "010000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff5503a4120d04b5a2bc667c204d41524120506f6f6c207c204d61646520696e2055534120f09f87baf09f87b8207c202876303331393234293c39dbd326a3c601527b7112d4";
pub const EXTRANONCE1: &str = "f11f911356a5e945";
pub const EXTRANONCE2_SIZE: usize = 4;
const COINBASE_2: &str = "00ffffffffffffffff0234e2e412000000001976a9142fc701e2049ee4957b07134b6c1d771dd5a96b2188ac0000000000000000266a24aa21a9edd611b073de70673be0fb5a77d929872ad76c718b73c314ad7e8d6e6c71c7712301200000000000000000000000000000000000000000000000000000000000000000aff4fb79";

/// The hash of block 856739, the way an explorer shows it.
pub const PREV_HASH_DISPLAYED: &str =
    "00000000000000000002c64b6204ab56f2cd81fca5516c3ca7a1642a9fd65203";
/// The same hash the way stratum sends it: the internal byte order, with the
/// bytes of every four byte word reversed.
const PREV_HASH_STRATUM: &str = "9fd65203a7a1642aa5516c3cf2cd81fc6204ab560002c64b0000000000000000";

pub const HEADER_VERSION: u32 = 0x21048000;
pub const HEADER_BITS: u32 = 0x17031abe;
pub const HEADER_TIME: u32 = 0x66bca309;

/// Two made up merkle branch nodes. They are only here to check that the
/// branch is passed on unchanged, so their bytes are not symmetric.
pub const MERKLE_BRANCH: [&str; 2] = [
    "00112233445566778899aabbccddeeff0f1e2d3c4b5a69788796a5b4c3d2e1f0",
    "1a2b3c4d5e6f708192a3b4c5d6e7f8090a1b2c3d4e5f60718293a4b5c6d7e8f9",
];

pub fn test_pool(name: &str) -> Pool {
    Pool {
        name: name.to_string(),
        endpoint: "pool.example.com:3333".to_string(),
        user: "user".to_string(),
        password: None,
        max_lifetime: None,
    }
}

/// Builds a JobUpdate from a `mining.notify` line as a pool sends it, plus the
/// extranonce1 and the extranonce2 size the pool handed out when the client
/// subscribed.
pub fn job_update_from_notify(
    pool: Pool,
    notify_json: &str,
    extranonce1_hex: &str,
    extranonce2_size: usize,
) -> JobUpdate<'static> {
    let message: json_rpc::Message =
        serde_json::from_str(notify_json).expect("the notify line should be valid JSON");
    let notification = match message {
        json_rpc::Message::Notification(n) => n,
        other => panic!("the notify line is not a notification: {:?}", other),
    };
    let job = server_to_client::Notify::try_from(notification)
        .expect("the notification should be a mining.notify");
    let now = Utc::now();
    JobUpdate {
        timestamp: now,
        pool,
        job,
        extranonce1: Extranonce::try_from(decode_hex(extranonce1_hex).unwrap()).unwrap(),
        extranonce2_size,
        time_connected: now,
    }
}

/// A job holding the real values of mainnet block 856740, sent by a pool with
/// the given name. The timestamps are pinned so tests can compare them.
pub fn real_block_job(pool_name: &str) -> JobUpdate<'static> {
    let notify = format!(
        r#"{{"id":null,"method":"mining.notify","params":["6a1f2f3c","{}","{}","{}",["{}","{}"],"{:08x}","{:08x}","{:08x}",true]}}"#,
        PREV_HASH_STRATUM,
        COINBASE_1,
        COINBASE_2,
        MERKLE_BRANCH[0],
        MERKLE_BRANCH[1],
        HEADER_VERSION,
        HEADER_BITS,
        HEADER_TIME,
    );
    let mut job =
        job_update_from_notify(test_pool(pool_name), &notify, EXTRANONCE1, EXTRANONCE2_SIZE);
    job.timestamp = DateTime::from_timestamp(HEADER_TIME as i64, 0).unwrap();
    job.time_connected = DateTime::from_timestamp(HEADER_TIME as i64 - 60, 0).unwrap();
    job
}

/// A name no other test uses, so tests can run next to each other.
pub fn unique_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}-{}",
        prefix,
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}
