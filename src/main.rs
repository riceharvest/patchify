use std::process::ExitCode;

fn print_help() {
    println!("Usage: agentic-edit [OPTIONS] [--update] [--help] [--version]");
    println!();
    println!("agentic-edit - batch tool for AI agents (pure Rust). One call does what used to cost N.");
    println!();
    println!("Options:");
    println!("  --update    Self-update from GitHub Releases (checksum-verified)");
    println!("  -h, --help  Print help");
    println!("  -V, --version  Print version");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a=="--help" || a=="-h") { print_help(); return ExitCode::SUCCESS; }
    if args.iter().any(|a| a=="--version" || a=="-V") { println!("agentic-edit {}", env!("CARGO_PKG_VERSION")); return ExitCode::SUCCESS; }
    if args.iter().any(|a| a=="--update") {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
        match rt.block_on(agentic_edit::update::run_update()) {
            Ok(m) => { println!("{m}"); return ExitCode::SUCCESS; }
            Err(agentic_edit::update::UpdateError::UpToDate(m)) => { println!("{m}"); return ExitCode::SUCCESS; }
            Err(e) => { eprintln!("agentic-edit update: {e}"); return ExitCode::FAILURE; }
        }
    }
    // TODO: implement batch JSON stdin protocol - stub for now
    eprintln!("agentic-edit: batch protocol not yet implemented (stub builds, tests pass)");
    eprintln!("Pipe BatchRequest JSON to stdin or use library API directly.");
    ExitCode::from(2)
}
