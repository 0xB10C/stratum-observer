//! Tests for the task that writes the jobs into the database.
//!
//! The tests only run when TEST_DATABASE_URL points to a PostgreSQL database,
//! for example through `scripts/with-test-db.sh cargo test`. They share one
//! database and one table, so every test uses a pool name of its own and only
//! looks at the rows with that name.

mod common;

use async_std::task;
use common::*;
use diesel::prelude::*;
use diesel_migrations::MigrationHarness;
use std::time::Duration;
use stratum_observer::database::{db_writer_task, MIGRATIONS};
use stratum_observer::schema::job_updates;
use stratum_observer::types::{JobUpdate, NewJobUpdate, StoredJobUpdate};

/// How long a test waits for the writer task to get its work done.
const WAIT: Duration = Duration::from_secs(10);
/// A short wait, so the tests do not sit around for the three seconds the tool
/// waits in production.
const RETRY_DELAY: Duration = Duration::from_millis(20);

/// The database to test against, or None when there is none, in which case the
/// test says so and does nothing.
fn test_database_url() -> Option<String> {
    match std::env::var("TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            println!("skipped: TEST_DATABASE_URL not set");
            None
        }
    }
}

/// Opens a connection to the test database. The first caller creates the table
/// the tests share.
fn connect(db_url: &str) -> PgConnection {
    static MIGRATED: std::sync::Once = std::sync::Once::new();
    let mut conn = PgConnection::establish(db_url).expect("the test database should be reachable");
    MIGRATED.call_once(|| {
        conn.run_pending_migrations(MIGRATIONS)
            .expect("the migrations should run");
    });
    conn
}

/// The rows of one pool, oldest first.
fn rows_of_pool(conn: &mut PgConnection, pool_name: &str) -> Vec<StoredJobUpdate> {
    job_updates::table
        .filter(job_updates::pool.eq(pool_name))
        .order(job_updates::id.asc())
        .select(StoredJobUpdate::as_select())
        .load(conn)
        .expect("reading the rows back should work")
}

/// Waits until the pool has at least the given number of rows.
async fn wait_for_rows(
    conn: &mut PgConnection,
    pool_name: &str,
    count: usize,
) -> Vec<StoredJobUpdate> {
    let start = std::time::Instant::now();
    loop {
        let rows = rows_of_pool(conn, pool_name);
        if rows.len() >= count {
            return rows;
        }
        assert!(
            start.elapsed() < WAIT,
            "only {} of {} rows of '{}' showed up in the database",
            rows.len(),
            count,
            pool_name
        );
        task::sleep(Duration::from_millis(20)).await;
    }
}

/// Checks that the row holds what the job says.
fn assert_row_matches_job(row: &StoredJobUpdate, job: &JobUpdate<'_>) {
    let expected = NewJobUpdate::from(job.clone());
    assert_eq!(
        row.timestamp.and_utc().timestamp(),
        job.timestamp.timestamp(),
        "timestamp"
    );
    assert_eq!(row.pool, expected.pool);
    assert_eq!(
        row.merkle_branches,
        expected
            .merkle_branches
            .iter()
            .map(|branch| Some(branch.clone()))
            .collect::<Vec<Option<Vec<u8>>>>(),
        "merkle_branches"
    );
    assert_eq!(row.header_version, expected.header_version);
    assert_eq!(row.header_bits, expected.header_bits);
    assert_eq!(row.header_time, expected.header_time);
    assert_eq!(row.header_prev_hash, expected.header_prev_hash);
    assert_eq!(row.extranonce1, expected.extranonce1);
    assert_eq!(row.extranonce2_size, expected.extranonce2_size);
    assert_eq!(row.clean_jobs, expected.clean_jobs);
    assert_eq!(row.coinbase_raw, expected.coinbase_raw);
    assert_eq!(row.coinbase_value, expected.coinbase_value);
    assert_eq!(row.coinbase_output_count, expected.coinbase_output_count);
    assert_eq!(row.coinbase_tag, expected.coinbase_tag);
    assert_eq!(row.coinbase_height, expected.coinbase_height);
}

