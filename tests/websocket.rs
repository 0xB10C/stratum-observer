//! Tests for the websocket server that sends jobs to the connected clients.

mod common;

use async_std::future::timeout;
use async_std::task;
use common::*;
use serde_json::Value;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;
use stratum_observer::types::{JobUpdate, JobUpdateJson};
use stratum_observer::websocket::websocket_sender_task_with_listener;
use tungstenite::stream::MaybeTlsStream;

/// How long a test waits for something that should happen right away.
const WAIT: Duration = Duration::from_secs(5);
/// How long a test waits to be sure that nothing else arrives.
const QUIET: Duration = Duration::from_millis(250);

/// Number of panics that happened anywhere in this test process. A panic in a
/// spawned task does not fail a test by itself, so a test that wants to know
/// whether a task panicked counts them.
static PANICS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn count_panics() -> usize {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            previous_hook(info);
        }));
    });
    PANICS.load(std::sync::atomic::Ordering::SeqCst)
}

/// A websocket client. It reads the messages on a thread of its own, because
/// reading from a websocket blocks, and hands them to the test through a
/// channel.
struct TestClient {
    messages: async_channel::Receiver<String>,
    /// A second handle on the TCP connection, so the test can hang up.
    connection: TcpStream,
}

impl TestClient {
    async fn connect(addr: SocketAddr) -> TestClient {
        let (message_sender, messages) = async_channel::unbounded();
        let (connection_sender, connection_receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let (mut websocket, _) = match tungstenite::connect(format!("ws://{}", addr)) {
                Ok(c) => c,
                Err(e) => panic!("the client should be able to connect to {}: {}", addr, e),
            };
            let connection = match websocket.get_ref() {
                MaybeTlsStream::Plain(stream) => stream
                    .try_clone()
                    .expect("a second handle on the connection"),
                _ => panic!("the test connects without TLS"),
            };
            if connection_sender.send_blocking(connection).is_err() {
                return;
            }
            loop {
                match websocket.read() {
                    Ok(tungstenite::Message::Text(msg)) => {
                        if message_sender.send_blocking(msg).is_err() {
                            return;
                        }
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }
        });
        let connection = timeout(WAIT, connection_receiver.recv())
            .await
            .expect("connecting should not take long")
            .expect("the client thread should hand over the connection");
        TestClient {
            messages,
            connection,
        }
    }

    /// The next message the server sent, parsed as JSON.
    async fn next_message(&self) -> Value {
        let msg = timeout(WAIT, self.messages.recv())
            .await
            .expect("the client should receive a message")
            .expect("the client should still be connected");
        serde_json::from_str(&msg).expect("every message should be JSON")
    }

    /// All messages the server sent until it went quiet.
    async fn messages_until_quiet(&self) -> Vec<Value> {
        let mut messages = Vec::new();
        while let Ok(Ok(msg)) = timeout(QUIET, self.messages.recv()).await {
            messages.push(serde_json::from_str(&msg).expect("every message should be JSON"));
        }
        messages
    }

    async fn assert_quiet(&self) {
        let messages = self.messages_until_quiet().await;
        assert!(
            messages.is_empty(),
            "the client should not have received more messages, got {:?}",
            messages
        );
    }

    fn hang_up(self) {
        let _ = self.connection.shutdown(std::net::Shutdown::Both);
    }
}

/// Starts the websocket task on a port the operating system picks and returns
/// the address it listens on together with the channel jobs go into.
fn start_server() -> (SocketAddr, async_channel::Sender<JobUpdate<'static>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("binding a free port should work");
    let addr = listener.local_addr().expect("the port that was picked");
    let (sender, receiver) = async_channel::unbounded();
    task::spawn(websocket_sender_task_with_listener(receiver, listener));
    (addr, sender)
}

/// Connects a client, collects the jobs the server sends right away and hangs
/// up again.
async fn job_snapshot(addr: SocketAddr) -> Vec<Value> {
    let client = TestClient::connect(addr).await;
    let messages = client.messages_until_quiet().await;
    client.hang_up();
    messages
}

/// Waits until a job by the given pool is one of the jobs a new client gets,
/// which means the server has read it from the channel.
async fn wait_until_known(addr: SocketAddr, pool_name: &str) -> Vec<Value> {
    for _ in 0..40 {
        let snapshot = job_snapshot(addr).await;
        if snapshot
            .iter()
            .any(|job| job["pool_name"] == Value::from(pool_name))
        {
            return snapshot;
        }
    }
    panic!(
        "the server never sent a job by '{}' to a new client",
        pool_name
    );
}

/// Connects a client and waits until it is known to the server, so that jobs
/// sent afterwards reach it. The server has to know at least one job already.
async fn connect_subscribed(addr: SocketAddr) -> TestClient {
    let client = TestClient::connect(addr).await;
    // Every new client first gets the most recent job per pool. Once those
    // arrived, the server has the client in its list of subscribers.
    let snapshot = client.messages_until_quiet().await;
    assert!(
        !snapshot.is_empty(),
        "a new client should get the jobs the server knows"
    );
    client
}

#[async_std::test]
async fn test_job_reaches_a_connected_client() {
    let (addr, sender) = start_server();
    let pool = unique_name("pool");
    let warmup = unique_name("warmup");

    // Make sure the client is subscribed before the job under test is sent.
    sender.send(real_block_job(&warmup)).await.unwrap();
    wait_until_known(addr, &warmup).await;
    let client = connect_subscribed(addr).await;

    let job = real_block_job(&pool);
    sender.send(job.clone()).await.unwrap();

    let message = client.next_message().await;
    // The message is exactly the JSON the web page in www/index.html reads.
    assert_eq!(
        message,
        serde_json::to_value(JobUpdateJson::from(job)).unwrap()
    );
    for field in [
        "pool_name",
        "prev_hash",
        "coinbase_tag",
        "height",
        "coinbase_sum",
        "job_timestamp",
        "header_version",
        "header_time",
        "header_bits",
        "merkle_branches",
        "clean_jobs",
    ] {
        assert!(
            message.get(field).is_some(),
            "the message is missing the field {}",
            field
        );
    }
    assert_eq!(message["pool_name"], Value::from(pool.as_str()));
    assert_eq!(message["prev_hash"], PREV_HASH_DISPLAYED);
    assert_eq!(message["coinbase_tag"], COINBASE_TAG);
    assert_eq!(message["height"], BLOCK_HEIGHT);
    assert_eq!(message["coinbase_sum"], COINBASE_VALUE);
    assert_eq!(message["header_version"], HEADER_VERSION);
    assert_eq!(message["header_bits"], HEADER_BITS);
    assert_eq!(message["header_time"], HEADER_TIME);
    assert_eq!(message["clean_jobs"], true);
    assert_eq!(
        message["merkle_branches"],
        serde_json::json!([MERKLE_BRANCH[0], MERKLE_BRANCH[1]])
    );
    // The job was sent once, so it should not arrive a second time.
    client.assert_quiet().await;
}

#[async_std::test]
async fn test_new_client_gets_the_latest_job_of_every_pool() {
    let (addr, sender) = start_server();
    let pool_a = unique_name("pool-a");
    let pool_b = unique_name("pool-b");

    // Two jobs by the same pool. Only the newer one should be kept.
    let mut old_job = real_block_job(&pool_a);
    old_job.timestamp = old_job.timestamp - chrono::TimeDelta::seconds(60);
    sender.send(old_job).await.unwrap();
    let new_job = real_block_job(&pool_a);
    sender.send(new_job.clone()).await.unwrap();
    let job_b = real_block_job(&pool_b);
    sender.send(job_b.clone()).await.unwrap();

    // Both jobs arrive at the server one after the other, so wait for the one
    // sent last.
    let mut snapshot = wait_until_known(addr, &pool_b).await;
    snapshot.sort_by_key(|job| job["pool_name"].as_str().unwrap_or_default().to_string());

    assert_eq!(
        snapshot.len(),
        2,
        "a new client should get one job per pool, got {:?}",
        snapshot
    );
    assert_eq!(
        snapshot[0],
        serde_json::to_value(JobUpdateJson::from(new_job.clone())).unwrap()
    );
    assert_eq!(
        snapshot[1],
        serde_json::to_value(JobUpdateJson::from(job_b)).unwrap()
    );
    assert_eq!(
        snapshot[0]["job_timestamp"],
        new_job.timestamp.timestamp(),
        "the older job by the same pool should have been replaced"
    );
}

#[async_std::test]
async fn test_two_clients_both_get_a_new_job() {
    let (addr, sender) = start_server();
    let pool = unique_name("pool");
    let warmup = unique_name("warmup");

    sender.send(real_block_job(&warmup)).await.unwrap();
    wait_until_known(addr, &warmup).await;
    let first = connect_subscribed(addr).await;
    let second = connect_subscribed(addr).await;

    sender.send(real_block_job(&pool)).await.unwrap();

    assert_eq!(
        first.next_message().await["pool_name"],
        Value::from(pool.as_str())
    );
    assert_eq!(
        second.next_message().await["pool_name"],
        Value::from(pool.as_str())
    );
}

#[async_std::test]
async fn test_a_leaving_client_does_not_stop_the_others() {
    let (addr, sender) = start_server();
    let pool = unique_name("pool");
    let warmup = unique_name("warmup");

    sender.send(real_block_job(&warmup)).await.unwrap();
    wait_until_known(addr, &warmup).await;
    let leaving = connect_subscribed(addr).await;
    let staying = connect_subscribed(addr).await;

    leaving.hang_up();

    // The server only notices the closed connection while sending, so send
    // two jobs: the first one runs into the closed connection, the second one
    // shows that this did not take the server down with it.
    for _ in 0..2 {
        sender.send(real_block_job(&pool)).await.unwrap();
        assert_eq!(
            staying.next_message().await["pool_name"],
            Value::from(pool.as_str())
        );
    }

    // New clients are still served, too.
    let joining = connect_subscribed(addr).await;
    sender.send(real_block_job(&pool)).await.unwrap();
    assert_eq!(
        joining.next_message().await["pool_name"],
        Value::from(pool.as_str())
    );
}

#[async_std::test]
async fn test_closed_job_channel_does_not_crash_the_server() {
    let panics_before = count_panics();
    let (addr, sender) = start_server();
    let warmup = unique_name("warmup");

    sender.send(real_block_job(&warmup)).await.unwrap();
    wait_until_known(addr, &warmup).await;

    // Nobody sends jobs anymore, for example because the main task stopped.
    drop(sender);
    task::sleep(Duration::from_millis(100)).await;

    assert_eq!(
        count_panics(),
        panics_before,
        "the closed job channel should not have made a task panic"
    );

    // The server should not have gone down over it and still serves the jobs
    // it knows to a new client.
    let snapshot = job_snapshot(addr).await;
    assert_eq!(
        snapshot.len(),
        1,
        "a new client should still get the known job, got {:?}",
        snapshot
    );
    assert_eq!(snapshot[0]["pool_name"], Value::from(warmup.as_str()));
}
