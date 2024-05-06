use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Pool {
    //id: u32,
    pub name: String,
    pub endpoint: String,
    //is_v2: bool,
    pub user: String,
    pub password: String,
}
