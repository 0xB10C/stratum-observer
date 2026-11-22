use crate::schema::job_updates;
use crate::utils::{bip34_coinbase_block_height, encode_hex, extract_coinbase_string};
use bitcoin::consensus::encode::Error as ConsensusError;
use bitcoin::hashes::sha256d::Hash;
use chrono::prelude::*;
use diesel::{Insertable, Queryable, Selectable};
use log::warn;
use serde::{Deserialize, Serialize};
use sv1_api::server_to_client;
use sv1_api::utils::Extranonce;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct Pool {
    //id: u32,
    pub name: String,
    pub endpoint: String,
    //is_v2: bool,
    pub user: String,
    pub password: Option<String>,
    /// Optional maximum age of the connection in seconds before we close it and open a new one.
    /// If None, keep the connection open for as long as possible.
    pub max_lifetime: Option<u32>,
}

#[derive(Insertable)]
#[diesel(table_name = job_updates)]
pub struct NewJobUpdate {
    pub timestamp: chrono::NaiveDateTime,
    pub pool: String,
    pub merkle_branches: Vec<Vec<u8>>,
    pub header_version: i64,
    pub header_bits: i64,
    pub header_time: i64,
    pub header_prev_hash: String,
    pub clean_jobs: bool,
    pub extranonce1: Vec<u8>,
    pub extranonce2_size: i32,
    pub coinbase_raw: Vec<u8>,
    pub coinbase_tag: String,
    pub coinbase_value: i64,
    pub coinbase_height: i64,
    pub coinbase_output_count: i32,
}

impl From<JobUpdate<'_>> for NewJobUpdate {
    fn from(o: JobUpdate<'_>) -> Self {
        let coinbase_info = o.coinbase_info();
        NewJobUpdate {
            timestamp: o.timestamp.naive_utc(),
            pool: o.pool.name.clone(),
            merkle_branches: o
                .job
                .merkle_branch
                .iter()
                .map(|branch| branch.as_ref().to_vec())
                .collect(),
            header_version: o.job.version.0 as i64,
            header_bits: o.job.bits.0 as i64,
            header_time: o.job.time.0 as i64,
            header_prev_hash: o.prev_block_hash().to_string(),
            extranonce1: o.extranonce1.as_ref().to_vec(),
            extranonce2_size: o.extranonce2_size as i32,
            clean_jobs: o.job.clean_jobs,
            coinbase_raw: coinbase_info.raw,
            coinbase_value: coinbase_info.value_sum as i64,
            coinbase_tag: coinbase_info.tag,
            coinbase_height: coinbase_info.height as i64,
            coinbase_output_count: coinbase_info.output_count,
        }
    }
}

/// A job update as it is stored in the database. Reading rows back needs the
/// columns in the order the table has them.
#[derive(Debug, Clone, PartialEq, Queryable, Selectable)]
#[diesel(table_name = job_updates)]
pub struct StoredJobUpdate {
    pub id: i32,
    pub timestamp: chrono::NaiveDateTime,
    pub pool: String,
    pub merkle_branches: Vec<Option<Vec<u8>>>,
    pub header_version: i64,
    pub header_bits: i64,
    pub header_time: i64,
    pub header_prev_hash: String,
    pub extranonce1: Vec<u8>,
    pub extranonce2_size: i32,
    pub clean_jobs: bool,
    pub coinbase_raw: Vec<u8>,
    pub coinbase_value: i64,
    pub coinbase_output_count: i32,
    pub coinbase_tag: String,
    pub coinbase_height: i64,
}

#[derive(Serialize)]
pub struct JobUpdateJson {
    pool_name: String,
    prev_hash: String,
    coinbase_tag: String,
    height: u32,
    coinbase_sum: u64,
    job_timestamp: i64,
    header_version: u32,
    header_time: u32,
    header_bits: u32,
    merkle_branches: Vec<String>,
    clean_jobs: bool,
}

