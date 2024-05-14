use crate::schema::job_updates;
use crate::utils::{bip34_coinbase_block_height, encode_hex, extract_coinbase_string};
use bitcoin::hashes::sha256d::Hash;
use chrono::prelude::*;
use diesel::Insertable;
use serde::{Deserialize, Serialize};
use sv1_api::server_to_client;
use sv1_api::utils::Extranonce;

#[derive(Debug, Deserialize, Clone)]
pub struct Pool {
    //id: u32,
    pub name: String,
    pub endpoint: String,
    //is_v2: bool,
    pub user: String,
    pub password: String,
    /// Optional maximum age of the connection in seconds before we close it and open a new one.
    /// If None, keep the connection open for as long as possible.
    pub max_lifetime: Option<u32>,
}

#[derive(Insertable)]
#[diesel(table_name = job_updates)]
pub struct NewJobUpdate {
    pub timestamp: chrono::NaiveDateTime,
    pub pool_name: String,
    pub coinbase_tag: String,
    pub prev_hash: String,
    pub merkle_branches: String,
    pub height: i64,
    pub output_sum: i64,
    pub header_version: i64,
    pub header_bits: i64,
    pub header_time: i64,
    pub clean_jobs: bool,
    pub extranonce1: Vec<u8>,
    pub extranonce2_size: i32,
}

impl From<JobUpdate<'_>> for NewJobUpdate {
    fn from(o: JobUpdate<'_>) -> Self {
        let cb = o.clone().coinbase();
        let coinbase_script_sig = &cb
            .input
            .first()
            .expect("coinbase should have an input")
            .script_sig;

        NewJobUpdate {
            timestamp: o.timestamp.naive_utc(),
            pool_name: o.pool.name.clone(),
            coinbase_tag: extract_coinbase_string(coinbase_script_sig),
            prev_hash: o.prev_block_hash().to_string(),
            merkle_branches: o
                .job
                .merkle_branch
                .iter()
                .map(|b| encode_hex(b.as_ref()))
                .collect::<Vec<String>>()
                .join(":"),
            height: (bip34_coinbase_block_height(&coinbase_script_sig).unwrap_or_default() as i64),
            output_sum: cb
                .output
                .iter()
                .map(|o| o.value.to_sat() as i64)
                .sum::<i64>(),
            header_version: (o.job.version.0 as i64),
            header_bits: o.job.bits.0 as i64,
            header_time: o.job.time.0 as i64,
            extranonce1: o.extranonce1.as_ref().to_vec(),
            extranonce2_size: o.extranonce2_size as i32,
            clean_jobs: o.job.clean_jobs,
        }
    }
}

#[derive(Serialize)]
pub struct JobUpdateJson {
    pool_name: String,
    prev_hash: String,
    coinbase_tag: String,
    height: u64,
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
        let cb = o.clone().coinbase();
        let coinbase_script_sig = &cb
            .input
            .first()
            .expect("coinbase should have an input")
            .script_sig;

        JobUpdateJson {
            pool_name: o.pool.clone().name,
            prev_hash: o.prev_block_hash().to_string(),
            coinbase_tag: extract_coinbase_string(coinbase_script_sig),
            height: (bip34_coinbase_block_height(&coinbase_script_sig).unwrap_or_default() as u64),
            coinbase_sum: cb
                .output
                .iter()
                .map(|o| o.value.to_sat() as u64)
                .sum::<u64>(),
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
    pub fn coinbase(self) -> bitcoin::Transaction {
        let extranonce2 = vec![&0u8; self.extranonce2_size];
        let rawtx: Vec<u8> = self
            .job
            .coin_base1
            .as_ref()
            .into_iter()
            .chain(self.extranonce1.as_ref().into_iter())
            .chain(extranonce2.into_iter())
            .chain(self.job.coin_base2.as_ref().into_iter())
            .map(|b| *b)
            .collect();
        bitcoin::consensus::deserialize(&rawtx).unwrap() // TODO: handle errors
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
