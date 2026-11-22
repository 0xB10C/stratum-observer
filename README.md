# stratum-observer

A work-in-progress tool to visualize and record stratum mining jobs.

## Development

Build and test the project with cargo:

```
cargo build
cargo test
```

The build needs a C linker and the PostgreSQL client library (libpq). If you
use Nix, `nix-shell` gives you a shell with everything needed, for example
`nix-shell --run 'cargo test'`.

Tests that talk to a database only run when the environment variable
`TEST_DATABASE_URL` points to a PostgreSQL database. The script
`scripts/with-test-db.sh` starts a temporary database, sets the variable and
removes the database again afterwards:

```
scripts/with-test-db.sh cargo test
```
