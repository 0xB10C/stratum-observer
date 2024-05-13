// @generated automatically by Diesel CLI.

diesel::table! {
    job_updates (id) {
        id -> Nullable<Integer>,
        timestamp -> Timestamp,
        pool_name -> Text,
        coinbase_tag -> Text,
        prev_hash -> Text,
        merkle_branches -> Text,
        height -> BigInt,
        output_sum -> BigInt,
        header_version -> BigInt,
        header_bits -> BigInt,
        header_time -> BigInt,
        extranonce1 -> Binary,
        extranonce2_size -> Integer,
        clean_jobs -> Bool,
    }
}