impl From<JobUpdate<'_>> for JobUpdateJson {
    fn from(o: JobUpdate<'_>) -> Self {
        let coinbase_info = o.coinbase_info();
        JobUpdateJson {
            pool_name: o.pool.clone().name,
            prev_hash: o.prev_block_hash().to_string(),
            coinbase_tag: coinbase_info.tag,
            height: coinbase_info.height,
            coinbase_sum: coinbase_info.value_sum,
            job_timestamp: o.timestamp.timestamp(),
            header_version: o.job.version.0,
            header_bits: o.job.bits.0,
            header_time: o.job.time.0,
            clean_jobs: o.job.clean_jobs,
            merkle_branches: o
                .job
                .merkle_branch
                .iter()
                .map(|b| encode_hex(b.as_ref()))
                .collect(),
        }
    }
}

pub struct CoinbaseInfo {
    pub height: u32,
    pub tag: String,
    pub value_sum: u64,
    pub output_count: i32,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct JobUpdate<'a> {
    /// JobUpdate timestamp
    pub timestamp: DateTime<Utc>,
    pub pool: Pool,
    pub job: server_to_client::Notify<'a>,
    pub extranonce1: Extranonce<'a>,
    pub extranonce2_size: usize,
    /// Time the client connection was established.
    pub time_connected: DateTime<Utc>,
}

impl JobUpdate<'_> {
    pub fn raw_coinbase(&self) -> Vec<u8> {
        let extranonce2 = vec![&0u8; self.extranonce2_size];
        self.job
            .coin_base1
            .as_ref()
            .into_iter()
            .chain(self.extranonce1.as_ref().into_iter())
            .chain(extranonce2.into_iter())
            .chain(self.job.coin_base2.as_ref().into_iter())
            .map(|b| *b)
            .collect()
    }

    pub fn coinbase(&self) -> Result<bitcoin::Transaction, ConsensusError> {
        let result = bitcoin::consensus::deserialize(&self.raw_coinbase());
        if let Err(ref e) = result {
            warn!("failed to deserialize coinbase transaction with extranonce1={} extranonce2size={}: {} - rawtx={}",
                encode_hex(self.extranonce1.as_ref()),
                self.extranonce2_size,
                e,
                encode_hex(&self.raw_coinbase()),
            );
        }
        result
    }

    pub fn coinbase_info(&self) -> CoinbaseInfo {
        let raw_coinbase = self.raw_coinbase();
        match self.coinbase() {
            Ok(coinbase) => {
                // A coinbase always has exactly one input, but a transaction
                // without any inputs still deserializes, so don't trust the
                // pool to send us one that has an input.
                let coinbase_script_sig = match coinbase.input.first() {
                    Some(input) => &input.script_sig,
                    None => {
                        warn!(
                            "coinbase transaction has no inputs: rawtx={}",
                            encode_hex(&raw_coinbase),
                        );
                        return CoinbaseInfo {
                            height: 0,
                            tag: "failed to deserialize coinbase: transaction has no inputs"
                                .to_string(),
                            value_sum: 0,
                            output_count: 0,
                            raw: raw_coinbase,
                        };
                    }
                };

                CoinbaseInfo {
                    height: bip34_coinbase_block_height(&coinbase_script_sig).unwrap_or_default(),
                    tag: extract_coinbase_string(coinbase_script_sig),
                    value_sum: coinbase
                        .output
                        .iter()
                        .map(|o| o.value.to_sat() as u64)
                        .sum::<u64>(),
                    output_count: coinbase.output.len() as i32,
                    raw: raw_coinbase,
                }
            }
            Err(e) => CoinbaseInfo {
                height: 0,
                tag: format!("failed to deserialize coinbase: {}", e),
                value_sum: 0,
                output_count: 0,
                raw: raw_coinbase,
            },
        }
    }

    pub fn prev_block_hash(&self) -> bitcoin::BlockHash {
        let h: &[u8] = self.job.prev_hash.as_ref();
        let array: [u8; 32] = h.try_into().expect("prev_hash should always be 32 byte");
        bitcoin::BlockHash::from_raw_hash(*Hash::from_bytes_ref(&array))
    }

    pub fn time_connected_seconds(&self) -> i64 {
        (Utc::now() - self.time_connected).num_seconds()
    }

    /// Age of the JobUpdate in seconds.
    pub fn age(&self) -> i64 {
        (Utc::now() - self.timestamp).num_seconds()
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use crate::utils::decode_hex;
    use sv1_api::json_rpc;

    pub fn test_pool() -> Pool {
        Pool {
            name: "test pool".to_string(),
            endpoint: "pool.example.com:3333".to_string(),
            user: "user".to_string(),
            password: None,
            max_lifetime: None,
        }
    }

    /// Build a JobUpdate from a `mining.notify` line as a pool sends it, plus
    /// the extranonce1 and the extranonce2 size the pool handed out when the
    /// client subscribed.
    pub fn job_update_from_notify(
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
        assert_eq!(notification.method, "mining.notify");
        let job = server_to_client::Notify::try_from(notification)
            .expect("the notification should be a mining.notify");
        let now = Utc::now();
        JobUpdate {
            timestamp: now,
            pool: test_pool(),
            job,
            extranonce1: Extranonce::try_from(decode_hex(extranonce1_hex).unwrap()).unwrap(),
            extranonce2_size,
            time_connected: now,
        }
    }
}

