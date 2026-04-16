use std::process::Command;

fn lumitide() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lumitide"))
}

#[test]
fn help_exits_zero_and_names_app() {
    let out = lumitide().arg("--help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("lumitide"));
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    let out = lumitide().arg("foobar").output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn search_help_exits_zero_and_mentions_query() {
    let out = lumitide().args(["search", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("query"));
}

#[test]
fn search_no_query_exits_nonzero() {
    // The app prints a custom clap error and exits before touching the network
    let out = lumitide().arg("search").output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn search_no_query_error_mentions_search() {
    let out = lumitide().arg("search").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("search") || stderr.contains("query"));
}

#[test]
fn search_invalid_limit_exits_nonzero() {
    // Clap rejects a non-integer for -n before any auth happens
    let out = lumitide().args(["search", "test", "-n", "notanumber"]).output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn download_command_no_longer_exists() {
    let out = lumitide().arg("download").output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn library_help_exits_zero() {
    let out = lumitide().args(["library", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("library") || stdout.contains("Browse"));
}
