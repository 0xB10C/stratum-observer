// @generated automatically by Diesel CLI.

diesel::table! {
    job_updates (id) {
        id -> Int4,
        timestamp -> Timestamp,
        pool_name -> Text,
        coinbase_tag -> Text,
        prev_hash -> Text,
        merkle_branches -> Text,
        height -> Int8,
        output_sum -> Int8,
        header_version -> Int8,
        header_bits -> Int8,
        header_time -> Int8,
        extranonce1 -> Bytea,
        extranonce2_size -> Int4,
        clean_jobs -> Bool,
    }
}
