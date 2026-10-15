//! `looka`: the old command name, kept so existing MCP registrations and scripts keep working.
use std::os::unix::process::CommandExt;

fn main() {
    let lkc = std::env::current_exe().map(|p| p.with_file_name("lkc")).unwrap_or_else(|_| "lkc".into());
    let err = std::process::Command::new(&lkc).args(std::env::args_os().skip(1)).exec();
    eprintln!("looka: could not run {}: {err}", lkc.display());
    std::process::exit(127)
}
