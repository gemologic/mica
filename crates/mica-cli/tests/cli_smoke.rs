use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use mica_index::generate::{ingest_packages, init_db, set_meta, NixPackage};

struct TempHome {
    path: PathBuf,
}

impl TempHome {
    fn new(name: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock error")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mica-cli-tests-{}-{}-{}",
            name,
            std::process::id(),
            timestamp
        ));
        fs::create_dir_all(&path).expect("failed to create temp HOME");
        Self { path }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("failed to locate workspace root")
}

fn mica_cmd_in(home: &TempHome, cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mica"));
    cmd.current_dir(cwd);
    cmd.env("HOME", &home.path);
    cmd
}

fn mica_cmd(home: &TempHome) -> Command {
    mica_cmd_in(home, &workspace_root())
}

fn shell_escape(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn command_available(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn run_pty_command(
    home: &TempHome,
    cwd: &Path,
    args: &[&str],
    timeout_secs: u64,
    input: &[u8],
) -> std::process::Output {
    let mut command_parts = vec![shell_escape(env!("CARGO_BIN_EXE_mica"))];
    command_parts.extend(args.iter().map(|arg| shell_escape(arg)));
    let command_line = command_parts.join(" ");

    let mut child = Command::new("timeout")
        .args([
            "--signal=TERM",
            &format!("{}s", timeout_secs),
            "script",
            "-q",
            "-e",
            "-c",
            &command_line,
            "/dev/null",
        ])
        .current_dir(cwd)
        .env("HOME", &home.path)
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run PTY command");

    if !input.is_empty() {
        if let Some(mut stdin) = child.stdin.take() {
            for delay_ms in [300_u64, 500, 800] {
                thread::sleep(std::time::Duration::from_millis(delay_ms));
                if stdin.write_all(input).is_err() {
                    break;
                }
                let _ = stdin.flush();
            }
        }
    }

    child
        .wait_with_output()
        .expect("failed to wait on PTY command")
}

fn write_default_nix_fixture(project_dir: &Path) {
    let default_nix = r#"# Managed by Mica v0.1.0
# Do not edit sections between mica: markers
# Manual additions outside markers will be preserved

{ pkgs ? import (fetchTarball {
    # mica:pin:begin
    url = "https://github.com/jpetrucciani/nix/archive/deadbeef.tar.gz";
    sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123";
    # mica:pin:end
  }) {}
  # mica:pins:begin
  # mica:pins:end
}:

let
  name = "pty-test-env";

  # mica:let:begin
  # mica:let:end

  scripts = with pkgs; {
    # mica:scripts:begin
    # mica:scripts:end
  };

  # mica:packages:begin
  tools = with pkgs; [
    # mica:packages-raw:begin
    # mica:packages-raw:end
  ] ++ (pkgs.lib.attrsets.attrValues scripts);
  # mica:packages:end

  paths = pkgs.lib.flatten [ tools ];
  env = pkgs.buildEnv {
    inherit name paths; buildInputs = paths;
    # mica:env:begin
    # mica:env-raw:begin
    # mica:env-raw:end
    # mica:env:end

    # mica:shellhook:begin
    shellHook = ''
    '';
    # mica:shellhook:end
  };
in
env.overrideAttrs (prev: {
  # mica:override:begin
  # mica:override:end
}
  # mica:override-merge:begin
  # mica:override-merge:end
  // { inherit scripts; }
)
"#;
    fs::write(project_dir.join("default.nix"), default_nix).expect("failed to write default.nix");
}

fn write_index_fixture(home: &TempHome) {
    let cache_dir = home.path.join(".config").join("mica").join("cache");
    fs::create_dir_all(&cache_dir).expect("failed to create cache dir");
    let index_path = cache_dir.join("index.db");

    let mut conn = init_db(&index_path).expect("failed to initialize index db");
    let packages = vec![NixPackage {
        attr_path: "ripgrep".to_string(),
        name: "ripgrep".to_string(),
        version: Some("14.1.0".to_string()),
        description: Some("fixture package".to_string()),
        homepage: None,
        license: None,
        platforms: None,
        main_program: Some("rg".to_string()),
        position: Some("pkgs/tools/text/ripgrep/default.nix".to_string()),
        broken: Some(false),
        insecure: Some(false),
    }];
    ingest_packages(&mut conn, &packages).expect("failed to ingest fixture package");
    set_meta(&conn, "index_meta", "true").expect("failed to set index_meta");
    set_meta(&conn, "package_count", "1").expect("failed to set package_count");
}

#[test]
fn help_uses_optional_command_usage_and_lists_presets() {
    let home = TempHome::new("help");
    let output = mica_cmd(&home)
        .arg("--help")
        .output()
        .expect("failed to run mica --help");

    assert!(
        output.status.success(),
        "mica --help should exit successfully"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage: mica [OPTIONS] [COMMAND]"),
        "usage should advertise optional subcommand, got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("presets"),
        "help should list the presets command, got:\n{}",
        stdout
    );
}

#[test]
fn presets_command_prints_available_presets() {
    let home = TempHome::new("presets");
    let output = mica_cmd(&home)
        .arg("presets")
        .output()
        .expect("failed to run mica presets");

    assert!(
        output.status.success(),
        "mica presets should exit successfully"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|line| line.contains("[order:")),
        "expected formatted preset rows, got:\n{}",
        stdout
    );
}

#[test]
fn default_invocation_launches_tui() {
    if !command_available("script") || !command_available("timeout") {
        eprintln!("skipping PTY test, required system commands are unavailable");
        return;
    }

    let home = TempHome::new("tui-default");
    let project_dir = home.path.join("project");
    fs::create_dir_all(&project_dir).expect("failed to create project directory");
    write_default_nix_fixture(&project_dir);
    write_index_fixture(&home);

    let output = run_pty_command(&home, &project_dir, &[], 2, b"");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("\u{1b}[?1049h"),
        "default invocation should enter alternate screen in PTY (TUI startup).\nstdout:\n{}\nstderr:\n{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}