#[cfg(test)]
mod test {
    use super::test_support::*;
    use super::*;
    use crate::utils::decode_hex;
    use chrono::TimeDelta;

    /// A coinbase that holds no inputs at all. Such a transaction deserializes
    /// fine, even though a real coinbase always has exactly one input. Taken
    /// apart: a version, a segwit marker and flag, no inputs, no outputs and a
    /// locktime.
    const NO_INPUT_COINBASE_1: &str = "010000000001";
    const NO_INPUT_COINBASE_EXTRANONCE1: &str = "00";
    const NO_INPUT_COINBASE_2: &str = "00000000";

    fn no_input_coinbase_job() -> JobUpdate<'static> {
        let notify = format!(
            r#"{{"method":"mining.notify","params":["job1","{}","{}","{}",[],"20000000","17031abe","66bca309",true]}}"#,
            "0".repeat(64),
            NO_INPUT_COINBASE_1,
            NO_INPUT_COINBASE_2,
        );
        job_update_from_notify(&notify, NO_INPUT_COINBASE_EXTRANONCE1, 1)
    }

    #[test]
    fn test_coinbase_without_inputs_does_not_panic() {
        let job = no_input_coinbase_job();
        assert_eq!(encode_hex(&job.raw_coinbase()), "010000000001000000000000");
        // The transaction itself deserializes, it just has no input to read the
        // height and the tag from.
        assert!(job.coinbase().is_ok());

        let info = job.coinbase_info();
        assert_eq!(info.height, 0);
        assert_eq!(info.output_count, 0);
        assert_eq!(info.value_sum, 0);
        assert_eq!(info.raw, job.raw_coinbase());
        assert!(
            info.tag.starts_with("failed to deserialize coinbase:"),
            "the tag should say what went wrong, got {:?}",
            info.tag
        );
    }

    // The values below belong to mainnet block 856740, hash
    // 00000000000000000002bc7c54cd5df36e432e93a5ad102344db9ad876f0311a. The
    // header fields come from the mempool.space API endpoint
    // /api/block/<hash>, the raw coinbase transaction from
    // /api/tx/<txid>/hex.
    const BLOCK_HEIGHT: u32 = 856740;
    const COINBASE_TXID: &str = "82f3748ef4ffc2acfefbbc682adf34532eef41e13c4819085a8af611a8f118fa";
    const COINBASE_RAW: &str = "010000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff5503a4120d04b5a2bc667c204d41524120506f6f6c207c204d61646520696e2055534120f09f87baf09f87b8207c202876303331393234293c39dbd326a3c601527b7112d4f11f911356a5e9450000000000ffffffffffffffff0234e2e412000000001976a9142fc701e2049ee4957b07134b6c1d771dd5a96b2188ac0000000000000000266a24aa21a9edd611b073de70673be0fb5a77d929872ad76c718b73c314ad7e8d6e6c71c7712301200000000000000000000000000000000000000000000000000000000000000000aff4fb79";
    const COINBASE_TAG: &str = "f| MARA Pool | Made in USA  | (v031924)<9";
    const COINBASE_VALUE: u64 = 316990004;
    const COINBASE_OUTPUT_COUNT: i32 = 2;

    // The raw coinbase above, split the way a pool splits it: the extranonce1
    // the pool hands out and the extranonce2 the miner picks both sit in the
    // input script, between the two coinbase parts. The real transaction has
    // four zero bytes where the extranonce2 goes, which is exactly what
    // raw_coinbase() fills in, so the parts add back up to the real
    // transaction.
    const COINBASE_1: &str = "010000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff5503a4120d04b5a2bc667c204d41524120506f6f6c207c204d61646520696e2055534120f09f87baf09f87b8207c202876303331393234293c39dbd326a3c601527b7112d4";
    const EXTRANONCE1: &str = "f11f911356a5e945";
    const EXTRANONCE2_SIZE: usize = 4;
    const COINBASE_2: &str = "00ffffffffffffffff0234e2e412000000001976a9142fc701e2049ee4957b07134b6c1d771dd5a96b2188ac0000000000000000266a24aa21a9edd611b073de70673be0fb5a77d929872ad76c718b73c314ad7e8d6e6c71c7712301200000000000000000000000000000000000000000000000000000000000000000aff4fb79";

    /// The hash of block 856739, the way an explorer shows it.
    const PREV_HASH_DISPLAYED: &str =
        "00000000000000000002c64b6204ab56f2cd81fca5516c3ca7a1642a9fd65203";
    /// The same hash the way stratum sends it: the internal byte order, with
    /// the bytes of every four byte word reversed.
    const PREV_HASH_STRATUM: &str =
        "9fd65203a7a1642aa5516c3cf2cd81fc6204ab560002c64b0000000000000000";

    const HEADER_VERSION: u32 = 0x21048000;
    const HEADER_BITS: u32 = 0x17031abe;
    const HEADER_TIME: u32 = 0x66bca309;

    // Block 856740 holds thousands of transactions and the explorer doesn't
    // hand out the merkle branch a miner would have been given, so these two
    // nodes are made up. They are only here to check that the branch is passed
    // on unchanged, so their bytes are not symmetric.
    const MERKLE_BRANCH: [&str; 2] = [
        "00112233445566778899aabbccddeeff0f1e2d3c4b5a69788796a5b4c3d2e1f0",
        "1a2b3c4d5e6f708192a3b4c5d6e7f8090a1b2c3d4e5f60718293a4b5c6d7e8f9",
    ];

    fn real_block_job() -> JobUpdate<'static> {
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
        let mut job = job_update_from_notify(&notify, EXTRANONCE1, EXTRANONCE2_SIZE);
        // Pin the timestamps so the conversions below are reproducible.
        job.timestamp = DateTime::from_timestamp(HEADER_TIME as i64, 0).unwrap();
        job.time_connected = DateTime::from_timestamp(HEADER_TIME as i64 - 60, 0).unwrap();
        job
    }

    #[test]
    fn test_real_block_raw_coinbase() {
        let job = real_block_job();
        // Putting the parts back together gives the coinbase transaction that
        // ended up in the block.
        assert_eq!(encode_hex(&job.raw_coinbase()), COINBASE_RAW);
        let coinbase = job.coinbase().expect("the coinbase should deserialize");
        assert_eq!(coinbase.compute_txid().to_string(), COINBASE_TXID);
    }

    #[test]
    fn test_real_block_coinbase_info() {
        let info = real_block_job().coinbase_info();
        assert_eq!(info.height, BLOCK_HEIGHT);
        assert_eq!(info.tag, COINBASE_TAG);
        assert_eq!(info.value_sum, COINBASE_VALUE);
        assert_eq!(info.output_count, COINBASE_OUTPUT_COUNT);
        assert_eq!(encode_hex(&info.raw), COINBASE_RAW);
    }

    #[test]
    fn test_real_block_prev_block_hash() {
        // The prev hash a pool sends has the bytes of every four byte word
        // swapped. Undoing that has to give the hash of the previous block as
        // an explorer shows it.
        let job = real_block_job();
        assert_eq!(job.prev_block_hash().to_string(), PREV_HASH_DISPLAYED);
    }

    #[test]
    fn test_real_block_job_update_json() {
        let job = real_block_job();
        let json = serde_json::to_value(JobUpdateJson::from(job)).unwrap();

        // The web page in www/index.html reads these fields by name.
        for field in [
            "pool_name",
            "prev_hash",
            "coinbase_tag",
            "height",
            "coinbase_sum",
            "job_timestamp",
            "merkle_branches",
            "clean_jobs",
        ] {
            assert!(
                json.get(field).is_some(),
                "the JSON is missing the field {} that the web page reads",
                field
            );
        }

        assert_eq!(json["pool_name"], "test pool");
        assert_eq!(json["prev_hash"], PREV_HASH_DISPLAYED);
        assert_eq!(json["coinbase_tag"], COINBASE_TAG);
        assert_eq!(json["height"], BLOCK_HEIGHT);
        assert_eq!(json["coinbase_sum"], COINBASE_VALUE);
        assert_eq!(json["job_timestamp"], HEADER_TIME);
        assert_eq!(json["header_version"], HEADER_VERSION);
        assert_eq!(json["header_bits"], HEADER_BITS);
        assert_eq!(json["header_time"], HEADER_TIME);
        assert_eq!(json["clean_jobs"], true);
        assert_eq!(
            json["merkle_branches"],
            serde_json::json!([MERKLE_BRANCH[0], MERKLE_BRANCH[1]])
        );
    }

    #[test]
    fn test_real_block_new_job_update() {
        let job = real_block_job();
        let row = NewJobUpdate::from(job.clone());

        assert_eq!(row.timestamp, job.timestamp.naive_utc());
        assert_eq!(row.pool, "test pool");
        assert_eq!(
            row.merkle_branches,
            vec![
                decode_hex(MERKLE_BRANCH[0]).unwrap(),
                decode_hex(MERKLE_BRANCH[1]).unwrap(),
            ]
        );
        assert_eq!(row.header_version, HEADER_VERSION as i64);
        assert_eq!(row.header_bits, HEADER_BITS as i64);
        assert_eq!(row.header_time, HEADER_TIME as i64);
        assert_eq!(row.header_prev_hash, PREV_HASH_DISPLAYED);
        assert!(row.clean_jobs);
        assert_eq!(row.extranonce1, decode_hex(EXTRANONCE1).unwrap());
        assert_eq!(row.extranonce2_size, EXTRANONCE2_SIZE as i32);
        assert_eq!(encode_hex(&row.coinbase_raw), COINBASE_RAW);
        assert_eq!(row.coinbase_tag, COINBASE_TAG);
        assert_eq!(row.coinbase_value, COINBASE_VALUE as i64);
        assert_eq!(row.coinbase_height, BLOCK_HEIGHT as i64);
        assert_eq!(row.coinbase_output_count, COINBASE_OUTPUT_COUNT);
    }

    #[test]
    fn test_age_and_time_connected_seconds() {
        let mut job = real_block_job();
        let now = Utc::now();
        job.timestamp = now;
        job.time_connected = now;
        assert_eq!(job.age(), 0);
        assert_eq!(job.time_connected_seconds(), 0);

        job.timestamp = now - TimeDelta::seconds(5);
        job.time_connected = now - TimeDelta::seconds(90);
        // Some time passes while the test runs, so allow for a second of slack.
        assert!((5..=6).contains(&job.age()), "age was {}", job.age());
        assert!(
            (90..=91).contains(&job.time_connected_seconds()),
            "time connected was {}",
            job.time_connected_seconds()
        );
    }
}
