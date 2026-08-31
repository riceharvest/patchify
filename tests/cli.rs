//! End-to-end CLI matrix: build the binary, drive it against a fixture repo.
//! No network.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin_path() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    // target/debug/deps/<test-harness> -> target/debug/patchify
    p.pop(); // deps
    p.pop(); // debug
    p.join(if cfg!(windows) {
        "patchify.exe"
    } else {
        "patchify"
    })
}

fn run_binary(args: &[&str], stdin: &str, cwd: &Path) -> (Option<i32>, String, String) {
    // `run_binary` is for flag-only invocations; stdin content is ignored here.
    let _ = stdin;
    let out = Command::new(bin_path())
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("spawn patchify");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn run_with_stdin(args: &[&str], stdin: &str, cwd: &Path) -> (Option<i32>, String, String) {
    use std::io::Write;
    let mut child = Command::new(bin_path())
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn patchify");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().expect("wait");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn fixture_repo() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "patchify-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("README"), "demo fixture\n").unwrap();
    dir
}

#[test]
fn help_version_exit_zero() {
    let dir = fixture_repo();
    let (code, out, _) = run_binary(&["--help"], "", &dir);
    assert_eq!(code, Some(0));
    assert!(out.contains("patchify"));
    let (code, out, _) = run_binary(&["--version"], "", &dir);
    assert_eq!(code, Some(0));
    assert!(out.contains(env!("CARGO_PKG_VERSION")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn empty_stdin_is_usage_error() {
    let dir = fixture_repo();
    let (code, _, _) = run_binary(&[], "", &dir);
    assert_eq!(code, Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn three_patch_batch_with_failure_rolls_back_cleanly() {
    let dir = fixture_repo();
    let before = std::fs::read_to_string(dir.join("main.rs")).unwrap();
    let before_lib = std::fs::read_to_string(dir.join("lib.rs")).unwrap();

    // Edit 1 and 2 are valid; edit 3 must fail (old_string appears twice after
    // edit 2 lands... actually give it a nonexistent string) -> full rollback.
    let payload = r#"{
        "edits": [
            {"path": "main.rs", "old_string": "hello", "new_string": "goodbye"},
            {"path": "lib.rs", "old_string": "a + b", "new_string": "a.wrapping_add(b)"},
            {"path": "README", "old_string": "not-present-anywhere", "new_string": "x"}
        ]
    }"#;
    let (code, stdout, _stderr) = run_with_stdin(&[], payload, &dir);
    assert_eq!(code, Some(1), "rolled-back batch exits 1; stdout: {stdout}");
    assert!(stdout.contains("rolled_back"), "{stdout}");
    assert!(stdout.contains("not found"), "{stdout}");
    // No partial state anywhere.
    assert_eq!(
        std::fs::read_to_string(dir.join("main.rs")).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("lib.rs")).unwrap(),
        before_lib
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn full_success_batch_with_verify_commands() {
    let dir = fixture_repo();
    let payload = r#"{
        "writes": [{"path": "src/newmod.rs", "content": "pub const V: &str = \"new\";\n"}],
        "edits": [
            {"path": "main.rs", "old_string": "println!(\"hello\");", "new_string": "println!(\"goodbye\");"}
        ],
        "verify": [
            {"cmd": "cat main.rs"},
            {"cmd": "test -f src/newmod.rs && echo created-ok"},
            {"cmd": "exit 3"}
        ]
    }"#;
    let (code, stdout, _stderr) = run_with_stdin(&[], payload, &dir);
    assert_eq!(code, Some(0), "successful batch exits 0; stdout: {stdout}");
    assert!(stdout.contains("\"status\":\"applied\""), "{stdout}");
    assert!(stdout.contains("goodbye"), "{stdout}");
    assert!(stdout.contains("created-ok"), "{stdout}");
    assert!(stdout.contains("\"exit\":3"), "{stdout}");
    assert!(dir.join("src/newmod.rs").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dry_run_writes_nothing() {
    let dir = fixture_repo();
    let before = std::fs::read_to_string(dir.join("main.rs")).unwrap();
    let payload = r#"{"edits":[{"path":"main.rs","old_string":"hello","new_string":"goodbye"}],"dry_run":true}"#;
    let (code, stdout, _) = run_with_stdin(&[], payload, &dir);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("dry_run"), "{stdout}");
    assert!(
        stdout.contains(r#"+     println!(\"goodbye\");"#),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("main.rs")).unwrap(),
        before
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn traversal_and_absolute_refused_via_cli() {
    let dir = fixture_repo();
    for bad in ["../escape.txt", "/etc/passwd"] {
        let payload =
            format!(r#"{{"edits":[{{"path":"{bad}","old_string":"a","new_string":"b"}}]}}"#);
        let (code, stdout, _) = run_with_stdin(&[], &payload, &dir);
        assert_eq!(code, Some(1), "{bad}: {stdout}");
        assert!(stdout.contains("refused"), "{stdout}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn input_file_flag_reads_request_from_disk() {
    let dir = fixture_repo();
    std::fs::write(
        dir.join("req.json"),
        r#"{"edits":[{"path":"README","old_string":"demo","new_string":"DEMO"}]}"#,
    )
    .unwrap();
    let (code, stdout, _) = run_binary(&["--input", "req.json"], "", &dir);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(stdout.contains("\"applied\""), "{stdout}");
    assert!(
        std::fs::read_to_string(dir.join("README"))
            .unwrap()
            .contains("DEMO")
    );
    // missing file -> exit 2
    let (code, _, _) = run_binary(&["--input", "nope.json"], "", &dir);
    assert_eq!(code, Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn chained_same_path_edits_via_cli() {
    let dir = fixture_repo();
    let payload = r#"{"edits":[
        {"path":"main.rs","old_string":"hello","new_string":"goodbye"},
        {"path":"main.rs","old_string":"goodbye","new_string":"farewell"}
    ]}"#;
    let (code, stdout, _) = run_with_stdin(&[], payload, &dir);
    assert_eq!(code, Some(0), "{stdout}");
    assert!(
        std::fs::read_to_string(dir.join("main.rs"))
            .unwrap()
            .contains("farewell")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tool_manifest_and_completions_and_man_render() {
    let dir = fixture_repo();
    let (code, out, _) = run_binary(&["--tool-manifest"], "", &dir);
    assert_eq!(code, Some(0));
    let v: serde_json::Value = serde_json::from_str(&out).expect("manifest is valid JSON");
    assert_eq!(v["name"], "patchify");
    assert_eq!(v["exit_codes"]["0"], "all edits and writes applied");

    let (code, bash, _) = run_binary(&["--completions", "bash"], "", &dir);
    assert_eq!(code, Some(0));
    assert!(bash.contains("patchify"));

    let (code, man, _) = run_binary(&["--man"], "", &dir);
    assert_eq!(code, Some(0));
    assert!(man.starts_with(".TH") || man.contains("patchify"));
    let _ = std::fs::remove_dir_all(&dir);
}
