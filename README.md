# patchify
Batch edits: read_file + patch + write_file in one tool call (read->patch 10.7%, patch->patch 49.6%, write->terminal 54%). Pure Rust.

Why: Fixes read->patch verification loops and patch->patch bursts atomically with rollback.

## Bigram evidence (from hermes state.db ~283k tool calls)
See `cargo test` and `src/lib.rs` for batch API. Pure Rust, tokio.

## Usage
```bash
cargo build --release
echo '{"items":[{}]}' | ./target/release/patchify --input -
```
```bash
cargo test
```
