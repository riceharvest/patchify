# Changelog

## v0.2.0

### Features
- `--diff` flag: print unified diff preview without applying (implies dry-run)
- `--format text|json` flag: human-readable text output or machine JSON (default json)
- flock-based file locking (`.patchify-lock` sidecar): concurrent patchify instances serialize; check-write window protected against lost updates
- `--input FILE` flag: read request JSON from file (or `-` for stdin)
- `--tool-manifest`: print hermes-tool.json agent-facing tool schema
- `--completions SHELL`: shell completions for bash/zsh/fish/powershell/elvish
- `--man`: print roff man page
- `--update`: self-update from GitHub Releases (SHA-256 verified)

### Fixes
- Chained same-path edits now compose correctly (each edit matches previous edit's result)
- `--input FILE` now actually loads the payload (was discarded)
- `result_to_text` status rendering simplified (borrow &str)

### Agentic QOL log (friction → fix)

| Friction | Fix |
| --- | --- |
| Agent had to re-read file after patch to confirm result | Per-edit `ok/applied/error/diff_preview/replacements` in response |
| Agent needed separate tool call to verify after patch | Built-in `verify[]` field runs shell cmds in same call |
| Agent unsure if edit will match before committing | `dry_run` / `--diff` / `--dry-run` flag returns diff previews risk-free |
| Agent unsure if file exists or is editable | Pre-flight catches missing/oversized/non-UTF8 targets before any write |
| Agent confused by silent partial failure | Per-edit `error` field names exact failure reason |
| Agent unsure if concurrent writes collide | flock locking serializes instances; `writes[]` dedupes last-wins with status |
| Agent unsure if patchify is up to date | `--update` self-updates from GitHub Releases with SHA-256 check |
| Agent unsure which shell completions available | `--completions SHELL` for bash/zsh/fish/powershell/elvish |
| Agent needs to check tool capabilities without reading docs | `--tool-manifest` prints machine-readable schema |

### Tests
- 21 unit + 11 CLI integration = 32 tests, no network required
- clippy clean, fmt clean, cargo check --all-targets clean
