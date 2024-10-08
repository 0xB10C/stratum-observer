use crate::types::{JobUpdate, Pool};
use crate::utils;
use async_channel::{bounded, Receiver, Sender};
use async_std::net::Shutdown;
use async_std::{
    io::BufReader,
    net::TcpStream,
    prelude::*,
    sync::{Arc, Mutex},
    task,
};
use chrono::prelude::*;
use chrono::TimeDelta;
use log::warn;
use log::{debug, error, info};
use std::net::ToSocketAddrs;
use std::time::{Duration, Instant};
use sv1_api::{
    client_to_server,
    error::Error,
    json_rpc, server_to_client,
    utils::{Extranonce, HexU32Be},
    ClientStatus, IsClient,
};

const USER_AGENT: &str = "stratum-observer";
/// The default no-job timeout in seconds.
const STRATUM_JOB_TIMEOUT_SECONDS: u64 = 60;
/// The default max lifetime of a connection to a stratum server.
const STRATUM_CONNECTION_MAX_LIFETIME_SECONDS: u32 = 590; // just below 10 minutes as some pools disconnect after 10 minutes

fn extranonce_from_hex<'a>(hex: &str) -> Extranonce<'a> {
    let data = utils::decode_hex(hex).unwrap();
    Extranonce::try_from(data).expect("Failed to convert hex to U256")
}

pub struct Client<'a> {
    pool: Pool,
    job_sender: Sender<JobUpdate<'a>>,
    message_id: u64,
    time_connected: DateTime<Utc>,
    time_last_notify: Option<Instant>,
    extranonce1: Extranonce<'a>,
    extranonce2_size: usize,
    version_rolling_mask: Option<HexU32Be>,
    version_rolling_min_bit: Option<HexU32Be>,
    status: ClientStatus,
    last_notify: Option<server_to_client::Notify<'a>>,
    sent_authorize_request: Vec<(u64, String)>, // (id, user_name)
    authorized: Vec<String>,
    receiver_incoming: Receiver<String>,
    sender_outgoing: Sender<String>,
    sender_shutdown: Sender<bool>,
    shutting_down: bool,
}

