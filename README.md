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

```sh
cat <<'EOF' | patchify
{
  "writes": [{"path": "src/new.rs", "content": "pub const V: u32 = 1;\n"}],
  "edits": [
    {"path": "src/main.rs", "old_string": "println!(\"hi\");", "new_string": "println!(\"bye\");"},
    {"path": "src/lib.rs", "old_string": "a + b", "new_string": "a.wrapping_add(b)", "replace_all": true}
  ],
  "verify": [{"cmd": "cargo check"}, {"cmd": "git diff --stat"}],
  "dry_run": false,
  "allow_outside": false
}
EOF
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
| `--input FILE` | Read request JSON from FILE instead of stdin |
| `--dry-run` | Force dry-run regardless of JSON |
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
- Exact string matching, no regex, no fuzzy matching.
- Path traversal (`../`), absolute paths, and symlink escapes out of the working
  directory are refused unless `allow_outside` is set.
- File size cap 5 MiB (skip/rollback), payload cap 2 MiB per string, max 50 edits
  and 50 writes per call, empty `old_string` rejected.
- TOCTOU guard: file is re-read and compared immediately before each write.

## Testing

```sh
cargo test          # 15 unit + 7 CLI integration tests, no network
cargo check --all-targets
```

## Docs

- `docs/spec.md` — normative behavior
- `docs/architecture.md` — design boundaries
- `docs/install.md` — install details per platform
- `hermes-tool.json` — machine-readable tool manifest (`patchify --tool-manifest`)

## License

MIT OR Apache-2.0