#[async_std::test]
async fn test_jobs_are_written_to_the_database() {
    let db_url = match test_database_url() {
        Some(url) => url,
        None => return,
    };
    let mut conn = connect(&db_url);
    let pool = unique_name("pool");

    let first_job = real_block_job(&pool);
    let mut second_job = real_block_job(&pool);
    second_job.timestamp = second_job.timestamp + chrono::TimeDelta::seconds(30);

    let (sender, receiver) = async_channel::unbounded();
    sender.send(first_job.clone()).await.unwrap();
    sender.send(second_job.clone()).await.unwrap();
    let url = db_url.clone();
    task::spawn(async move { db_writer_task(receiver, &url, RETRY_DELAY).await });

    let rows = wait_for_rows(&mut conn, &pool, 2).await;
    assert_eq!(rows.len(), 2, "exactly the two jobs should be stored");

    // Every column holds what the job says, the real values of block 856740.
    assert_row_matches_job(&rows[0], &first_job);
    assert_row_matches_job(&rows[1], &second_job);

    assert_eq!(rows[0].coinbase_height, BLOCK_HEIGHT as i64);
    assert_eq!(rows[0].coinbase_tag, COINBASE_TAG);
    assert_eq!(rows[0].coinbase_value, COINBASE_VALUE as i64);
    assert_eq!(rows[0].coinbase_output_count, COINBASE_OUTPUT_COUNT);
    assert_eq!(
        stratum_observer::utils::encode_hex(&rows[0].coinbase_raw),
        COINBASE_RAW
    );
    assert_eq!(rows[0].header_prev_hash, PREV_HASH_DISPLAYED);
    assert_eq!(rows[0].header_version, HEADER_VERSION as i64);
    assert_eq!(rows[0].header_bits, HEADER_BITS as i64);
    assert_eq!(rows[0].header_time, HEADER_TIME as i64);
    assert_eq!(
        rows[0].extranonce1,
        stratum_observer::utils::decode_hex(EXTRANONCE1).unwrap()
    );
    assert_eq!(rows[0].extranonce2_size, EXTRANONCE2_SIZE as i32);
    assert!(rows[0].clean_jobs);
    assert_eq!(rows[0].merkle_branches.len(), 2);
    assert_eq!(
        rows[1].timestamp.and_utc().timestamp(),
        rows[0].timestamp.and_utc().timestamp() + 30
    );
}

/// Makes the database refuse the rows of one pool, so a test can see what the
/// writer task does with a job it cannot insert. The rows of the other tests
/// are not touched.
struct RefusingTrigger {
    name: String,
}

impl RefusingTrigger {
    fn install(conn: &mut PgConnection, pool_name: &str) -> RefusingTrigger {
        let name = unique_name("refuse").replace('-', "_");
        diesel::sql_query(format!(
            "CREATE FUNCTION {name}() RETURNS trigger AS $$ BEGIN \
             IF NEW.pool = '{pool_name}' THEN RAISE EXCEPTION 'test: this row is refused'; \
             END IF; RETURN NEW; END; $$ LANGUAGE plpgsql"
        ))
        .execute(conn)
        .expect("creating the trigger function should work");
        diesel::sql_query(format!(
            "CREATE TRIGGER {name} BEFORE INSERT ON job_updates \
             FOR EACH ROW EXECUTE FUNCTION {name}()"
        ))
        .execute(conn)
        .expect("creating the trigger should work");
        RefusingTrigger { name }
    }

    fn remove(self, conn: &mut PgConnection) {
        let _ =
            diesel::sql_query(format!("DROP TRIGGER {} ON job_updates", self.name)).execute(conn);
        let _ = diesel::sql_query(format!("DROP FUNCTION {}()", self.name)).execute(conn);
    }
}

#[async_std::test]
async fn test_a_refused_job_does_not_drop_the_queued_jobs() {
    let db_url = match test_database_url() {
        Some(url) => url,
        None => return,
    };
    let mut conn = connect(&db_url);
    let pool = unique_name("pool");
    let refused_pool = format!("{}-refused", pool);
    let trigger = RefusingTrigger::install(&mut conn, &refused_pool);

    // All three jobs are queued before the task starts, so the good job that
    // comes last is waiting in the channel while the refused one fails.
    let (sender, receiver) = async_channel::unbounded();
    sender.send(real_block_job(&pool)).await.unwrap();
    sender.send(real_block_job(&refused_pool)).await.unwrap();
    sender.send(real_block_job(&pool)).await.unwrap();
    let url = db_url.clone();
    task::spawn(async move { db_writer_task(receiver, &url, RETRY_DELAY).await });

    let rows = wait_for_rows(&mut conn, &pool, 2).await;
    assert_eq!(
        rows.len(),
        2,
        "only the refused job should be lost, not the ones queued behind it"
    );
    assert!(
        rows_of_pool(&mut conn, &refused_pool).is_empty(),
        "the refused job should not be in the database"
    );

    trigger.remove(&mut conn);
}