impl<'a> Client<'static> {
    pub async fn run(pool: &Pool, job_sender: Sender<JobUpdate<'static>>) {
        // TODO: handle errors
        let socket = pool.endpoint.to_socket_addrs().unwrap().next().unwrap();

        let stream = loop {
            match TcpStream::connect(socket).await {
                Ok(st) => {
                    info!("Connected to pool '{}'", pool.name);
                    break st;
                }
                Err(_) => {
                    warn!("Pool '{}' unreachable...", pool.name);
                    task::sleep(Duration::from_secs(2)).await;
                    // TODO: backoff
                    continue;
                }
            }
        };

        let arc_stream = Arc::new(stream);

        let (reader, writer) = (arc_stream.clone(), arc_stream.clone());
        let (sender_incoming, receiver_incoming) = bounded(10);
        let (sender_outgoing, receiver_outgoing) = bounded(10);
        let (sender_shutdown, receiver_shutdown) = bounded(1);

        let client = Client {
            pool: pool.clone(),
            message_id: 0,
            job_sender,
            time_last_notify: None,
            time_connected: Utc::now(),
            extranonce1: extranonce_from_hex("00000000"),
            extranonce2_size: 2,
            version_rolling_mask: None,
            version_rolling_min_bit: None,
            status: ClientStatus::Init,
            last_notify: None,
            sent_authorize_request: vec![],
            authorized: vec![],
            receiver_incoming,
            sender_outgoing,
            sender_shutdown,
            shutting_down: false,
        };

        let client = Arc::new(Mutex::new(client));

        // Inbound message receive task
        let cloned = client.clone();
        task::spawn(async move {
            let mut messages = BufReader::new(&*reader).lines();
            while let Some(message) = messages.next().await {
                match message {
                    Ok(msg) => {
                        if let Err(e) = sender_incoming.send(msg).await {
                            debug!(
                                "Could not send received message into processing channel: {}",
                                e
                            );
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("could not read incoming message: {}", e);
                        break;
                    }
                }
            }
            if let Some(mut self_) = cloned.try_lock() {
                debug!("Stream with '{}' closed", self_.pool.name);
                self_.shutdown().await;
            }
        });

        // Outbound message send task
        let outbound_pool_clone = pool.clone();
        task::spawn(async move {
            loop {
                match receiver_outgoing.recv().await {
                    Ok(message) => {
                        if let Err(e) = (&*writer).write_all(message.as_bytes()).await {
                            debug!("Failed to send stratum message to '{}'. Stopping outbound message send task: {}", outbound_pool_clone.name, e);
                            break;
                        };
                    }
                    Err(e) => {
                        debug!("Outbound message receiver to '{}' is closed. Stopping outbound message send task: {}", outbound_pool_clone.name, e);
                        break;
                    }
                }
            }
            debug!(
                "Closed outbound send task to '{}'.",
                outbound_pool_clone.name
            );
        });

        // Message parsing and processing task
        let cloned = client.clone();
        task::spawn(async move {
            loop {
                if let Some(mut self_) = cloned.try_lock() {
                    let incoming = self_.receiver_incoming.try_recv();
                    self_.parse_message(incoming).await;

                    // check connection age, disconnect if connection too old
                    let duration_connected = Utc::now() - self_.time_connected;
                    let max_lifetime: u32 = self_
                        .pool
                        .max_lifetime
                        .unwrap_or(STRATUM_CONNECTION_MAX_LIFETIME_SECONDS);
                    if duration_connected > TimeDelta::seconds(max_lifetime.into()) {
                        debug!(
                            "Closing connection to {} as the connection is {:?} old (max_lifetime={}s)",
                            self_.pool.name, duration_connected, max_lifetime,
                        );
                        self_.shutdown().await;
                        break;
                    }

                    // check last job time, disconnect if too old
                    if let Some(time_last_notify) = self_.time_last_notify {
                        if time_last_notify.elapsed()
                            > Duration::from_secs(STRATUM_JOB_TIMEOUT_SECONDS)
                        {
                            warn!(
                                "No notify from {} in more than {}s: disconnecting...",
                                self_.pool.name, STRATUM_JOB_TIMEOUT_SECONDS
                            );
                            self_.shutdown().await;
                            break;
                        }
                    }
                }
                // It's healthy to sleep after giving up the lock so the other thread has a shot
                // at acquiring it - it also prevents pegging the cpu
                task::sleep(Duration::from_millis(100)).await;
            }
        });

        // initialize client loop
        loop {
            let mut client_ = client.lock().await;
            match client_.status {
                ClientStatus::Init => client_.send_configure().await,
                ClientStatus::Configured => client_.send_subscribe().await,
                ClientStatus::Subscribed => {
                    client_.send_authorize().await;
                    break;
                }
            }
            drop(client_);
            // If we already know we're shutting down, we can break here
            // and directly go into the wait-for-shutdown loop.
            if !receiver_shutdown.is_empty() {
                warn!(
                    "Existing shutdown signal during client initialization loop for pool '{}'",
                    pool.name
                );
                break;
            }
            task::sleep(Duration::from_secs(1)).await;
        }

        // Wait-for-shutdown loop
        loop {
            match receiver_shutdown.recv().await {
                Ok(_) => {
                    // Try to shut down the stream. On errors, do nothing - just terminate this client (and open a new one).
                    let _ = arc_stream.shutdown(Shutdown::Both);
                    return;
                }
                Err(e) => {
                    error!("Shutdown receiver closed before receiving shutdown: {}", e);
                    return;
                }
            }
        }
    }

    async fn parse_message(
        &mut self,
        incoming_message: Result<String, async_channel::TryRecvError>,
    ) {
        if let Ok(line) = incoming_message {
            debug!("recv from {}: {}", self.pool.name, line);
            match serde_json::from_str(&line) {
                Ok(msg) => {
                    if let Err(e) = self.handle_message(msg) {
                        warn!(
                            "could not handle '{}' message (ignoring): {}",
                            self.pool.name, e
                        );
                    }
                }
                Err(e) => {
                    warn!("could not parse message: {} - msg={}", e, line);
                }
            }
        }
    }

    async fn send_message(&mut self, msg: &json_rpc::Message) {
        let content =
            serde_json::to_string(&msg).expect("could not serialize message as JSON string");
        debug!("sending to {}: {}", self.pool.name, content);
        if let Err(e) = self.sender_outgoing.send(format!("{}\n", content)).await {
            warn!("could not send message to '{}': {}", self.pool.name, e);
            self.shutdown().await;
        } else {
            self.message_id += 1;
        }
    }

    pub async fn send_subscribe(&mut self) {
        loop {
            if let ClientStatus::Configured = self.status {
                break;
            }
        }
        let subscribe = self.subscribe(self.message_id, None).unwrap();
        self.send_message(&subscribe).await;
    }

    pub async fn send_authorize(&mut self) {
        if let Ok(authorize) = self.authorize(
            self.message_id,
            self.pool.user.clone(),
            self.pool.password.clone().unwrap_or_default(),
        ) {
            self.send_message(&authorize).await;
        }
    }

    pub async fn send_configure(&mut self) {
        let configure = self.configure(self.message_id);
        self.send_message(&configure).await;
        // since we currently don't need to configure anything, treat
        // having send the configure message as being configured.
        // This allows us to support pools that don't respond to the (optional)
        // configure message.
        self.status = ClientStatus::Configured;
    }

    pub async fn shutdown(&mut self) {
        if !self.shutting_down {
            self.sender_outgoing.close();
            self.receiver_incoming.close();
            self.sender_shutdown
                .send(true)
                .await
                .expect("Could not send shutdown");
            self.shutting_down = true;
        }
    }
}

impl<'a> IsClient<'a> for Client<'a> {
    fn handle_set_difficulty(
        &mut self,
        _conf: &mut server_to_client::SetDifficulty,
    ) -> Result<(), Error<'a>> {
        Ok(())
    }

    fn handle_set_extranonce(
        &mut self,
        _conf: &mut server_to_client::SetExtranonce,
    ) -> Result<(), Error<'a>> {
        Ok(())
    }

    fn handle_set_version_mask(
        &mut self,
        _conf: &mut server_to_client::SetVersionMask,
    ) -> Result<(), Error<'a>> {
        Ok(())
    }

    fn handle_notify(&mut self, notify: server_to_client::Notify<'a>) -> Result<(), Error<'a>> {
        self.time_last_notify = Some(Instant::now());
        self.last_notify = Some(notify.clone());
        let job_update = JobUpdate {
            timestamp: Utc::now(),
            pool: self.pool.clone(),
            job: notify.clone(),
            extranonce1: self.extranonce1.clone(),
            extranonce2_size: self.extranonce2_size,
            time_connected: self.time_connected,
        };
        if let Err(e) = self.job_sender.try_send(job_update) {
            error!("Failed to send JobUpdate for {}: {}", self.pool.name, e);
        }
        Ok(())
    }

    fn handle_configure(
        &mut self,
        _conf: &mut server_to_client::Configure,
    ) -> Result<(), Error<'a>> {
        Ok(())
    }

    fn handle_subscribe(
        &mut self,
        _subscribe: &server_to_client::Subscribe<'a>,
    ) -> Result<(), Error<'a>> {
        Ok(())
    }

    fn set_extranonce1(&mut self, extranonce1: Extranonce<'a>) {
        self.extranonce1 = extranonce1;
    }

    fn extranonce1(&self) -> Extranonce<'a> {
        self.extranonce1.clone()
    }

    fn set_extranonce2_size(&mut self, extra_nonce2_size: usize) {
        self.extranonce2_size = extra_nonce2_size;
    }

    fn extranonce2_size(&self) -> usize {
        self.extranonce2_size
    }

    fn version_rolling_mask(&self) -> Option<HexU32Be> {
        self.version_rolling_mask.clone()
    }

    fn set_version_rolling_mask(&mut self, mask: Option<HexU32Be>) {
        self.version_rolling_mask = mask;
    }

    fn set_version_rolling_min_bit(&mut self, min: Option<HexU32Be>) {
        self.version_rolling_min_bit = min;
    }

    fn set_status(&mut self, status: ClientStatus) {
        self.status = status;
    }

    fn signature(&self) -> String {
        format!("{}", USER_AGENT)
    }

    fn status(&self) -> ClientStatus {
        self.status
    }

    fn version_rolling_min_bit(&mut self) -> Option<HexU32Be> {
        self.version_rolling_min_bit.clone()
    }

    fn id_is_authorize(&mut self, id: &u64) -> Option<String> {
        let req: Vec<&(u64, String)> = self
            .sent_authorize_request
            .iter()
            .filter(|x| x.0 == *id)
            .collect();
        match req.len() {
            0 => None,
            _ => Some(req[0].1.clone()),
        }
    }

    fn id_is_submit(&mut self, _: &u64) -> bool {
        false
    }

    fn authorize_user_name(&mut self, name: String) {
        self.authorized.push(name)
    }

    fn is_authorized(&self, name: &String) -> bool {
        self.authorized.contains(name)
    }

    fn authorize(
        &mut self,
        id: u64,
        name: String,
        password: String,
    ) -> Result<json_rpc::Message, Error> {
        match self.status() {
            ClientStatus::Init => Err(Error::IncorrectClientStatus("mining.authorize".to_string())),
            _ => {
                self.sent_authorize_request.push((id, name.to_string()));
                Ok(client_to_server::Authorize { id, name, password }.into())
            }
        }
    }

    fn last_notify(&self) -> Option<server_to_client::Notify> {
        self.last_notify.clone()
    }

    fn handle_error_message(
        &mut self,
        message: sv1_api::Message,
    ) -> Result<Option<json_rpc::Message>, Error<'a>> {
        error!("{:?}", message);
        Ok(None)
    }
}
