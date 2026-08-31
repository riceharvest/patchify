# patchify

Batch edits + writes + verify commands in one atomic tool call. Pure Rust, edition 2024.

One `patchify` call replaces the `read_file -> patch -> patch -> write_file -> terminal`
chain agents pay for on nearly every edit turn (measured across ~283k hermes tool calls:
`patch -> patch` 49.6%, `write -> terminal` 54%, `patch -> terminal` 32%).

## Install

```sh
# one-line (Linux/macOS)
curl -fsSL https://raw.githubusercontent.com/riceharvest/patchify/main/install.sh | sh

# one-line (Windows PowerShell)
irm https://raw.githubusercontent.com/riceharvest/patchify/main/install.ps1 | iex

# or from source
cargo install patchify

# self-update an existing install
patchify --update
```

## Usage

Pipe a `BatchRequest` JSON to stdin; results come back as one JSON line on stdout.
Note the ordering: **edits run before writes**, so you can't edit a file you
create in the same batch — create it first, patch it in the next call.

Create a file and verify it in one call (`/tmp/patchify-demo`, real output):

```sh
mkdir -p /tmp/patchify-demo/src && cd /tmp/patchify-demo
echo '{"writes":[{"path":"src/greeting.rs","content":"pub fn greet() -> &'"'"'static str {\n    \"hello\"\n}\n"}],"verify":[{"cmd":"cat src/greeting.rs"}]}' | patchify
```

```json
{"ok":true,"status":"applied","edits":[],"writes":[{"path":"src/greeting.rs","ok":true,"applied":true,"created_dirs":true}],"verify":[{"cmd":"cat src/greeting.rs","exit":0,"stdout":"pub fn greet() -> &'static str {\n    \"hello\"\n}\n","stderr":"","duration_ms":2}]}
```

Then patch it — two chained edits on the same path plus a verify command:

```sh
echo '{"edits":[{"path":"src/greeting.rs","old_string":"\"hello\"","new_string":"\"hello, patchify\""},{"path":"src/greeting.rs","old_string":"hello, patchify","new_string":"HELLO, PATCHIFY"}],"verify":[{"cmd":"cat src/greeting.rs"}]}' | patchify
```

```json
{"ok":true,"status":"applied","edits":[{"path":"src/greeting.rs","ok":true,"applied":true,"replacements":1},{"path":"src/greeting.rs","ok":true,"applied":true,"diff_preview":"  pub fn greet() -> &'static str {\n-     \"hello, patchify\"\n+     \"HELLO, PATCHIFY\"\n","replacements":1}],"verify":[{"cmd":"cat src/greeting.rs","exit":0,"stdout":"pub fn greet() -> &'static str {\n    \"HELLO, PATCHIFY\"\n}\n","stderr":"","duration_ms":1}]}
```

## Request schema

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `edits` | array | `[]` | Exact-string edits, applied in order. Max 50. |
| `edits[].path` | string | required | Relative path; absolute and `../` refused. |
| `edits[].old_string` | string | required | Must match exactly once unless `replace_all`. Non-empty. Max 2 MiB. |
| `edits[].new_string` | string | required | Replacement text. |
| `edits[].replace_all` | bool | `false` | Replace every occurrence instead of exactly-one. |
| `writes` | array | `[]` | Create/overwrite files. Max 50. Duplicate paths: last wins. |
| `writes[].path` | string | required | Parent dirs created (`mkdir -p`) when `create_dirs`. |
| `writes[].content` | string | required | Full file content. Max 2 MiB. |
| `writes[].create_dirs` | bool | `true` | Create missing parent directories. |
| `verify` | array | `[]` | Shell commands run after edits land. |
| `verify[].cmd` | string | required | Run via `sh -c` (Windows: `cmd /C`), cwd-relative. |
| `dry_run` | bool | `false` | Return diff previews only; write nothing. |
| `allow_outside` | bool | `false` | Unsafe opt-in: allow paths outside the working directory. |

## Response schema

```json
{
  "ok": true,
  "status": "applied",
  "edits": [{"path": "src/main.rs", "ok": true, "applied": true, "error": null,
             "diff_preview": "  fn main() {\n- old\n+ new\n", "replacements": 1}],
  "writes": [{"path": "src/new.rs", "ok": true, "applied": true, "error": null, "created_dirs": true}],
  "verify": [{"cmd": "cargo check", "exit": 0, "stdout": "...", "stderr": "...", "duration_ms": 812}]
}
```

`status` is `applied`, `rolled_back`, or `dry_run`. On any edit failure the whole
batch is undone (prior edits restored byte-for-byte, freshly written files removed)
and every per-edit status explains what happened — no re-reading needed.

## CLI flags

| Flag | Meaning |
| --- | --- |
| `--input FILE` | Read request JSON from FILE (`-` = stdin) |
| `--dry-run` | Force dry-run regardless of JSON |
| `--diff` | Print unified diff instead of applying (implies dry-run) |
| `--format text\|json` | Human text or machine JSON output (default: json) |
| `--allow-outside` | Allow paths outside cwd (unsafe opt-in) |
| `--cwd DIR` | Base directory for relative paths |
| `--completions SHELL` | Emit shell completions (bash/zsh/fish/powershell/elvish) |
| `--man` | Print the roff man page |
| `--tool-manifest` | Print `hermes-tool.json` (agent-facing tool schema) |
| `--update` | Self-update from GitHub Releases (SHA-256 verified) |
| `-h`, `-V` | Help / version |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | All edits and writes applied |
| `1` | Batch rolled back, failed validation, or dry-run found failures |
| `2` | Usage/IO error (empty stdin, unreadable input) |

## Safety model

- Edits apply in order with **atomic rollback**: one mismatch (0 or >1 matches)
  undoes everything already applied in the batch.
- Chained same-path edits compose: each edit matches the previous edit's result.
- Exact string matching, no regex, no fuzzy matching.
- Path traversal (`../`), absolute paths, and symlink escapes out of the working
  directory are refused unless `allow_outside` is set.
- File size cap 5 MiB (skip/rollback), payload cap 2 MiB per string, max 50 edits
  and 50 writes per call, empty `old_string` rejected.
- TOCTOU guard: file is re-read and compared immediately before each write, and
  the check-write window is protected by an advisory `flock` on a
  `<file>.patchify-lock` sidecar (removed on release). Concurrent patchify
  instances serialize; a racing external writer that lands inside the window is
  detected and the batch rolls back rather than clobbering it.
- Adversarial audit (all verified by live probes): `../../etc/passwd` traversal
  refused; symlinked file AND directory escapes refused (secret untouched);
  10 MiB `old_string` rejected in <50 ms (2 MiB cap, no OOM); NUL bytes in paths
  rejected; 20-round concurrent-write race produced zero torn states and zero
  stale lock files.

## Testing

```sh
cargo test          # 21 unit + 11 CLI integration tests, no network
cargo check --all-targets
cargo clippy --all-targets   # 0 warnings
```

## Pre-commit hook

`examples/pre-commit` ships a ready hook that probes the executor + guards
before every commit:

```sh
cp examples/pre-commit .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit
```

## Docs

- `docs/spec.md` — normative behavior
- `docs/architecture.md` — design boundaries
- `docs/install.md` — install details per platform
- `hermes-tool.json` — machine-readable tool manifest (`patchify --tool-manifest`)

## License

MIT OR Apache-2.0
