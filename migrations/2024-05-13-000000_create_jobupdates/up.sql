CREATE TABLE IF NOT EXISTS job_updates (
    id                  INTEGER   PRIMARY KEY,
    timestamp           TIMESTAMP NOT NULL,
    pool_name           TEXT      NOT NULL,
    coinbase_tag        TEXT      NOT NULL,
    prev_hash           TEXT      NOT NULL,
    merkle_branches     TEXT      NOT NULL,
    height              BIGINT    NOT NULL,
    output_sum          BIGINT    NOT NULL,
    header_version      BIGINT    NOT NULL,
    header_bits         BIGINT    NOT NULL,
    header_time         BIGINT    NOT NULL,
    extranonce1         BLOB      NOT NULL,
    extranonce2_size    INTEGER   NOT NULL,
    clean_jobs          BOOL      NOT NULL
)
