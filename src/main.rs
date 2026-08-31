use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use clap_complete::{Shell, generate};

use patchify::{BatchError, BatchRequest, BatchResult, execute_batch};

/// patchify - batch edits + writes + verify commands in one atomic tool call.
///
/// Pipe a BatchRequest JSON to stdin; get structured per-edit diagnostics back.
/// Edits apply in order with exact-string matching and atomic rollback.
#[derive(Parser)]
#[command(name = "patchify", version, about, arg_required_else_help = false)]
struct Cli {
    /// Read BatchRequest JSON from this file instead of stdin
    #[arg(long, value_name = "FILE")]
    input: Option<PathBuf>,
    /// Return diff previews only; touch nothing
    #[arg(long)]
    dry_run: bool,
    /// Print a unified diff of the batch instead of applying it (implies dry-run)
    #[arg(long)]
    diff: bool,
    /// Output format: json (machine) or text (human)
    #[arg(long, value_name = "FORMAT", default_value = "json")]
    format: String,
    /// Allow edits/writes outside the working directory (unsafe)
    #[arg(long)]
    allow_outside: bool,
    /// Working directory for relative paths (default: cwd)
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// Shell completions for the given shell
    #[arg(long, value_name = "SHELL")]
    completions: Option<Shell>,
    /// Print the roff man page
    #[arg(long)]
    man: bool,
    /// Print the hermes-tool.json tool manifest
    #[arg(long)]
    tool_manifest: bool,
    /// Self-update from GitHub Releases (checksum-verified)
    #[arg(long)]
    update: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if cli.completions.is_some() || cli.man || cli.update || cli.tool_manifest {
        return handle_special(&cli);
    }

