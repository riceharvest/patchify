# Architecture

`patchify` is a local Rust binary that collapses the agent edit loop (read → patch → patch → write → shell-verify) into one atomic call. Edition 2024, Rust 1.85+, minimal deps (serde, clap, tokio for the self-update path).

## Boundaries

1. **CLI (`src/main.rs`)** — clap argument parsing, JSON stdin handling, exit-code mapping, completions (`clap_complete`), man page (`clap_mangen`), and the `--tool-manifest` agent-facing schema.
2. **Executor (`src/lib.rs`)** — validation, pre-flight matching, ordered apply, rollback, verify execution, dry-run. Pure `std::fs`; no async.
3. **Self-update (`src/update.rs`)** — GitHub Releases download with SHA-256 verification and atomic binary replace. The only networked path, and only via `--update`.

## Execution flow

```text
stdin JSON (BatchRequest)
        |
        v
  validate: sizes, counts, empty old_string, path safety (all or nothing)
        |
        v
  dry_run?  -> diff previews, exit without writing
        |
        v
  pre-flight: read every edit target, count matches, build new contents
        |                     \
        |                      any failure -> RolledBack (nothing written)
        v
  apply phase: write prepared contents in order
        |            each write preceded by TOCTOU re-read check
        v
  writes: create dirs, write files (deduped, last wins)
        |
        v
  verify: run shell commands, capture exit/stdout/stderr
        |
        v
  BatchResult JSON (per-edit status + diff_preview)
```

## Invariants

- **Atomicity**: a batch either fully applies or leaves every touched file byte-identical to before. Pre-flight failures write nothing; apply-phase failures roll back in reverse order (newly created files are removed, overwritten files restored).
- **Bounded work**: hard caps on edit/write count, file size, and payload size prevent a single call from OOMing the agent or the tool.
- **Path confinement**: `..`, absolute paths, and symlink escapes are refused unless explicitly allowed. Refusal is a validation error with a structured message, never a partial write.
- **No re-read needed**: the result carries per-edit `ok/applied/error/diff_preview/replacements` and full verify output, so the agent can decide in one turn.

## Testing

Unit tests in `src/lib.rs` cover matching, replace_all, rollback, create-dirs, duplicate writes, path traversal, symlink escape, size/count caps, dry-run, and verify. `tests/cli.rs` drives the real release binary: help/version, empty-input exit code, a 3-patch batch where the third fails and the first two must leave no trace, a full success batch with verify commands (including a failing one), dry-run, path refusal, manifest/completions/man rendering. No network in tests.
