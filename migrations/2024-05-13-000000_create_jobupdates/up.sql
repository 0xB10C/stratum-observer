CREATE TABLE IF NOT EXISTS job_updates (
    id                     SERIAL    PRIMARY KEY,
    timestamp              TIMESTAMP NOT NULL,
    pool                   TEXT      NOT NULL,
    merkle_branches        BYTEA[]   NOT NULL,
    header_version         BIGINT    NOT NULL,
    header_bits            BIGINT    NOT NULL,
    header_time            BIGINT    NOT NULL,
    header_prev_hash       TEXT      NOT NULL,
    extranonce1            BYTEA     NOT NULL,
    extranonce2_size       INTEGER   NOT NULL,
    clean_jobs             BOOL      NOT NULL,
    coinbase_raw           BYTEA     NOT NULL,
    coinbase_value         BIGINT    NOT NULL,
    coinbase_output_count  INTEGER   NOT NULL,
    coinbase_tag           TEXT      NOT NULL,
    coinbase_height        BIGINT    NOT NULL
)
