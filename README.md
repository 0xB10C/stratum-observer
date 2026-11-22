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

### Mutation testing

[cargo-mutants](https://mutants.rs) changes the code in small ways and checks
that the tests notice, which shows where the tests are not looking:

```
cargo mutants
scripts/with-test-db.sh cargo mutants   # also runs the database tests
```

The results land in `mutants.out/`, `missed.txt` being the interesting one:
every change listed there went unnoticed by the tests. The settings, for
example the files that are left out, are in `.cargo/mutants.toml`.
