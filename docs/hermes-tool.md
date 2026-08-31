# patchify tool registration for hermes

The tool is the `patchify` binary (installed via `cargo install patchify` or
`install.sh`). This manifest describes it for agent tool registries; regenerate
with `patchify --tool-manifest` (kept in sync with the binary version).

## Hermes registration snippet

In the agent's tool config, register a custom tool that pipes JSON to stdin:

```yaml
tools:
  patchify:
    cmd: patchify
    stdin: json          # BatchRequest JSON on stdin
    stdout: json         # single-line BatchResult JSON
    exit_codes:
      0: all edits and writes applied
      1: rolled back or invalid request
      2: usage/IO error
```

Library usage (Rust consumers):

```rust
let req = patchify::BatchRequest::from_json(&payload)?;
let result = patchify::execute_batch(&req, &cwd);
println!("{}", patchify::result_to_json(&result));
```

## Response contract

- `status`: `applied` | `rolled_back` | `dry_run`
- `edits[]`: `{path, ok, applied, error?, diff_preview?, replacements?}` — per-edit
  diagnostics so the agent decides without re-reading files
- `writes[]`: `{path, ok, applied, error?, created_dirs?}`
- `verify[]`: `{cmd, exit, stdout, stderr, duration_ms}` — shell output included,
  capped at 4000 chars per stream
- Any edit failure rolls the whole batch back; `error` carries the first failure

## Limits (enforced, see src/lib.rs)

- max 50 edits / 50 writes per call
- 5 MiB per target file, 2 MiB per string payload
- empty `old_string` rejected; 0 or >1 matches rejected (unless `replace_all`)
- paths must stay inside the working directory unless `allow_outside`
