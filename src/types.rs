use bitcoin::hashes::sha256d::Hash;

use serde::Deserialize;
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
}

#[derive(Debug, Clone)]
pub struct JobUpdate<'a> {
    pub pool: Pool,
    pub job: server_to_client::Notify<'a>,
    pub extranonce1: Extranonce<'a>,
    pub extranonce2_size: usize,
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
}
