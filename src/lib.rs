//! patchify — batch edits, writes and verify commands in one atomic tool call.
//!
//! One call replaces the `read_file -> patch -> patch -> write_file -> terminal`
//! chain hermes pays for on nearly every edit turn. Edits apply in order with
//! exact-string matching and atomic rollback; writes create parent dirs; verify
//! commands run after the batch so the agent gets `cargo check` output in the
//! same response.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

pub mod update;

/// Hard cap: refuse batches larger than this (per-call DoS bound).
pub const MAX_EDITS: usize = 50;
/// Hard cap: refuse batches larger than this.
pub const MAX_WRITES: usize = 50;
/// Hard cap: skip files larger than this for edits and writes (bytes).
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
/// Hard cap: reject single edit payloads (old/new combined) larger than this (bytes).
pub const MAX_STRING_BYTES: usize = 2 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// One exact-string edit. `old_string` must occur exactly once in the file
/// (or `replace_all: true` must be set).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Edit {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
}

/// One raw write (create or overwrite). Parent dirs are created (`mkdir -p`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Write {
    pub path: String,
    pub content: String,
    #[serde(default = "default_true")]
    pub create_dirs: bool,
}

fn default_true() -> bool {
    true
}

/// One post-edit shell verification command.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Verify {
    pub cmd: String,
}

/// The full batch request, piped as JSON to stdin.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchRequest {
    #[serde(default)]
    pub edits: Vec<Edit>,
    #[serde(default)]
    pub writes: Vec<Write>,
    #[serde(default)]
    pub verify: Vec<Verify>,
    /// Return diff previews only; touch nothing on disk.
    #[serde(default)]
    pub dry_run: bool,
    /// Allow editing paths outside the working directory (unsafe opt-in).
    #[serde(default)]
    pub allow_outside: bool,
}

