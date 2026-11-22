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
/// How long to wait before handing a pool whose address we could not work out
/// back to the caller, so it doesn't retry in a tight loop.
const ENDPOINT_RETRY_DELAY_SECONDS: u64 = 3;

/// Why the client gives up on a connection to a pool.
#[derive(Debug, PartialEq, Eq)]
enum DisconnectReason {
    /// The connection has been open for longer than the pool allows.
    MaxLifetime,
    /// The pool has not sent a job in a long time.
    JobTimeout,
}

/// Decides whether a connection should be closed, from how long it has been
/// open and how long ago the pool sent its last job. A connection on which no
/// job has arrived yet is left alone: the handshake may still be going on.
fn should_disconnect(
    duration_connected: TimeDelta,
    max_lifetime_seconds: u32,
    time_since_last_notify: Option<Duration>,
    job_timeout: Duration,
) -> Option<DisconnectReason> {
    if duration_connected > TimeDelta::seconds(max_lifetime_seconds.into()) {
        return Some(DisconnectReason::MaxLifetime);
    }
    match time_since_last_notify {
        Some(elapsed) if elapsed > job_timeout => Some(DisconnectReason::JobTimeout),
        _ => None,
    }
}

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

impl Client<'static> {
    /// Builds a client that talks to a pool through the given channels. [Self::run]
    /// wires the channels up to a TCP connection; tests can hand the client
    /// channels of their own and feed stratum messages in without a socket.
    pub fn new(
        pool: Pool,
        job_sender: Sender<JobUpdate<'static>>,
        receiver_incoming: Receiver<String>,
        sender_outgoing: Sender<String>,
        sender_shutdown: Sender<bool>,
    ) -> Client<'static> {
        Client {
            pool,
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
        }
    }

    pub async fn run(pool: &Pool, job_sender: Sender<JobUpdate<'static>>) {
        // We can't do anything without an address to connect to. Return, so
        // the caller can call us again - the endpoint may be a name that only
        // resolves later, or the configuration may be fixed in the meantime.
        let socket = match pool.endpoint.to_socket_addrs() {
            Ok(mut addresses) => match addresses.next() {
                Some(socket) => socket,
                None => {
                    error!(
                        "No address found for pool '{}' ({}).",
                        pool.name, pool.endpoint
                    );
                    task::sleep(Duration::from_secs(ENDPOINT_RETRY_DELAY_SECONDS)).await;
                    return;
                }
            },
            Err(e) => {
                error!(
                    "Could not look up the address of pool '{}' ({}): {}",
                    pool.name, pool.endpoint, e
                );
                task::sleep(Duration::from_secs(ENDPOINT_RETRY_DELAY_SECONDS)).await;
                return;
            }
        };

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

        let client = Client::new(
            pool.clone(),
            job_sender,
            receiver_incoming,
            sender_outgoing,
            sender_shutdown,
        );

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
            Client::shut_down_shared(&cloned).await;
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
                    // The client has already been stopped elsewhere, for
                    // example because the pool closed the connection. Hand on
                    // the messages that did arrive before stopping.
                    if self_.shutting_down && self_.receiver_incoming.is_empty() {
                        break;
                    }
                    let incoming = self_.receiver_incoming.try_recv();
                    self_.parse_message(incoming).await;

                    // close the connection when it has been open for too long
                    // or when the pool stopped sending jobs
                    let duration_connected = Utc::now() - self_.time_connected;
                    let max_lifetime: u32 = self_
                        .pool
                        .max_lifetime
                        .unwrap_or(STRATUM_CONNECTION_MAX_LIFETIME_SECONDS);
                    let job_timeout = Duration::from_secs(STRATUM_JOB_TIMEOUT_SECONDS);
                    if let Some(reason) = should_disconnect(
                        duration_connected,
                        max_lifetime,
                        self_.time_last_notify.map(|time| time.elapsed()),
                        job_timeout,
                    ) {
                        match reason {
                            DisconnectReason::MaxLifetime => debug!(
                                "Closing connection to {} as the connection is {:?} old (max_lifetime={}s)",
                                self_.pool.name, duration_connected, max_lifetime,
                            ),
                            DisconnectReason::JobTimeout => warn!(
                                "No notify from {} in more than {}s: disconnecting...",
                                self_.pool.name, STRATUM_JOB_TIMEOUT_SECONDS
                            ),
                        }
                        self_.shutdown();
                        break;
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

    /// Shuts down a client that the tasks of a connection share. Waits for the
    /// client to be free: skipping the shutdown because another task happens to
    /// hold it would leave us sitting on a connection that is already gone.
    /// Waiting is safe, as the other tasks only hold the client briefly.
    async fn shut_down_shared(client: &Arc<Mutex<Client<'static>>>) {
        let mut client = client.lock().await;
        debug!("Stopping the client for '{}'", client.pool.name);
        client.shutdown();
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
            self.shutdown();
        } else {
            self.message_id += 1;
        }
    }

    pub async fn send_subscribe(&mut self) {
        // Subscribing before the configure is an error, which we report rather
        // than wait out: nothing here can change the status of the client.
        let subscribe = self.subscribe(self.message_id, None);
        match subscribe {
            Ok(subscribe) => self.send_message(&subscribe).await,
            Err(e) => warn!(
                "could not build the subscribe message for '{}': {}",
                self.pool.name, e
            ),
        }
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
}

impl<'a> Client<'a> {
    /// Stops the client: the tasks around the connection wind down and
    /// [Client::run] returns, so the caller can open a new connection. Takes
    /// no await, so the message handlers can call it too.
    pub fn shutdown(&mut self) {
        if !self.shutting_down {
            self.sender_outgoing.close();
            self.receiver_incoming.close();
            // The channel takes one shutdown and we only ever send one, so an
            // error here means nobody is waiting for it any more.
            if let Err(e) = self.sender_shutdown.try_send(true) {
                debug!(
                    "Nobody to tell that the client for '{}' is stopping: {}",
                    self.pool.name, e
                );
            }
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
        conf: &mut server_to_client::SetExtranonce,
    ) -> Result<(), Error<'a>> {
        // These replace what the pool handed out when we subscribed. Every
        // coinbase from the next job on is built with them, so we have to take
        // them over - sticking to the old ones would make the coinbases we
        // report differ from the ones that are being mined.
        let extranonce1: Vec<u8> = conf.extra_nonce1.clone().into();
        debug!(
            "{} handed out a new extranonce1={} with extranonce2_size={}",
            self.pool.name,
            utils::encode_hex(&extranonce1),
            conf.extra_nonce2_size,
        );
        self.set_extranonce1(Extranonce::try_from(extranonce1)?);
        self.set_extranonce2_size(conf.extra_nonce2_size);
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
        match self.job_sender.try_send(job_update) {
            Ok(()) => {}
            // Nobody is reading the jobs any more, so there is no point in
            // staying connected to the pool and reporting this for every job
            // that comes in.
            Err(async_channel::TrySendError::Closed(_)) => {
                error!(
                    "Nobody is reading the jobs of {} any more: stopping the client",
                    self.pool.name
                );
                self.shutdown();
            }
            Err(e) => error!("Failed to send JobUpdate for {}: {}", self.pool.name, e),
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
    ) -> Result<json_rpc::Message, Error<'_>> {
        match self.status() {
            ClientStatus::Init => Err(Error::IncorrectClientStatus("mining.authorize".to_string())),
            _ => {
                self.sent_authorize_request.push((id, name.to_string()));
                Ok(client_to_server::Authorize { id, name, password }.into())
            }
        }
    }

    fn last_notify(&self) -> Option<server_to_client::Notify<'_>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::test_support::test_pool;
    use crate::utils::encode_hex;
    use async_channel::unbounded;

    /// A `mining.notify` as a pool sends it. The coinbase parts are made up,
    /// the other fields are shaped the way a real pool shapes them.
    const NOTIFY: &str = concat!(
        r#"{"id":null,"method":"mining.notify","params":["job1","#,
        r#""9fd65203a7a1642aa5516c3cf2cd81fc6204ab560002c64b0000000000000000","#,
        r#""0100000001000000","00ffffffff00000000","#,
        r#"["00112233445566778899aabbccddeeff0f1e2d3c4b5a69788796a5b4c3d2e1f0"],"#,
        r#""20000000","17031abe","66bca309",true]}"#
    );
    /// The answer a pool gives to `mining.subscribe`: the subscriptions, the
    /// extranonce1 it hands out and the size it expects the extranonce2 to have.
    const SUBSCRIBE_RESPONSE: &str =
        r#"{"id":1,"error":null,"result":[[["mining.notify","abc"]],"f11f9113",4]}"#;
    /// A pool that has nothing to configure answers with an empty result.
    const CONFIGURE_RESPONSE: &str = r#"{"id":0,"error":null,"result":{}}"#;
    /// A pool that lets miners roll the version answers the configure with the
    /// mask of the bits it allows and the number of bits it wants rolled.
    const CONFIGURE_RESPONSE_VERSION_ROLLING: &str = concat!(
        r#"{"id":0,"error":null,"result":{"version-rolling":true,"#,
        r#""version-rolling.mask":"1fffe000","#,
        r#""version-rolling.min-bit-count":"00000005"}}"#
    );
    /// A pool handing out a new extranonce in the middle of a session.
    const SET_EXTRANONCE: &str =
        r#"{"id":null,"method":"mining.set_extranonce","params":["aabbccddee",2]}"#;

    /// A client plus the ends of its channels a test holds on to. Dropping an
    /// end would close the channel, so they all have to stay alive.
    struct Harness {
        client: Client<'static>,
        job_receiver: Receiver<JobUpdate<'static>>,
        #[allow(dead_code)]
        sender_incoming: Sender<String>,
        receiver_outgoing: Receiver<String>,
        receiver_shutdown: Receiver<bool>,
    }

    fn harness() -> Harness {
        let (job_sender, job_receiver) = unbounded();
        let (sender_incoming, receiver_incoming) = unbounded();
        let (sender_outgoing, receiver_outgoing) = unbounded();
        let (sender_shutdown, receiver_shutdown) = bounded(1);
        Harness {
            client: Client::new(
                test_pool(),
                job_sender,
                receiver_incoming,
                sender_outgoing,
                sender_shutdown,
            ),
            job_receiver,
            sender_incoming,
            receiver_outgoing,
            receiver_shutdown,
        }
    }

    /// Parses a line the way the client does before handing it to the handlers.
    fn message(line: &str) -> json_rpc::Message {
        serde_json::from_str(line).expect("the test message should be valid JSON")
    }

    /// A message as JSON, the way it goes over the wire to the pool.
    fn request_json(message: &json_rpc::Message) -> serde_json::Value {
        serde_json::to_value(message).expect("a message should serialize")
    }

    /// Reads the id and the method of everything the client has sent so far.
    fn sent_requests(receiver: &Receiver<String>) -> Vec<(u64, String)> {
        let mut requests = vec![];
        while let Ok(line) = receiver.try_recv() {
            let message: json_rpc::Message =
                serde_json::from_str(&line).expect("the client should send valid JSON");
            match message {
                json_rpc::Message::StandardRequest(request) => {
                    requests.push((request.id, request.method))
                }
                other => panic!("the client should only send requests, sent {:?}", other),
            }
        }
        requests
    }

    #[test]
    fn a_configure_response_configures_the_client() {
        let mut h = harness();
        assert_eq!(h.client.status, ClientStatus::Init);

        let answer = h
            .client
            .handle_message(message(CONFIGURE_RESPONSE))
            .expect("a configure response should be handled");

        assert_eq!(h.client.status, ClientStatus::Configured);
        // The library answers a configure response with a subscribe message. We
        // send our own subscribe, so we ignore it - it just must not be an error.
        assert!(answer.is_some());

        // A pool that configures nothing leaves the version rolling off, so the
        // next configure asks for nothing again.
        assert_eq!(h.client.version_rolling_mask(), None);
        assert_eq!(h.client.version_rolling_min_bit(), None);
        assert_eq!(
            request_json(&h.client.configure(4)),
            serde_json::json!({
                "id": 4,
                "method": "mining.configure",
                "params": [[], {}],
            })
        );
    }

    /// What a pool answers to the configure decides how the miners on it may
    /// roll the version of the jobs we report, so the client keeps the settings
    /// and asks for the same ones again on its next connection.
    #[test]
    fn a_configure_response_sets_the_version_rolling() {
        let mut h = harness();
        assert_eq!(h.client.version_rolling_mask(), None);
        assert_eq!(h.client.version_rolling_min_bit(), None);

        h.client
            .handle_message(message(CONFIGURE_RESPONSE_VERSION_ROLLING))
            .expect("a configure response should be handled");

        assert_eq!(h.client.version_rolling_mask(), Some(HexU32Be(0x1fffe000)));
        assert_eq!(h.client.version_rolling_min_bit(), Some(HexU32Be(5)));
        assert_eq!(h.client.status, ClientStatus::Configured);

        // The next configure asks the pool for what it handed out.
        assert_eq!(
            request_json(&h.client.configure(9)),
            serde_json::json!({
                "id": 9,
                "method": "mining.configure",
                "params": [
                    ["version-rolling"],
                    {
                        "version-rolling.mask": "1fffe000",
                        "version-rolling.min-bit-count": "00000005",
                    },
                ],
            })
        );
    }

    #[test]
    fn a_subscribe_response_sets_the_extranonce() {
        let mut h = harness();
        h.client.status = ClientStatus::Configured;

        h.client
            .handle_message(message(SUBSCRIBE_RESPONSE))
            .expect("a subscribe response should be handled");

        assert_eq!(encode_hex(h.client.extranonce1.as_ref()), "f11f9113");
        assert_eq!(h.client.extranonce2_size, 4);
        assert_eq!(h.client.status, ClientStatus::Subscribed);
        // Both are needed to build the coinbase of a job, so they are also what
        // the client reports to whoever asks it.
        assert_eq!(encode_hex(h.client.extranonce1().as_ref()), "f11f9113");
        assert_eq!(IsClient::extranonce2_size(&h.client), 4);
    }

    #[test]
    fn a_notify_becomes_a_single_job_update() {
        let mut h = harness();
        h.client.status = ClientStatus::Configured;
        h.client
            .handle_message(message(SUBSCRIBE_RESPONSE))
            .expect("a subscribe response should be handled");
        assert!(h.client.time_last_notify.is_none());
        assert!(h.client.last_notify.is_none());

        h.client
            .handle_message(message(NOTIFY))
            .expect("a notify should be handled");

        assert_eq!(h.job_receiver.len(), 1, "a notify is one job update");
        let job = h.job_receiver.try_recv().expect("there should be a job");
        assert_eq!(job.pool, test_pool());
        // The job carries what the pool handed out when we subscribed, as the
        // coinbase can only be put back together with both.
        assert_eq!(encode_hex(job.extranonce1.as_ref()), "f11f9113");
        assert_eq!(job.extranonce2_size, 4);
        assert_eq!(job.job.job_id, "job1");
        assert_eq!(job.job.version, HexU32Be(0x20000000));
        assert_eq!(job.job.bits, HexU32Be(0x17031abe));
        assert_eq!(job.job.time, HexU32Be(0x66bca309));
        assert!(job.job.clean_jobs);
        assert_eq!(job.job.merkle_branch.len(), 1);
        assert_eq!(job.time_connected, h.client.time_connected);

        // The client remembers the job, so it knows when the last one came in
        // and can say which job the pool is working on.
        assert!(h.client.time_last_notify.is_some());
        let remembered = h.client.last_notify.clone().expect("a remembered job");
        assert_eq!(remembered.job_id, "job1");
        let reported = IsClient::last_notify(&h.client).expect("the last job is reported");
        assert_eq!(reported.job_id, "job1");
        assert_eq!(reported.version, HexU32Be(0x20000000));
    }

    #[test]
    fn messages_we_have_nothing_to_do_with_are_accepted() {
        let mut h = harness();
        h.client.status = ClientStatus::Configured;
        h.client
            .handle_message(message(SUBSCRIBE_RESPONSE))
            .expect("a subscribe response should be handled");

        for line in [
            r#"{"id":null,"method":"mining.set_difficulty","params":[1024]}"#,
            r#"{"id":null,"method":"mining.set_difficulty","params":[0.5]}"#,
            r#"{"id":null,"method":"mining.set_version_mask","params":["1fffe000"]}"#,
        ] {
            h.client
                .handle_message(message(line))
                .unwrap_or_else(|e| panic!("{} should be handled: {}", line, e));
        }

        // None of them changes what we need to build coinbases, and none of
        // them produces a job.
        assert_eq!(encode_hex(h.client.extranonce1.as_ref()), "f11f9113");
        assert_eq!(h.client.extranonce2_size, 4);
        assert_eq!(h.client.status, ClientStatus::Subscribed);
        assert!(h.job_receiver.is_empty());
        assert!(h.receiver_shutdown.is_empty());
    }

    #[test]
    fn authorizing_only_works_once_the_client_is_configured() {
        let mut h = harness();
        let error = h
            .client
            .authorize(5, "user".to_string(), "password".to_string())
            .expect_err("we can not authorize before being configured");
        assert!(matches!(error, Error::IncorrectClientStatus(_)));
        assert_eq!(h.client.id_is_authorize(&5), None);

        h.client.status = ClientStatus::Configured;
        h.client
            .authorize(7, "user".to_string(), "password".to_string())
            .expect("we can authorize once configured");
        assert_eq!(h.client.id_is_authorize(&7), Some("user".to_string()));
        assert!(!h.client.is_authorized(&"user".to_string()));

        h.client
            .handle_message(message(r#"{"id":7,"error":null,"result":true}"#))
            .expect("an authorize response should be handled");
        assert!(h.client.is_authorized(&"user".to_string()));
    }

    #[async_std::test]
    async fn a_line_that_is_not_json_is_ignored() {
        let mut h = harness();
        h.client
            .parse_message(Ok("not json at all".to_string()))
            .await;
        h.client.parse_message(Ok(String::new())).await;
        h.client
            .parse_message(Ok(
                r#"{"method":"mining.notify","params":"not an array"}"#.to_string()
            ))
            .await;
        // An empty channel is not a message either.
        h.client
            .parse_message(Err(async_channel::TryRecvError::Empty))
            .await;

        assert!(h.job_receiver.is_empty(), "no job should come out of this");
        assert!(
            h.receiver_shutdown.is_empty(),
            "a line we can not read is no reason to give up on the pool"
        );
        assert!(!h.client.shutting_down);
    }

    #[async_std::test]
    async fn an_error_response_is_ignored() {
        let mut h = harness();
        h.client
            .parse_message(Ok(
                r#"{"id":3,"result":null,"error":{"code":-1,"message":"boom","data":null}}"#
                    .to_string(),
            ))
            .await;
        assert!(h.job_receiver.is_empty());
        assert!(h.receiver_shutdown.is_empty());
        assert!(!h.client.shutting_down);
    }

    /// The timeouts the client runs with when a pool does not ask for a
    /// shorter connection lifetime.
    fn default_timeouts() -> (u32, Duration) {
        (
            STRATUM_CONNECTION_MAX_LIFETIME_SECONDS,
            Duration::from_secs(STRATUM_JOB_TIMEOUT_SECONDS),
        )
    }

    #[test]
    fn a_young_connection_with_fresh_jobs_stays_open() {
        let (max_lifetime, job_timeout) = default_timeouts();
        assert_eq!(
            should_disconnect(TimeDelta::zero(), max_lifetime, None, job_timeout),
            None
        );
        assert_eq!(
            should_disconnect(
                TimeDelta::seconds(300),
                max_lifetime,
                Some(Duration::from_secs(10)),
                job_timeout
            ),
            None
        );
        // Being exactly at a limit is not being past it.
        assert_eq!(
            should_disconnect(
                TimeDelta::seconds(590),
                max_lifetime,
                Some(Duration::from_secs(60)),
                job_timeout
            ),
            None
        );
    }

    #[test]
    fn a_connection_that_is_too_old_is_closed() {
        let (max_lifetime, job_timeout) = default_timeouts();
        assert_eq!(
            should_disconnect(TimeDelta::seconds(591), max_lifetime, None, job_timeout),
            Some(DisconnectReason::MaxLifetime)
        );
        // A pool can ask for a shorter lifetime than the default.
        assert_eq!(
            should_disconnect(TimeDelta::seconds(2), 1, None, job_timeout),
            Some(DisconnectReason::MaxLifetime)
        );
        // When both reasons apply, the age of the connection is the one reported.
        assert_eq!(
            should_disconnect(
                TimeDelta::seconds(591),
                max_lifetime,
                Some(Duration::from_secs(600)),
                job_timeout
            ),
            Some(DisconnectReason::MaxLifetime)
        );
    }

    #[test]
    fn a_connection_without_new_jobs_is_closed() {
        let (max_lifetime, job_timeout) = default_timeouts();
        assert_eq!(
            should_disconnect(
                TimeDelta::seconds(100),
                max_lifetime,
                Some(Duration::from_secs(61)),
                job_timeout
            ),
            Some(DisconnectReason::JobTimeout)
        );
        // As long as no job has arrived at all we wait: the handshake with the
        // pool may still be going on.
        assert_eq!(
            should_disconnect(TimeDelta::seconds(300), max_lifetime, None, job_timeout),
            None
        );
    }

    /// A pool can hand out a new extranonce in the middle of a session. Every
    /// coinbase from the next job on is built with it, so the client has to
    /// take it over - keeping the old one would make the coinbases it reports
    /// differ from the ones that are being mined.
    #[test]
    fn a_new_extranonce_replaces_the_one_from_the_subscribe() {
        let mut h = harness();
        h.client.status = ClientStatus::Configured;
        h.client
            .handle_message(message(SUBSCRIBE_RESPONSE))
            .expect("a subscribe response should be handled");

        h.client
            .handle_message(message(SET_EXTRANONCE))
            .expect("a set_extranonce should be handled");

        assert_eq!(encode_hex(h.client.extranonce1.as_ref()), "aabbccddee");
        assert_eq!(h.client.extranonce2_size, 2);

        // The next job is reported with the new values.
        h.client
            .handle_message(message(NOTIFY))
            .expect("a notify should be handled");
        let job = h.job_receiver.try_recv().expect("there should be a job");
        assert_eq!(encode_hex(job.extranonce1.as_ref()), "aabbccddee");
        assert_eq!(job.extranonce2_size, 2);
    }

    /// Handing on the jobs is the whole point of the connection, so a client
    /// whose jobs go nowhere stops rather than reporting the same problem for
    /// every job that comes in.
    #[test]
    fn a_client_whose_jobs_go_nowhere_stops() {
        let mut h = harness();
        h.client.status = ClientStatus::Configured;
        h.client
            .handle_message(message(SUBSCRIBE_RESPONSE))
            .expect("a subscribe response should be handled");
        h.job_receiver.close();

        h.client
            .handle_message(message(NOTIFY))
            .expect("the notify itself is fine");

        assert!(h.client.shutting_down);
        assert!(
            !h.receiver_shutdown.is_empty(),
            "the shutdown should have been signalled"
        );
        assert!(
            h.receiver_outgoing.is_closed(),
            "the outbound send task should be stopped"
        );
    }

    /// The connection to the pool is gone, so the client has to be shut down.
    /// Another task may well be working with the client at that moment, which
    /// must not keep the shutdown from happening.
    #[async_std::test]
    async fn the_client_is_shut_down_even_while_it_is_busy() {
        let Harness {
            client,
            receiver_outgoing,
            receiver_shutdown,
            job_receiver: _job_receiver,
            sender_incoming: _sender_incoming,
        } = harness();
        let client = Arc::new(Mutex::new(client));

        // Somebody else is working with the client right now ...
        let busy = client.lock().await;
        let shutting_down = task::spawn({
            let client = client.clone();
            async move { Client::shut_down_shared(&client).await }
        });
        // ... and hands it over a moment later.
        task::sleep(Duration::from_millis(100)).await;
        drop(busy);

        async_std::future::timeout(Duration::from_secs(5), shutting_down)
            .await
            .expect("the shutdown should go through once the client is free again");

        assert!(
            !receiver_shutdown.is_empty(),
            "the shutdown should have been signalled"
        );
        assert!(
            receiver_outgoing.is_closed(),
            "the outbound send task should be stopped"
        );
        assert!(client.lock().await.shutting_down);
    }

    /// Subscribing is only allowed once the client is configured. Being asked
    /// to do it earlier is a mistake we report, not one we wait out.
    #[async_std::test]
    async fn subscribing_before_the_configure_returns() {
        let Harness {
            mut client,
            receiver_outgoing,
            receiver_shutdown,
            job_receiver: _job_receiver,
            sender_incoming: _sender_incoming,
        } = harness();
        assert_eq!(client.status, ClientStatus::Init);

        // Run it elsewhere: a client that waits for a status that can not
        // change would otherwise hang this test instead of failing it.
        let subscribing = task::spawn(async move {
            client.send_subscribe().await;
            client
        });
        let client = async_std::future::timeout(Duration::from_secs(2), subscribing)
            .await
            .expect("send_subscribe should return instead of waiting for a status change");

        assert_eq!(client.status, ClientStatus::Init);
        assert_eq!(client.message_id, 0, "nothing was sent");
        assert!(receiver_outgoing.is_empty());
        assert!(receiver_shutdown.is_empty());
    }

    #[async_std::test]
    async fn every_message_gets_its_own_id() {
        let mut h = harness();
        assert_eq!(h.client.message_id, 0);

        h.client.send_configure().await;
        assert_eq!(h.client.message_id, 1);
        h.client.send_subscribe().await;
        assert_eq!(h.client.message_id, 2);
        h.client.send_authorize().await;
        assert_eq!(h.client.message_id, 3);

        assert_eq!(
            sent_requests(&h.receiver_outgoing),
            vec![
                (0, "mining.configure".to_string()),
                (1, "mining.subscribe".to_string()),
                (2, "mining.authorize".to_string()),
            ]
        );
    }
}