    // Read JSON payload.
    let mut payload = String::new();
    if let Some(path) = &cli.input {
        if path.as_os_str() == "-" {
            if std::io::stdin().read_to_string(&mut payload).is_err() {
                eprintln!("patchify: failed to read stdin");
                return ExitCode::from(2);
            }
        } else {
            match std::fs::read_to_string(path) {
                Ok(content) => payload = content,
                Err(e) => {
                    eprintln!("patchify: cannot read {path:?}: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    } else if std::io::stdin().read_to_string(&mut payload).is_err() {
        eprintln!("patchify: failed to read stdin (pipe BatchRequest JSON, or use --input FILE)");
        return ExitCode::from(2);
    }
    if payload.trim().is_empty() {
        eprintln!("patchify: empty input — pipe BatchRequest JSON to stdin");
        return ExitCode::from(2);
    }

    let mut req = match BatchRequest::from_json(&payload) {
        Ok(r) => r,
        Err(e) => {
            let err: Result<BatchResult, BatchError> = Err(e);
            if cli.format == "text" {
                print!("{}", patchify::result_to_text(&err));
            } else {
                println!("{}", patchify::result_to_json(&err));
            }
            return ExitCode::from(2);
        }
    };
    // CLI flags override JSON fields.
    if cli.dry_run || cli.diff {
        req.dry_run = true;
    }
    if cli.allow_outside {
        req.allow_outside = true;
    }

    let cwd = match cli.cwd.as_deref() {
        Some(d) => d.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("patchify: cannot determine cwd: {e}");
                return ExitCode::from(2);
            }
        },
    };

    let result = execute_batch(&req, &cwd);
    if cli.diff {
        print_unified_diffs(&result);
    } else if cli.format == "text" {
        print!("{}", patchify::result_to_text(&result));
    } else {
        println!("{}", patchify::result_to_json(&result));
    }
    exit_code_for(&result)
}

/// Human-readable unified diff view (--diff): never applies, per-file hunks.
fn print_unified_diffs(res: &Result<BatchResult, BatchError>) {
    match res {
        Ok(r) => {
            for e in &r.edits {
                match (&e.diff_preview, &e.error) {
                    (Some(preview), _) => {
                        println!("--- a/{}", e.path);
                        println!("+++ b/{}", e.path);
                        print!("{preview}");
                        if !preview.ends_with('\n') {
                            println!();
                        }
                    }
                    (None, Some(err)) => println!("! {}: {err}", e.path),
                    (None, None) => println!("! {}: no preview", e.path),
                }
            }
            for w in r.writes.iter().flatten() {
                if w.applied {
                    println!("+ created/overwrote: {}", w.path);
                }
            }
        }
        Err(e) => {
            println!("! {e}");
        }
    }
}

fn exit_code_for(res: &Result<BatchResult, BatchError>) -> ExitCode {
    match res {
        Ok(r) if r.ok => ExitCode::SUCCESS, // 0: all applied
        Ok(_) => ExitCode::FAILURE,         // 1: partial/dry-run with failures
        Err(_) => ExitCode::FAILURE,        // 1: rolled back
    }
}

fn handle_special(cli: &Cli) -> ExitCode {
    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        generate(shell, &mut cmd, "patchify", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }
    if cli.man {
        let mut buf = Vec::new();
        clap_mangen::Man::new(Cli::command())
            .render(&mut buf)
            .expect("render man page");
        use std::io::Write;
        std::io::stdout().write_all(&buf).expect("write man page");
        return ExitCode::SUCCESS;
    }
    if cli.tool_manifest {
        println!("{}", tool_manifest_json());
        return ExitCode::SUCCESS;
    }
    if cli.update {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        return match rt.block_on(patchify::update::run_update()) {
            Ok(m) => {
                println!("{m}");
                ExitCode::SUCCESS
            }
            Err(patchify::update::UpdateError::UpToDate(m)) => {
                println!("{m}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("patchify update: {e}");
                ExitCode::FAILURE
            }
        };
    }
    ExitCode::SUCCESS
}

fn tool_manifest_json() -> String {
    let schema = serde_json::json!({
        "name": "patchify",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Batch edits + writes + verify commands in one atomic tool call. Edits apply in order with exact-string matching and rollback on any mismatch.",
        "input": {
            "type": "object",
            "properties": {
                "edits": {"type": "array", "maxItems": patchify::MAX_EDITS, "description": "exact-string edits applied in order; multiple edits to the same path compose (each matches the previous edit's result); first mismatch rolls back the whole batch", "items": {"type": "object", "properties": {
                    "path": {"type": "string", "description": "path relative to cwd; absolute and ../ refused unless allow_outside"},
                    "old_string": {"type": "string", "description": "exact literal text to find; must occur exactly once unless replace_all; non-empty; max 2MiB; NOT a regex"},
                    "new_string": {"type": "string", "description": "replacement literal; max 2MiB; empty string = deletion"},
                    "replace_all": {"type": "boolean", "default": false, "description": "replace every occurrence instead of requiring exactly one"}
                }, "required": ["path", "old_string", "new_string"]}},
                "writes": {"type": "array", "maxItems": patchify::MAX_WRITES, "description": "create or overwrite whole files; applied AFTER edits; duplicate paths: last wins", "items": {"type": "object", "properties": {
                    "path": {"type": "string", "description": "path relative to cwd"},
                    "content": {"type": "string", "description": "full file content; max 2MiB"},
                    "create_dirs": {"type": "boolean", "default": true, "description": "mkdir -p missing parent directories"}
                }, "required": ["path", "content"]}},
                "verify": {"type": "array", "description": "shell commands run after edits+writes land; use for cargo check / git diff --stat so one call covers patch+verify", "items": {"type": "object", "properties": {
                    "cmd": {"type": "string", "description": "run via sh -c in the working directory; verify failure does NOT roll back edits"}
                }, "required": ["cmd"]}},
                "dry_run": {"type": "boolean", "default": false, "description": "validate + return diff previews, write nothing"},
                "allow_outside": {"type": "boolean", "default": false, "description": "UNSAFE: permit absolute paths and symlink escapes outside the working directory"}
            }
        },
        "output": {"status": "applied|rolled_back|dry_run", "edits": [{"path": "...", "ok": true, "applied": true, "error": null, "diff_preview": "...", "replacements": 1}], "writes": "[...]", "verify": [{"cmd": "...", "exit": 0, "stdout": "...", "stderr": "...", "duration_ms": 12}]},
        "exit_codes": {"0": "all edits and writes applied", "1": "rolled back, failed, or invalid request", "2": "usage / IO error (no JSON, unreadable input)"},
        "limits": {"max_edits": patchify::MAX_EDITS, "max_writes": patchify::MAX_WRITES, "max_file_bytes": patchify::MAX_FILE_BYTES, "max_string_bytes": patchify::MAX_STRING_BYTES}
    });
    serde_json::to_string_pretty(&schema).unwrap()
}
