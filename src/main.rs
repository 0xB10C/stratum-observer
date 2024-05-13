use crate::schema::job_updates;
use crate::types::JobUpdate;
use crate::types::NewJobUpdate;
use crate::utils::{bip34_coinbase_block_height, extract_coinbase_string};
use async_channel::{unbounded, Receiver};
use async_std::task;
use client::{initialize_client, Client};
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use env_logger::Env;
use std::collections::BTreeMap;

mod client;
mod config;
mod schema;
mod types;
mod utils;

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations/");

fn main() {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let config = match config::load_config() {
        Ok(c) => c,
        Err(e) => panic!("could not load config: {}", e),
    };

    let (job_sender, job_receiver) = unbounded();

    for pool in config.pools.clone() {
        let js = job_sender.clone();
        task::spawn(async move {
            // reopen clients if a client looses or closes the connection
            loop {
                let client = Client::new(&pool, js.clone()).await;
                initialize_client(client).await;
            }
        });
    }

    let (visualization_sender, visulization_receiver) = unbounded();
    task::spawn(async move {
        terminal_visualization_task(visulization_receiver).await;
    });

    let (sqlite_sender, sqlite_receiver) = unbounded();
    task::spawn(async move {
        sqlite_writer_task(sqlite_receiver).await;
    });

    task::block_on(async {
        loop {
            let job = job_receiver.recv().await.unwrap();
            visualization_sender.send(job.clone()).await.unwrap();
            sqlite_sender.send(job).await.unwrap();
        }
    });
}

async fn sqlite_writer_task(receiver: Receiver<JobUpdate<'_>>) {
    let mut conn = SqliteConnection::establish("dev-stratum-observer.sqlite").unwrap();
    conn.run_pending_migrations(MIGRATIONS).unwrap();
    loop {
        let update = receiver.recv().await.unwrap();
        let v: NewJobUpdate = update.into();
        diesel::insert_into(job_updates::table)
            .values(&vec![v])
            .execute(&mut conn)
            .expect("Error saving new job update");
    }
}

async fn terminal_visualization_task(receiver: Receiver<JobUpdate<'_>>) {
    let mut last_job: BTreeMap<String, JobUpdate> = BTreeMap::new();
    loop {
        let job = receiver.recv().await.unwrap();
        last_job.insert(job.pool.clone().name, job);

        let mut lines: Vec<(u64, String)> = vec![];
        for (_, j) in last_job.iter() {
            let template_colors: String = j
                .job
                .merkle_branch
                .iter()
                .map(|branch| colored(branch.as_ref()[1] % 255, "  "))
                .collect::<Vec<String>>()
                .join("");
            let cb = j.clone().coinbase();
            let coinbase_script_sig = &cb
                .input
                .first()
                .expect("coinbase should have one input")
                .script_sig;
            let prevhash = j.prev_block_hash();
            let prevhash_colored = colored(prevhash[0], &prevhash.to_string());
            let output_value_f64: f64 = cb.output.iter().map(|o| o.value.to_btc()).sum::<f64>();
            let output_value_u64: u64 = cb.output.iter().map(|o| o.value.to_sat()).sum::<u64>();
            lines.push((
                output_value_u64,
                format!(
                    "{: <18} {:<50} {} {} {} {:>4}s {:>2}s {:>5} {}\t{}",
                    j.pool.name,
                    extract_coinbase_string(coinbase_script_sig),
                    prevhash_colored,
                    bip34_coinbase_block_height(&coinbase_script_sig).unwrap_or_default(),
                    colored(
                        (output_value_u64 % 128 as u64) as u8 + 42u8,
                        &format!("{:2.8} BTC", output_value_f64),
                    ),
                    j.time_connected_seconds(),
                    j.age(),
                    j.job.clean_jobs,
                    template_colors,
                    j.pool.name,
                ),
            ));

            lines.sort_by_key(|(output_value, _)| *output_value);
            for line in lines.iter().rev() {
                println!("{}", line.1);
            }
            println!("\n\n");
        }
    }
}

fn colored(color: u8, text: &str) -> String {
    return format!("\x1B[48;5;{}m\x1B[38;5;0m{}\x1B[0m", color, text);
}
