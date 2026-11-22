//! Tests that run the stratum client against a fake pool.

mod fake_pool;

use async_channel::{unbounded, Receiver};
use async_std::future::timeout;
use async_std::task;
use fake_pool::{Behaviour, Configure, FakePool};
use std::time::Duration;
use stratum_observer::client::Client;
use stratum_observer::types::{JobUpdate, Pool};
use stratum_observer::utils::encode_hex;
use sv1_api::json_rpc;

const POOL_USER: &str = "test.worker";
const POOL_PASSWORD: &str = "a password";

/// How long a test waits for something that should happen right away.
const SOON: Duration = Duration::from_secs(10);

fn pool(name: &str, endpoint: &str, max_lifetime: Option<u32>) -> Pool {
    Pool {
        name: name.to_string(),
        endpoint: endpoint.to_string(),
        user: POOL_USER.to_string(),
        password: Some(POOL_PASSWORD.to_string()),
        max_lifetime,
    }
}

/// Starts a client against the given pool in the background and hands back the
/// end of the channel the jobs arrive on.
fn run_client(pool: Pool) -> Receiver<JobUpdate<'static>> {
    let (job_sender, job_receiver) = unbounded();
    task::spawn(async move {
        Client::run(&pool, job_sender).await;
    });
    job_receiver
}

/// Waits for the next request the client sends to the pool.
async fn next_request(server: &FakePool) -> json_rpc::StandardRequest {
    let line = timeout(SOON, server.requests.recv())
        .await
        .expect("the client should send a request before the timeout")
        .expect("the request channel should still be open");
    serde_json::from_str(&line)
        .unwrap_or_else(|e| panic!("the client should send a stratum request: {} - {}", e, line))
}

