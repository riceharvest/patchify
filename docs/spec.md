# Specification

This document is normative for the `patchify` command. Behavior not specified here is not promised by the current release.

## Command contract

`patchify` reads a `BatchRequest` JSON document from stdin (or `--input FILE`; `--input -` reads stdin explicitly) and writes a single `BatchResult` JSON document to stdout. `--help` and `--version` succeed without input. Empty or unparseable input exits with code 2.

## Request contract

- `edits[]`: `{path, old_string, new_string, replace_all?}`. Applied in order. `old_string` must occur exactly once in the target file unless `replace_all: true`; zero or multiple matches fail the batch. `old_string` must be non-empty. Exact byte matching, no regex. Multiple edits to the same path compose: each edit matches against the previous edit's result (the batch writes the composed content once).
- `writes[]`: `{path, content, create_dirs?}`. Create or overwrite files; parent directories created when `create_dirs` (default true). Duplicate paths: last write wins.
- `verify[]`: `{cmd}`. Shell commands run after all edits and writes land, via `sh -c` (Windows: `cmd /C`) with the working directory as cwd. stdout/stderr capped at 4000 chars in output.
- `dry_run: true`: validate and return diff previews without writing anything.
- `allow_outside: true`: unsafe opt-in permitting absolute paths and targets outside the working directory.

## Defaults

| Option | Default |
| --- | --- |
| max edits per call | 50 |
| max writes per call | 50 |
| max target file size | 5 MiB |
| max string payload | 2 MiB |
| verify stdout/stderr cap | 4000 chars each |
| `dry_run` | false |
| `allow_outside` | false |

## Ordering

Edits apply before writes. All edits execute first (same-path edits composing in order), then all writes, then verify commands. A write to a path that was also edited overwrites the edit; the result reports both operations as applied.

## Atomicity and rollback

The batch is all-or-nothing. All edits are matched against pre-read file contents before anything is written (pre-flight). Any failure — match count violation, missing target, oversized file, refused path — aborts before writing where possible and otherwise rolls back every prior applied edit/write in reverse order, restoring original bytes. `status` reports `applied`, `rolled_back`, or `dry_run`.

## Path safety

Relative paths must not contain `..` components. Absolute paths are refused unless `allow_outside` is set. Symlinked targets are resolved (up to 8 hops) and refused if they escape the working directory unless `allow_outside` is set. A TOCTOU guard re-reads each file immediately before writing and aborts on drift. The check-write sequence is protected by an exclusive advisory `flock` on a `<file>.patchify-lock` sidecar; concurrent patchify instances serialize, and the lock sidecar is removed on release.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | All edits and writes applied |
| 1 | Batch rolled back, invalid request, or dry-run found failures |
| 2 | Usage/IO error (empty stdin, unreadable input) |

## Compatibility

The supported host targets are Linux, macOS, and Windows. The CLI flag names, request/response schemas, and exit codes in this document are public contract; changes require updating tests and this specification together.
