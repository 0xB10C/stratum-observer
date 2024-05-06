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