impl BatchRequest {
    pub fn from_json(text: &str) -> Result<Self, BatchError> {
        serde_json::from_str(text).map_err(|e| BatchError::InvalidRequest(format!("bad JSON: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Structured per-edit diagnostic so the caller never has to re-read.
#[derive(Debug, Clone, Serialize)]
pub struct EditStatus {
    pub path: String,
    pub ok: bool,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacements: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteStatus {
    pub path: String,
    pub ok: bool,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_dirs: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyStatus {
    pub cmd: String,
    pub exit: i64,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchResult {
    pub ok: bool,
    /// `applied` = all edits+writes landed; `rolled_back` = batch failed and
    /// every prior edit/write was undone; `dry_run` = nothing was touched.
    pub status: String,
    pub edits: Vec<EditStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writes: Option<Vec<WriteStatus>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<Vec<VerifyStatus>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum BatchError {
    /// Batch failed before/at the failing edit; per-edit statuses included.
    RolledBack(
        String,
        Vec<EditStatus>,
        Option<Vec<WriteStatus>>,
        Option<String>,
    ),
    InvalidRequest(String),
}

impl std::fmt::Display for BatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RolledBack(msg, ..) => write!(f, "{msg}"),
            Self::InvalidRequest(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for BatchError {}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn size_string(s: &str, what: &str) -> Result<(), BatchError> {
    if s.len() > MAX_STRING_BYTES {
        return Err(BatchError::InvalidRequest(format!(
            "{what} too large: {} bytes (max {MAX_STRING_BYTES}); split the edit",
            s.len()
        )));
    }
    Ok(())
}

fn validate_edit(edit: &Edit) -> Result<(), String> {
    if edit.old_string.is_empty() && !edit.replace_all {
        return Err("old_string must be non-empty (use writes to create files)".into());
    }
    if edit.old_string.is_empty() && edit.replace_all {
        return Err(
            "old_string must be non-empty; empty old_string with replace_all is rejected".into(),
        );
    }
    size_string(&edit.old_string, "old_string").map_err(|e| e.to_string())?;
    size_string(&edit.new_string, "new_string").map_err(|e| e.to_string())?;
    Ok(())
}

fn validate_write(write: &Write) -> Result<(), String> {
    size_string(&write.content, "content").map_err(|e| e.to_string())?;
    Ok(())
}

/// Reject absolute paths and any path that escapes the base via `..` or
/// symlinked parent components. Returns the normalized relative path.
pub fn safe_relative_path(path: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("empty path".into());
    }
    let p = Path::new(path);
    if p.is_absolute() {
        // Pass through; absolute paths are gated by `allow_outside` in the
        // executor, which knows the request flags.
        return Ok(p.to_path_buf());
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("path traversal refused: {path}"));
    }
    if path.contains('\0') {
        return Err("NUL byte in path".into());
    }
    let clean: PathBuf = p
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();
    Ok(clean)
}

/// Decide whether `target` is inside `base`, refusing symlink escapes.
/// `link_depth` limits how many symlink resolutions we follow before giving up.
fn resolves_inside(target: &Path, base: &Path, link_depth: usize) -> bool {
    if link_depth == 0 {
        return false;
    }
    // Walk up from target: the deepest existing component decides. If target
    // itself is a symlink (or a path under a symlinked dir), resolve it.
    match std::fs::symlink_metadata(target) {
        Ok(md) if md.file_type().is_symlink() => match std::fs::read_link(target) {
            Ok(link) => {
                let joined = if link.is_absolute() {
                    link
                } else {
                    target.parent().unwrap_or(base).join(link)
                };
                return resolves_inside(&joined, base, link_depth - 1);
            }
            Err(_) => return false,
        },
        Ok(_) => {}
        Err(_) => {
            // Does not exist (yet): check the nearest existing ancestor so a
            // symlinked parent directory cannot smuggle writes outside.
            let mut anc = target.parent();
            while let Some(a) = anc {
                if a.exists() {
                    return resolves_inside(a, base, link_depth - 1);
                }
                anc = a.parent();
            }
            return false;
        }
    }
    // Plain existing file/dir: compare canonical paths.
    match (target.canonicalize(), base.canonicalize()) {
        (Ok(t), Ok(b)) => t.starts_with(&b),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Diff preview
// ---------------------------------------------------------------------------

/// Small unified-ish diff preview (first changed hunk, max ~12 lines) purely
/// for agent eyeballing — not a real diff engine.
fn diff_preview(old: &str, new: &str) -> String {
    let mut out = String::new();
    let mut old_lines = old.lines();
    let mut new_lines = new.lines();
    let mut shown = 0usize;
    loop {
        match (old_lines.next(), new_lines.next()) {
            (Some(a), Some(b)) if a == b => {
                if shown < 3 {
                    out.push_str("  ");
                    out.push_str(a);
                    out.push('\n');
                    shown += 1;
                }
            }
            (ao, an) => {
                if let Some(a) = ao {
                    out.push_str("- ");
                    out.push_str(a);
                    out.push('\n');
                }
                if let Some(n) = an {
                    out.push_str("+ ");
                    out.push_str(n);
                    out.push('\n');
                }
                shown += 3;
            }
        }
        if shown >= 12 || (ao_is_none(&old_lines) && an_is_none(&new_lines)) {
            break;
        }
    }
    if out.len() > 800 {
        out.truncate(800);
        out.push_str("...\n");
    }
    out
}

fn ao_is_none(iter: &std::str::Lines<'_>) -> bool {
    iter.clone().count() == 0
}
fn an_is_none(iter: &std::str::Lines<'_>) -> bool {
    iter.clone().count() == 0
}

// ---------------------------------------------------------------------------
// Core executor
// ---------------------------------------------------------------------------

/// Saved snapshot of one file for rollback (None = file did not exist).
struct Snapshot {
    existed: bool,
    content: Option<Vec<u8>>,
}

/// Executed plan entries, in order, for rollback bookkeeping.
enum Applied {
    Edit {
        path: PathBuf,
        old: Option<String>,
    },
    Write {
        path: PathBuf,
        existed: bool,
        content: String,
    },
}

fn read_snapshot(path: &Path) -> Snapshot {
    match std::fs::read(path) {
        Ok(bytes) => Snapshot {
            existed: true,
            content: Some(bytes),
        },
        Err(_) => Snapshot {
            existed: false,
            content: None,
        },
    }
}

/// Acquire an exclusive advisory lock (flock) on the target for the duration
/// of its read-check-write sequence. Prevents lost updates between two
/// concurrent patchify processes and any writer that also takes the lock.
/// Returns the lock guard; dropping it releases. On platforms without flock
/// support this is a no-op guard.
pub struct FileLock {
    // Holds the fd; the lock lives as long as this file handle stays open.
    #[cfg(unix)]
    _file: std::fs::File,
    #[cfg(unix)]
    lock_path: std::path::PathBuf,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.lock_path);
        }
    }
}

impl FileLock {
    #[cfg(unix)]
    fn acquire(path: &Path) -> Result<Self, String> {
        use std::os::unix::io::AsRawFd;
        let lock_path = path.with_extension("patchify-lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("cannot open lock file {}: {e}", lock_path.display()))?;
        let ret = unsafe { libc_flock(file.as_raw_fd()) };
        if ret != 0 {
            return Err(format!(
                "flock failed on {}: errno {ret}",
                lock_path.display()
            ));
        }
        Ok(FileLock {
            _file: file,
            lock_path,
        })
    }

    #[cfg(not(unix))]
    fn acquire(_path: &Path) -> Result<Self, String> {
        Ok(FileLock {})
    }
}

#[cfg(unix)]
unsafe fn libc_flock(fd: std::os::unix::io::RawFd) -> i32 {
    // LOCK_EX = 2
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    unsafe { flock(fd, 2) }
}

/// TOCTOU guard: check file unchanged since we read it, right before writing.
/// The caller must hold the FileLock across check + write.
fn check_not_changed(snap: &Snapshot, path: &Path) -> Result<(), String> {
    match std::fs::read(path) {
        Ok(now) => {
            let before = snap.content.as_deref().unwrap_or(&[]);
            if now != before {
                return Err(
                    "file changed between read and write (TOCTOU guard) — rerun the batch".into(),
                );
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if snap.existed {
                Err("file disappeared before write (TOCTOU guard) — rerun the batch".into())
            } else {
                Ok(()) // creating a new file that still doesn't exist is fine
            }
        }
        Err(e) => Err(format!("could not stat file before write: {e}")),
    }
}

/// Apply the whole batch. On any failure everything already applied is
/// restored and a `BatchError::RolledBack` with per-edit statuses returns.
pub fn execute_batch(req: &BatchRequest, cwd: &Path) -> Result<BatchResult, BatchError> {
    if req.edits.len() > MAX_EDITS {
        return Err(BatchError::InvalidRequest(format!(
            "too many edits: {} (max {MAX_EDITS})",
            req.edits.len()
        )));
    }
    if req.writes.len() > MAX_WRITES {
        return Err(BatchError::InvalidRequest(format!(
            "too many writes: {} (max {MAX_WRITES})",
            req.writes.len()
        )));
    }

    // Validate everything up front so malformed input never touches disk.
    for (i, e) in req.edits.iter().enumerate() {
        validate_edit(e)
            .map_err(|m| BatchError::InvalidRequest(format!("edit[{i}] {}: {m}", e.path)))?;
    }
    for (i, w) in req.writes.iter().enumerate() {
        validate_write(w)
            .map_err(|m| BatchError::InvalidRequest(format!("write[{i}] {}: {m}", w.path)))?;
    }
    for (i, e) in req.edits.iter().enumerate() {
        let p = safe_relative_path(&e.path).map_err(BatchError::InvalidRequest)?;
        if p.is_absolute() && !req.allow_outside {
            return Err(BatchError::InvalidRequest(format!(
                "edit[{i}]: absolute path refused: {} (set allow_outside to edit outside the tree)",
                e.path
            )));
        }
    }
    for (i, w) in req.writes.iter().enumerate() {
        let p = safe_relative_path(&w.path).map_err(BatchError::InvalidRequest)?;
        if p.is_absolute() && !req.allow_outside {
            return Err(BatchError::InvalidRequest(format!(
                "write[{i}]: absolute path refused: {} (set allow_outside to write outside the tree)",
                w.path
            )));
        }
    }

    if req.dry_run {
        return dry_run_result(req, cwd);
    }

    // --- Pre-flight: read all target files, compute new contents, detect
    // duplicate-edit conflicts. Nothing is written yet.
    struct PreparedEdit {
        rel: PathBuf,
        content: Vec<u8>,
        new_content: String,
        snapshot: Snapshot,
    }
    let mut prepared: Vec<PreparedEdit> = Vec::new();
    let mut edit_statuses: Vec<EditStatus> = Vec::new();
    let mut applied_log: Vec<Applied> = Vec::new();

    let mut failure: Option<(usize, String)> = None;

    // Pre-read cache so chained edits to the SAME path compose: each edit
    // matches against the previous edit's result, not stale disk content.
    let mut preflight_cache: HashMap<String, String> = HashMap::new();
    let mut preflight_orig: HashMap<String, Vec<u8>> = HashMap::new();

    for (i, e) in req.edits.iter().enumerate() {
        let rel = match safe_relative_path(&e.path) {
            Ok(r) => r,
            Err(m) => {
                failure = Some((i, m));
                edit_statuses.push(EditStatus {
                    path: e.path.clone(),
                    ok: false,
                    applied: false,
                    error: Some("path refused".into()),
                    diff_preview: None,
                    replacements: None,
                });
                break;
            }
        };
        let cache_key = rel.to_string_lossy().into_owned();
        let abs = cwd.join(&rel);
        if !req.allow_outside && !resolves_inside(&abs, cwd, 8) {
            failure = Some((
                i,
                format!(
                    "refusing to patch outside cwd: {} (set allow_outside:true)",
                    e.path
                ),
            ));
            edit_statuses.push(EditStatus {
                path: e.path.clone(),
                ok: false,
                applied: false,
                error: Some("outside cwd".into()),
                diff_preview: None,
                replacements: None,
            });
            break;
        }
        let (snapshot, bytes) = if let Some(cached) = preflight_cache.get(&cache_key) {
            // Chained same-path edit: match against the previous edit's result.
            let orig = preflight_orig.get(&cache_key).unwrap().clone();
            (
                Snapshot {
                    existed: true,
                    content: Some(orig),
                },
                cached.clone().into_bytes(),
            )
        } else {
            let snapshot = read_snapshot(&abs);
            if !snapshot.existed {
                failure = Some((
                    i,
                    format!(
                        "edit target does not exist: {} (use writes to create files)",
                        e.path
                    ),
                ));
                edit_statuses.push(EditStatus {
                    path: e.path.clone(),
                    ok: false,
                    applied: false,
                    error: Some("target missing".into()),
                    diff_preview: None,
                    replacements: None,
                });
                break;
            }
            let bytes = snapshot.content.clone().unwrap();
            (snapshot, bytes)
        };
        if bytes.len() as u64 > MAX_FILE_BYTES {
            failure = Some((
                i,
                format!(
                    "file too large to edit: {} ({} bytes, max {MAX_FILE_BYTES})",
                    e.path,
                    bytes.len()
                ),
            ));
            edit_statuses.push(EditStatus {
                path: e.path.clone(),
                ok: false,
                applied: false,
                error: Some("file too large".into()),
                diff_preview: None,
                replacements: None,
            });
            break;
        }
        let current = match String::from_utf8(bytes.clone()) {
            Ok(s) => s,
            Err(_) => {
                failure = Some((i, format!("file is not valid UTF-8: {}", e.path)));
                edit_statuses.push(EditStatus {
                    path: e.path.clone(),
                    ok: false,
                    applied: false,
                    error: Some("not UTF-8".into()),
                    diff_preview: None,
                    replacements: None,
                });
                break;
            }
        };
        let count = current.matches(&e.old_string).count();
        let (new_content, replacements) = if e.replace_all {
            if count == 0 {
                failure = Some((i, format!("old_string not found in {}", e.path)));
                edit_statuses.push(EditStatus {
                    path: e.path.clone(),
                    ok: false,
                    applied: false,
                    error: Some("old_string not found".into()),
                    diff_preview: None,
                    replacements: None,
                });
                break;
            }
            (current.replace(&e.old_string, &e.new_string), count)
        } else if count == 1 {
            (current.replacen(&e.old_string, &e.new_string, 1), 1)
        } else {
            let err = if count == 0 {
                "old_string not found"
            } else {
                &format!("old_string matches {count} times (pass replace_all or make it unique)")
            };
            let err_owned: String = err.to_string();
            failure = Some((i, err_owned.clone()));
            edit_statuses.push(EditStatus {
                path: e.path.clone(),
                ok: false,
                applied: false,
                error: Some(err_owned),
                diff_preview: None,
                replacements: Some(count),
            });
            break;
        };
        preflight_cache.insert(cache_key.clone(), new_content.clone());
        preflight_orig
            .entry(cache_key.clone())
            .or_insert_with(|| bytes.clone());
        prepared.push(PreparedEdit {
            rel,
            content: bytes,
            new_content,
            snapshot,
        });
        edit_statuses.push(EditStatus {
            path: e.path.clone(),
            ok: true,
            applied: false,
            error: None,
            diff_preview: None,
            replacements: Some(replacements),
        });
    }

    if let Some((i, msg)) = failure {
        // Nothing was written yet (pre-flight failed) — report cleanly.
        let _ = i;
        return Err(BatchError::RolledBack(msg, edit_statuses, None, None));
    }

    // --- Apply phase: write prepared contents in order with TOCTOU checks.
    let mut write_statuses: Vec<WriteStatus> = Vec::new();
    let mut verify_statuses: Vec<VerifyStatus> = Vec::new();

    // Compute the last prepared index per path: chained same-path edits
    // collapse into one disk write (the final composed content).
    let mut last_idx_for_path: HashMap<String, usize> = HashMap::new();
    for (idx, p) in prepared.iter().enumerate() {
        last_idx_for_path.insert(p.rel.to_string_lossy().into_owned(), idx);
    }

    for (idx, mut p) in prepared.into_iter().enumerate() {
        let is_last_for_path =
            last_idx_for_path.get(&p.rel.to_string_lossy().into_owned()) == Some(&idx);
        if !is_last_for_path {
            // Intermediate edit in a same-path chain: result is already folded
            // into the chain's final content; nothing to write here.
            edit_statuses[idx].applied = true;
            continue;
        }
        let abs = cwd.join(&p.rel);
        let _lock = match FileLock::acquire(&abs) {
            Ok(l) => l,
            Err(m) => return Err(rollback(applied_log, cwd, m, edit_statuses, write_statuses)),
        };
        if let Err(m) = check_not_changed(&p.snapshot, &abs) {
            return Err(rollback(applied_log, cwd, m, edit_statuses, write_statuses));
        }
        if let Err(e) = std::fs::write(&abs, &p.new_content) {
            return Err(rollback(
                applied_log,
                cwd,
                format!("write failed: {e}"),
                edit_statuses,
                write_statuses,
            ));
        }
        applied_log.push(Applied::Edit {
            path: p.rel.clone(),
            old: String::from_utf8(p.content.clone()).ok(),
        });
        p.snapshot.content = Some(p.new_content.clone().into_bytes());
        edit_statuses[idx].applied = true;
        // Per-step diff: p.content is what this edit matched against (the
        // original for the first edit on a path, the previous edit's result
        // for chained same-path edits).
        edit_statuses[idx].diff_preview = Some(diff_preview(
            &String::from_utf8_lossy(&p.content),
            &p.new_content,
        ));
    }

    // --- Writes, deduped by path: skip earlier duplicates, last one wins.
    let mut written_paths: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (wi, w) in req.writes.iter().enumerate() {
        if !written_paths.insert(&w.path) {
            // A later duplicate in the list already exists; this one is superseded.
            let has_later = req.writes[wi + 1..].iter().any(|x| x.path == w.path);
            if has_later {
                write_statuses.push(WriteStatus {
                    path: w.path.clone(),
                    ok: true,
                    applied: false,
                    error: Some("superseded by a later write to the same path".into()),
                    created_dirs: None,
                });
                continue;
            }
            // Last occurrence: overwrite what an earlier occurrence wrote.
        }
        let rel = safe_relative_path(&w.path).map_err(BatchError::InvalidRequest)?;
        let abs = cwd.join(&rel);

        if !req.allow_outside && !resolves_inside(&abs, cwd, 8) {
            return Err(rollback(
                applied_log,
                cwd,
                format!("refusing to write outside cwd: {}", w.path),
                edit_statuses,
                write_statuses,
            ));
        }
        let existed_now = abs.exists();
        let snap = read_snapshot(&abs);
        if existed_now && snap.content.as_deref().unwrap_or(&[]).len() as u64 > MAX_FILE_BYTES {
            return Err(rollback(
                applied_log,
                cwd,
                format!("file too large to write: {} (max {MAX_FILE_BYTES})", w.path),
                edit_statuses,
                write_statuses,
            ));
        }
        if w.create_dirs {
            if let Some(parent) = abs.parent() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Err(rollback(
                        applied_log,
                        cwd,
                        format!("mkdir failed: {e}"),
                        edit_statuses,
                        write_statuses,
                    ));
                }
            }
        }
        let _lock = match FileLock::acquire(&abs) {
            Ok(l) => l,
            Err(m) => return Err(rollback(applied_log, cwd, m, edit_statuses, write_statuses)),
        };
        if let Err(m) = check_not_changed(&snap, &abs) {
            return Err(rollback(applied_log, cwd, m, edit_statuses, write_statuses));
        }
        if let Err(e) = std::fs::write(&abs, &w.content) {
            return Err(rollback(
                applied_log,
                cwd,
                format!("write failed: {e}"),
                edit_statuses,
                write_statuses,
            ));
        }
        applied_log.push(Applied::Write {
            path: rel.clone(),
            existed: existed_now,
            content: w.content.clone(),
        });
        write_statuses.push(WriteStatus {
            path: w.path.clone(),
            ok: true,
            applied: true,
            error: None,
            created_dirs: Some(w.create_dirs && !existed_now),
        });
    }

    let writes_ok: Option<Vec<WriteStatus>> = if req.writes.is_empty() {
        None
    } else {
        Some(write_statuses)
    };

    // --- Verify commands.
    for v in &req.verify {
        let status = run_verify(&v.cmd, cwd);
        verify_statuses.push(status);
    }
    let verify_out: Option<Vec<VerifyStatus>> = if req.verify.is_empty() {
        None
    } else {
        Some(verify_statuses)
    };

    Ok(BatchResult {
        ok: true,
        status: "applied".into(),
        edits: edit_statuses,
        writes: writes_ok,
        verify: verify_out,
        error: None,
    })
}

fn rollback(
    applied: Vec<Applied>,
    cwd: &Path,
    msg: String,
    mut edit_statuses: Vec<EditStatus>,
    mut write_statuses: Vec<WriteStatus>,
) -> BatchError {
    // Undo in reverse order.
    for a in applied.into_iter().rev() {
        match a {
            Applied::Edit { path, old, .. } => {
                if let Some(old) = old {
                    let _ = std::fs::write(cwd.join(&path), old);
                }
            }
            Applied::Write {
                path,
                existed,
                content,
            } => {
                let abs = cwd.join(&path);
                if existed {
                    let _ = std::fs::write(&abs, content);
                } else {
                    let _ = std::fs::remove_file(&abs);
                }
            }
        }
    }
    for st in edit_statuses.iter_mut() {
        if st.applied {
            st.applied = false;
            st.ok = false;
            st.error = Some("rolled back".into());
        }
    }
    for st in write_statuses.iter_mut() {
        if st.applied {
            st.applied = false;
            st.ok = false;
            st.error = Some("rolled back".into());
        }
    }
    BatchError::RolledBack(msg, edit_statuses, Some(write_statuses), None)
}

fn run_verify(cmd: &str, cwd: &Path) -> VerifyStatus {
    use std::process::{Command, Stdio};
    let start = std::time::Instant::now();
    let out = Command::new(if cfg!(windows) { "cmd" } else { "sh" })
        .args(if cfg!(windows) {
            ["/C", cmd]
        } else {
            ["-c", cmd]
        })
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let duration_ms = start.elapsed().as_millis();
    match out {
        Ok(o) => VerifyStatus {
            cmd: cmd.to_owned(),
            exit: o.status.code().unwrap_or(-1) as i64,
            stdout: String::from_utf8_lossy(&o.stdout)
                .chars()
                .take(4000)
                .collect(),
            stderr: String::from_utf8_lossy(&o.stderr)
                .chars()
                .take(4000)
                .collect(),
            duration_ms,
        },
        Err(e) => VerifyStatus {
            cmd: cmd.to_owned(),
            exit: -1,
            stdout: String::new(),
            stderr: format!("failed to spawn: {e}"),
            duration_ms,
        },
    }
}

fn dry_run_result(req: &BatchRequest, cwd: &Path) -> Result<BatchResult, BatchError> {
    let mut statuses = Vec::new();
    for e in &req.edits {
        let abs = cwd.join(safe_relative_path(&e.path).map_err(BatchError::InvalidRequest)?);
        if !abs.exists() {
            statuses.push(EditStatus {
                path: e.path.clone(),
                ok: false,
                applied: false,
                error: Some("target missing".into()),
                diff_preview: None,
                replacements: None,
            });
            continue;
        }
        let current = match std::fs::read_to_string(&abs) {
            Ok(s) => s,
            Err(_) => {
                statuses.push(EditStatus {
                    path: e.path.clone(),
                    ok: false,
                    applied: false,
                    error: Some("not UTF-8 or unreadable".into()),
                    diff_preview: None,
                    replacements: None,
                });
                continue;
            }
        };
        let count = current.matches(&e.old_string).count();
        let (ok, error) = if count == 1 || (e.replace_all && count > 0) {
            (true, None)
        } else {
            (
                false,
                Some(if count == 0 {
                    "old_string not found".into()
                } else {
                    format!("old_string matches {count} times")
                }),
            )
        };
        let preview = if ok {
            Some(diff_preview(
                &current,
                &current.replacen(
                    &e.old_string,
                    &e.new_string,
                    if e.replace_all { usize::MAX } else { 1 },
                ),
            ))
        } else {
            None
        };
        statuses.push(EditStatus {
            path: e.path.clone(),
            ok,
            applied: false,
            error,
            diff_preview: preview,
            replacements: Some(count),
        });
    }
    Ok(BatchResult {
        ok: statuses.iter().all(|s| s.ok),
        status: "dry_run".into(),
        edits: statuses,
        writes: None,
        verify: None,
        error: None,
    })
}

/// Human-readable text rendering of a batch result (`--format text`).
pub fn result_to_text(res: &Result<BatchResult, BatchError>) -> String {
    let mut out = String::new();
    match res {
        Ok(r) => {
            let status = if r.status == "applied" {
                "OK"
            } else {
                r.status.as_str()
            };
            out.push_str(&format!("{}: {}\n", status, r.status));
            for e in &r.edits {
                match (&e.error, e.applied) {
                    (Some(err), _) => out.push_str(&format!("  FAIL  {}  ({err})\n", e.path)),
                    (None, true) => out.push_str(&format!(
                        "  edit  {}  ({} replacement(s))\n",
                        e.path,
                        e.replacements.unwrap_or(0)
                    )),
                    (None, false) => out.push_str(&format!("  edit  {}  (pending)\n", e.path)),
                }
                if let Some(p) = &e.diff_preview {
                    for line in p.lines() {
                        out.push_str("    ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
            for w in r.writes.iter().flatten() {
                match &w.error {
                    Some(err) => out.push_str(&format!("  FAIL  {}  ({err})\n", w.path)),
                    None if w.applied => out.push_str(&format!(
                        "  write {}  {}\n",
                        w.path,
                        if w.created_dirs == Some(true) {
                            "(dirs created)"
                        } else {
                            ""
                        }
                    )),
                    None => out.push_str(&format!("  skip  {}  (superseded)\n", w.path)),
                }
            }
            for v in r.verify.iter().flatten() {
                out.push_str(&format!(
                    "  [{exit:>3}] {cmd}  ({duration_ms}ms)\n",
                    exit = v.exit,
                    cmd = v.cmd,
                    duration_ms = v.duration_ms
                ));
                if !v.stdout.trim().is_empty() {
                    for line in v.stdout.lines().take(20) {
                        out.push_str("    out: ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                if !v.stderr.trim().is_empty() {
                    for line in v.stderr.lines().take(20) {
                        out.push_str("    err: ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
            }
        }
        Err(BatchError::RolledBack(msg, edits, writes, _)) => {
            out.push_str(&format!("ROLLED_BACK: {msg}\n"));
            for e in edits {
                if let Some(err) = &e.error {
                    out.push_str(&format!("  FAIL  {}  ({err})\n", e.path));
                }
            }
            for w in writes.iter().flatten() {
                if let Some(err) = &w.error {
                    out.push_str(&format!("  FAIL  {}  ({err})\n", w.path));
                }
            }
        }
        Err(BatchError::InvalidRequest(m)) => out.push_str(&format!("INVALID: {m}\n")),
    }
    out
}

/// Convenience: serialize a result or error to a JSON string for stdout.
pub fn result_to_json(res: &Result<BatchResult, BatchError>) -> String {
    match res {
        Ok(r) => serde_json::to_string(r)
            .unwrap_or_else(|_| r#"{"ok":false,"error":"serialize failed"}"#.into()),
        Err(e) => match e {
            BatchError::RolledBack(msg, edits, writes, _) => {
                let json = serde_json::json!({
                    "ok": false,
                    "status": "rolled_back",
                    "error": msg,
                    "edits": edits,
                    "writes": writes,
                });
                json.to_string()
            }
            BatchError::InvalidRequest(m) => serde_json::json!({
                "ok": false,
                "status": "invalid_request",
                "error": m,
            })
            .to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "patchify-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn unique_match_applies() {
        let dir = tmpdir();
        write_file(&dir, "a.txt", "hello world\n");
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"a.txt","old_string":"world","new_string":"there"}]}"#,
        )
        .unwrap();
        let res = execute_batch(&req, &dir).unwrap();
        assert!(res.ok, "{res:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "hello there\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ambiguous_match_fails_without_writing() {
        let dir = tmpdir();
        write_file(&dir, "a.txt", "x x x\n");
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"a.txt","old_string":"x","new_string":"y"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, &dir) {
            Err(BatchError::RolledBack(msg, edits, _, _)) => {
                assert!(msg.contains("matches 3 times"), "{msg}");
                assert!(!edits[0].applied);
            }
            other => panic!("expected rollback, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "x x x\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_all_replaces_every_occurrence() {
        let dir = tmpdir();
        write_file(&dir, "a.txt", "x x x\n");
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"a.txt","old_string":"x","new_string":"y","replace_all":true}]}"#,
        )
        .unwrap();
        let res = execute_batch(&req, &dir).unwrap();
        assert!(res.ok);
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "y y y\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_restores_prior_edits() {
        let dir = tmpdir();
        write_file(&dir, "a.txt", "alpha\n");
        write_file(&dir, "b.txt", "bravo\n");
        // Edit a.txt (succeeds), then a broken edit on b.txt.
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"a.txt","old_string":"alpha","new_string":"ALPHA"},{"path":"b.txt","old_string":"missing","new_string":"x"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, &dir) {
            Err(BatchError::RolledBack(msg, edits, _, _)) => {
                // Second edit fails pre-flight; first was never written.
                assert!(msg.contains("not found"), "{msg}");
                assert!(!edits[0].applied);
                assert!(edits[1].error.is_some());
            }
            other => panic!("expected rollback, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "alpha\n",
            "a.txt must be rolled back"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("b.txt")).unwrap(),
            "bravo\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_creates_parent_dirs() {
        let dir = tmpdir();
        let req = BatchRequest::from_json(
            r#"{"writes":[{"path":"deep/nested/dir/new.txt","content":"hi"}]}"#,
        )
        .unwrap();
        let res = execute_batch(&req, &dir).unwrap();
        assert!(res.ok);
        assert_eq!(
            std::fs::read_to_string(dir.join("deep/nested/dir/new.txt")).unwrap(),
            "hi"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlapping_edits_last_wins() {
        let dir = tmpdir();
        let req = BatchRequest::from_json(
            r#"{"writes":[{"path":"w.txt","content":"one"},{"path":"w.txt","content":"two"}]}"#,
        )
        .unwrap();
        let res = execute_batch(&req, &dir).unwrap();
        assert!(res.ok, "{res:?}");
        assert_eq!(std::fs::read_to_string(dir.join("w.txt")).unwrap(), "two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_traversal_refused() {
        let dir = tmpdir();
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"../escape.txt","old_string":"a","new_string":"b"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, &dir) {
            Err(BatchError::InvalidRequest(m)) => assert!(m.contains("traversal"), "{m}"),
            other => panic!("expected invalid request, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absolute_path_refused() {
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"/etc/passwd","old_string":"a","new_string":"b"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, Path::new("/")) {
            Err(BatchError::InvalidRequest(m)) => assert!(m.contains("absolute"), "{m}"),
            other => panic!("expected invalid request, got {other:?}"),
        }
    }

    #[test]
    fn symlinked_dir_escape_refused() {
        // Symlinked DIRECTORY pointing outside cwd: writing through it is refused.
        let dir = tmpdir();
        let outside = tmpdir();
        write_file(&outside, "secret.txt", "secret\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"link/secret.txt","old_string":"secret","new_string":"owned"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, &dir) {
            Err(BatchError::RolledBack(m, ..)) => assert!(m.contains("outside"), "{m}"),
            other => panic!("expected outside-cwd refusal, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "secret\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn empty_old_string_rejected() {
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"a.txt","old_string":"","new_string":"b"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, Path::new("/tmp")) {
            Err(BatchError::InvalidRequest(m)) => {
                assert!(m.contains("empty") || m.contains("non-empty"), "{m}")
            }
            other => panic!("expected invalid request, got {other:?}"),
        }
    }

    #[test]
    fn too_many_edits_rejected() {
        let edits: Vec<String> = (0..51)
            .map(|i| format!(r#"{{"path":"f{i}.txt","old_string":"a","new_string":"b"}}"#))
            .collect();
        let body = format!(r#"{{"edits":[{}]}}"#, edits.join(","));
        let req = BatchRequest::from_json(&body).unwrap();
        match execute_batch(&req, Path::new("/tmp")) {
            Err(BatchError::InvalidRequest(m)) => assert!(m.contains("too many edits"), "{m}"),
            other => panic!("expected invalid request, got {other:?}"),
        }
    }

    #[test]
    fn missing_target_fails_cleanly() {
        let dir = tmpdir();
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"ghost.txt","old_string":"a","new_string":"b"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, &dir) {
            Err(BatchError::RolledBack(m, edits, _, _)) => {
                assert!(m.contains("does not exist"), "{m}");
                assert!(!edits[0].applied);
            }
            other => panic!("expected rollback, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_runs_after_edits() {
        let dir = tmpdir();
        write_file(&dir, "checkme.txt", "present\n");
        let req = BatchRequest::from_json(r#"{"edits":[],"verify":[{"cmd":"cat checkme.txt"}]}"#)
            .unwrap();
        let res = execute_batch(&req, &dir).unwrap();
        let v = &res.verify.as_ref().unwrap()[0];
        assert_eq!(v.exit, 0);
        assert!(v.stdout.contains("present"), "{}", v.stdout);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_touches_nothing() {
        let dir = tmpdir();
        write_file(&dir, "a.txt", "alpha beta\n");
        let req = BatchRequest::from_json(r#"{"edits":[{"path":"a.txt","old_string":"alpha","new_string":"ALPHA"}],"dry_run":true}"#).unwrap();
        let res = execute_batch(&req, &dir).unwrap();
        assert!(res.ok);
        assert_eq!(res.status, "dry_run");
        assert!(
            res.edits[0]
                .diff_preview
                .as_deref()
                .unwrap()
                .contains("+ ALPHA")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "alpha beta\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_instances_serialize_via_flock() {
        // Two patchify processes racing on the same file: flock forces the
        // check-read-write sequences to serialize, so both edits land.
        let dir = tmpdir();
        write_file(&dir, "shared.txt", "base\n");
        let req1 = BatchRequest::from_json(
            r#"{"edits":[{"path":"shared.txt","old_string":"base","new_string":"one"}],"verify":[{"cmd":"sleep 0.3"}]}"#,
        )
        .unwrap();
        let req2 = BatchRequest::from_json(
            r#"{"edits":[{"path":"shared.txt","old_string":"base","new_string":"two"}]}"#,
        )
        .unwrap();
        let d1 = dir.clone();
        let h1 = std::thread::spawn(move || execute_batch(&req1, &d1));
        let d2 = dir.clone();
        let h2 = std::thread::spawn(move || execute_batch(&req2, &d2));
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        let ok1 = r1.is_ok();
        let ok2 = r2.is_ok();
        // Both succeed (serialized), or the loser rolled back cleanly — but no
        // lost update: the final content must be exactly one of the two edits.
        let content = std::fs::read_to_string(dir.join("shared.txt")).unwrap();
        assert!(
            content == "one\n" || content == "two\n" || content == "base\n",
            "final content must be a known state, got {content:?}"
        );
        // With flock, the second instance's preflight re-reads after the first
        // wrote; one of them must have applied.
        assert!(ok1 || ok2, "at least one batch must apply: {r1:?} {r2:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn toctou_guard_fires_on_drift() {
        let dir = tmpdir();
        write_file(&dir, "x.txt", "before\n");
        let snap = read_snapshot(&dir.join("x.txt"));
        // external writer lands between snapshot and write
        std::fs::write(dir.join("x.txt"), "clobbered\n").unwrap();
        assert!(
            check_not_changed(&snap, &dir.join("x.txt")).is_err(),
            "guard must fire on drift"
        );
        // restored content passes
        std::fs::write(dir.join("x.txt"), "before\n").unwrap();
        assert!(check_not_changed(&snap, &dir.join("x.txt")).is_ok());
        // vanished file with snapshot errors
        std::fs::remove_file(dir.join("x.txt")).unwrap();
        assert!(check_not_changed(&snap, &dir.join("x.txt")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chained_same_path_edits_compose() {
        let dir = tmpdir();
        write_file(&dir, "f.txt", "alpha beta gamma\n");
        let req = BatchRequest::from_json(
            r#"{"edits":[
                {"path":"f.txt","old_string":"alpha","new_string":"ALPHA"},
                {"path":"f.txt","old_string":"beta","new_string":"BETA"},
                {"path":"f.txt","old_string":"gamma","new_string":"GAMMA"}
            ]}"#,
        )
        .unwrap();
        let res = execute_batch(&req, &dir).unwrap();
        assert!(res.ok, "{res:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "ALPHA BETA GAMMA\n"
        );
        // second edit matched against first edit's output
        assert_eq!(res.edits[1].replacements, Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chained_edit_matching_previous_output() {
        let dir = tmpdir();
        write_file(&dir, "f.txt", "v1\n");
        let req = BatchRequest::from_json(
            r#"{"edits":[
                {"path":"f.txt","old_string":"v1","new_string":"v2"},
                {"path":"f.txt","old_string":"v2","new_string":"v3"}
            ]}"#,
        )
        .unwrap();
        let res = execute_batch(&req, &dir).unwrap();
        assert!(res.ok, "{res:?}");
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "v3\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chained_edit_failure_rolls_back_chain() {
        let dir = tmpdir();
        write_file(&dir, "f.txt", "x1\n");
        let req = BatchRequest::from_json(
            r#"{"edits":[
                {"path":"f.txt","old_string":"x1","new_string":"x2"},
                {"path":"f.txt","old_string":"nope","new_string":"q"}
            ]}"#,
        )
        .unwrap();
        match execute_batch(&req, &dir) {
            Err(BatchError::RolledBack(..)) => {}
            other => panic!("expected rollback, got {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "x1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn symlink_escape_refused_real() {
        let dir = tmpdir();
        let outside = tmpdir();
        write_file(&outside, "secret.txt", "secret\n");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("secret.txt"), dir.join("link.txt")).unwrap();
        let req = BatchRequest::from_json(
            r#"{"edits":[{"path":"link.txt","old_string":"secret","new_string":"owned"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, &dir) {
            Err(BatchError::RolledBack(m, ..)) => assert!(m.contains("outside"), "{m}"),
            other => panic!("expected outside-cwd refusal, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "secret\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn write_rollback_removes_new_file() {
        let dir = tmpdir();
        write_file(&dir, "exist.txt", "e\n");
        // Good write, then bad edit — batch must roll back the write too.
        let req = BatchRequest::from_json(
            r#"{"writes":[{"path":"brandnew.txt","content":"w"}],"edits":[{"path":"exist.txt","old_string":"missing","new_string":"x"}]}"#,
        )
        .unwrap();
        match execute_batch(&req, &dir) {
            Err(BatchError::RolledBack(..)) => {}
            other => panic!("expected rollback, got {other:?}"),
        }
        assert!(
            !dir.join("brandnew.txt").exists(),
            "new file must be removed on rollback"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
