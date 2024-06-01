// @generated automatically by Diesel CLI.

diesel::table! {
    job_updates (id) {
        id -> Int4,
        timestamp -> Timestamp,
        pool -> Text,
        merkle_branches -> Array<Nullable<Bytea>>,
        header_version -> Int8,
        header_bits -> Int8,
        header_time -> Int8,
        header_prev_hash -> Text,
        extranonce1 -> Bytea,
        extranonce2_size -> Int4,
        clean_jobs -> Bool,
        coinbase_raw -> Bytea,
        coinbase_value -> Int8,
        coinbase_output_count -> Int4,
        coinbase_tag -> Text,
        coinbase_height -> Int8,
    }
}