/// Waits for the next job the client hands on.
async fn next_job(jobs: &Receiver<JobUpdate<'static>>) -> JobUpdate<'static> {
    timeout(SOON, jobs.recv())
        .await
        .expect("a job should arrive before the timeout")
        .expect("the job channel should still be open")
}

#[async_std::test]
async fn a_job_from_the_pool_becomes_a_job_update() {
    let server = FakePool::start(Behaviour::Stratum(Configure::Answer)).await;
    let jobs = run_client(pool("Fake Pool", &server.address, Some(15)));

    let job = next_job(&jobs).await;
    assert_eq!(job.pool.name, "Fake Pool");
    // The extranonce1 and its size come from the subscribe answer, the rest
    // from the job itself.
    assert_eq!(encode_hex(job.extranonce1.as_ref()), fake_pool::EXTRANONCE1);
    assert_eq!(job.extranonce2_size, fake_pool::EXTRANONCE2_SIZE);
    assert_eq!(job.job.job_id, fake_pool::JOB_ID);
    assert!(job.job.clean_jobs);
    assert_eq!(job.job.version.0, fake_pool::HEADER_VERSION);
    assert_eq!(job.job.bits.0, fake_pool::HEADER_BITS);
    assert_eq!(job.job.time.0, fake_pool::HEADER_TIME);
    assert_eq!(job.job.merkle_branch.len(), 2);

    // The two coinbase parts plus the extranonces add up to a coinbase we can
    // read the height, the tag and the payout from.
    assert!(job.coinbase().is_ok(), "the coinbase should deserialize");
    let coinbase = job.coinbase_info();
    assert_eq!(coinbase.height, fake_pool::COINBASE_HEIGHT);
    assert_eq!(coinbase.tag, fake_pool::COINBASE_TAG);
    assert_eq!(coinbase.value_sum, fake_pool::COINBASE_VALUE);
    assert_eq!(coinbase.output_count, fake_pool::COINBASE_OUTPUT_COUNT);
}

#[async_std::test]
async fn the_client_configures_subscribes_and_authorizes() {
    let server = FakePool::start(Behaviour::Stratum(Configure::Answer)).await;
    // Hold on to the jobs: a client nobody listens to gives up.
    let _jobs = run_client(pool("Fake Pool", &server.address, Some(15)));

    let configure = next_request(&server).await;
    assert_eq!(configure.method, "mining.configure");

    let subscribe = next_request(&server).await;
    assert_eq!(subscribe.method, "mining.subscribe");
    assert_eq!(
        subscribe.params,
        serde_json::json!(["stratum-observer"]),
        "the client should name itself when subscribing"
    );

    let authorize = next_request(&server).await;
    assert_eq!(authorize.method, "mining.authorize");
    assert_eq!(
        authorize.params,
        serde_json::json!([POOL_USER, POOL_PASSWORD]),
        "the client should authorize with the user and password of the pool"
    );

    assert!(
        configure.id < subscribe.id && subscribe.id < authorize.id,
        "every request should have its own, growing id, got {}, {} and {}",
        configure.id,
        subscribe.id,
        authorize.id
    );
}

/// `mining.configure` is optional, so a pool may not answer it at all. The
/// client has to carry on regardless.
#[async_std::test]
async fn a_pool_that_ignores_the_configure_still_gets_a_job_through() {
    let server = FakePool::start(Behaviour::Stratum(Configure::Ignore)).await;
    let jobs = run_client(pool("Quiet Pool", &server.address, Some(15)));

    let job = next_job(&jobs).await;
    assert_eq!(job.job.job_id, fake_pool::JOB_ID);
    assert_eq!(encode_hex(job.extranonce1.as_ref()), fake_pool::EXTRANONCE1);
}

#[async_std::test]
async fn the_client_gives_up_when_the_pool_closes_the_connection() {
    let server = FakePool::start(Behaviour::CloseImmediately).await;
    // No max lifetime, so the closed connection is the only thing that can
    // end this run. Waiting for the lifetime would take minutes.
    let pool = pool("Rude Pool", &server.address, None);
    let (job_sender, _jobs) = unbounded();

    timeout(Duration::from_secs(5), Client::run(&pool, job_sender))
        .await
        .expect("the client should notice that the pool closed the connection");
}

#[async_std::test]
async fn the_client_hangs_up_once_the_connection_is_too_old() {
    let server = FakePool::start(Behaviour::Stratum(Configure::Answer)).await;
    let pool = pool("Short Lived", &server.address, Some(1));
    let (job_sender, _jobs) = unbounded();

    timeout(Duration::from_secs(5), Client::run(&pool, job_sender))
        .await
        .expect("the client should hang up once the connection reached its age");

    timeout(SOON, server.client_gone.recv())
        .await
        .expect("the pool should see the connection go away")
        .expect("the channel should still be open");
}

#[async_std::test]
async fn the_client_keeps_trying_a_pool_that_refuses_the_connection() {
    let pool = pool("Closed Port", &fake_pool::closed_address().await, None);
    let (job_sender, _jobs) = unbounded();

    assert!(
        timeout(Duration::from_secs(1), Client::run(&pool, job_sender))
            .await
            .is_err(),
        "the client should keep trying to reach the pool instead of returning"
    );
}

/// An endpoint we can not even work out an address for must not take the pool
/// down: the caller of `run` keeps calling it, so returning lets it try again.
#[async_std::test]
async fn the_client_returns_on_an_endpoint_it_can_not_resolve() {
    let pool = pool("Broken Endpoint", "not-an-endpoint", None);
    let (job_sender, _jobs) = unbounded();

    timeout(SOON, Client::run(&pool, job_sender))
        .await
        .expect("the client should return so that the caller can try again");
}

/// A pool that hangs up on a connection it has been serving. Nothing more is
/// sent on it afterwards, so only the closed connection itself can tell the
/// client to stop.
#[async_std::test]
async fn the_client_gives_up_when_the_pool_hangs_up_on_a_working_connection() {
    let server = FakePool::start(Behaviour::CloseAfterHandshake).await;
    // No max lifetime and no job timeout to fall back on: waiting for either
    // would take minutes.
    let pool = pool("Fickle Pool", &server.address, None);
    let (job_sender, jobs) = unbounded();

    let run = task::spawn(async move { Client::run(&pool, job_sender).await });
    // The job the pool sent before hanging up still gets through.
    let job = next_job(&jobs).await;
    assert_eq!(job.job.job_id, fake_pool::JOB_ID);

    timeout(Duration::from_secs(5), run)
        .await
        .expect("the client should notice that the pool hung up");
}

/// The jobs are the whole point of a connection, so a client whose jobs go
/// nowhere stops and lets the caller decide what to do.
#[async_std::test]
async fn the_client_stops_when_nobody_listens_for_its_jobs() {
    let server = FakePool::start(Behaviour::Stratum(Configure::Answer)).await;
    // No max lifetime: only the jobs going nowhere can end this run.
    let pool = pool("Fake Pool", &server.address, None);
    let (job_sender, jobs) = unbounded::<JobUpdate<'static>>();
    drop(jobs);

    timeout(SOON, Client::run(&pool, job_sender))
        .await
        .expect("the client should stop once there is nobody to send jobs to");
}

/// A pool may hand out a new extranonce in the middle of a session. From then
/// on it builds its coinbases with that one.
#[async_std::test]
async fn a_new_extranonce_is_used_for_the_jobs_that_follow() {
    let server = FakePool::start(Behaviour::Stratum(Configure::Answer)).await;
    let jobs = run_client(pool("Fake Pool", &server.address, Some(15)));

    let first = next_job(&jobs).await;
    assert_eq!(
        encode_hex(first.extranonce1.as_ref()),
        fake_pool::EXTRANONCE1
    );

    for line in [
        fake_pool::set_extranonce("aabbccddee", 2),
        fake_pool::notify(),
    ] {
        server
            .to_client
            .send(format!("{}\n", line))
            .await
            .expect("the pool should be able to send to the client");
    }

    let second = next_job(&jobs).await;
    assert_eq!(encode_hex(second.extranonce1.as_ref()), "aabbccddee");
    assert_eq!(second.extranonce2_size, 2);
}
