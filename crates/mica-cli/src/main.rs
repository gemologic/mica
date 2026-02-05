use chrono::Utc;
use clap::{Parser, Subcommand};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mica_core::config::Config;
use mica_core::nixgen::{generate_profile_nix, generate_project_nix};
use mica_core::nixparse::{
    parse_nix_file, parse_profile_nix, parse_profile_state_from_nix, parse_project_state_from_nix,
};
use mica_core::preset::{
    load_embedded_presets, load_presets_from_dir, merge_presets, merge_profile_presets, Preset,
};
use mica_core::state::{
    GlobalProfileState, MicaMetadata, NixBlocks, Pin, PinnedPackage, PresetState, ProjectState,
    ShellState,
};
use mica_index::generate::{
    get_meta, ingest_packages, init_db, list_packages, load_packages_from_json, open_db,
    search_packages, set_meta,
};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

mod tui;

#[derive(Debug, Parser)]
#[command(name = "mica", version, about = "A TUI for managing Nix environments")]
struct Cli {
    #[arg(short = 'g', long = "global", help = "Operate on global profile")]
    global: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = "Launch TUI")]
    Tui,
    #[command(about = "Initialize state file")]
    Init {
        #[arg(
            long,
            help = "GitHub repo URL for nixpkgs (defaults to config or MICA_NIXPKGS_REPO)"
        )]
        repo: Option<String>,
    },
    #[command(about = "List current state")]
    List,
    #[command(about = "Add packages to environment")]
    Add { packages: Vec<String> },
    #[command(about = "Remove packages from environment")]
    Remove { packages: Vec<String> },
    #[command(about = "Search packages (index required)")]
    Search { query: String },
    #[command(about = "Manage environment variables")]
    Env {
        #[command(subcommand)]
        command: EnvCommand,
    },
    #[command(about = "Manage shell hook")]
    Shell {
        #[command(subcommand)]
        command: ShellCommand,
    },
    #[command(about = "Apply presets")]
    Apply { presets: Vec<String> },
    #[command(about = "Remove presets")]
    Unapply { presets: Vec<String> },
    #[command(about = "Update nixpkgs pin (stub)")]
    Update {
        #[arg(help = "Optional package name for version pinning (stub)")]
        package: Option<String>,
        #[arg(long, help = "Set nixpkgs URL for the pin")]
        url: Option<String>,
        #[arg(
            long,
            help = "Fetch latest commit hash for the pin URL from GitHub",
            conflicts_with = "rev"
        )]
        latest: bool,
        #[arg(long, help = "Set nixpkgs revision for the pin")]
        rev: Option<String>,
        #[arg(
            long,
            help = "Set nixpkgs sha256 for the pin (auto-computed when rev/latest is set)"
        )]
        sha256: Option<String>,
        #[arg(long, help = "Set nixpkgs branch for the pin")]
        branch: Option<String>,
    },
    #[command(about = "Manage extra pins")]
    Pin {
        #[command(subcommand)]
        command: PinCommand,
    },
    #[command(about = "Manage package index (stub)")]
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    #[command(about = "Regenerate nix file from state")]
    Sync {
        #[arg(long, help = "Update state from existing nix file (limited parsing)")]
        from_nix: bool,
    },
    #[command(about = "Check for drift between state and nix file")]
    Diff,
}

#[derive(Debug, Subcommand)]
enum EnvCommand {
    #[command(about = "Set an environment variable")]
    Set { key: String, value: String },
    #[command(about = "Unset an environment variable")]
    Unset { key: String },
}

#[derive(Debug, Subcommand)]
enum ShellCommand {
    #[command(about = "Set shell hook content (overwrites)")]
    Set { content: String },
    #[command(about = "Clear shell hook")]
    Clear,
}

#[derive(Debug, Subcommand)]
enum PinCommand {
    #[command(about = "Add an extra pin")]
    Add {
        #[arg(help = "Pin name (used as attribute)")]
        name: String,
        #[arg(long, help = "GitHub repo URL for the pin")]
        url: String,
        #[arg(long, help = "Git branch to resolve (defaults to base pin branch)")]
        branch: Option<String>,
        #[arg(long, help = "Set fetchTarball name")]
        tarball_name: Option<String>,
        #[arg(long, help = "Fetch latest commit hash for the pin URL from GitHub")]
        latest: bool,
        #[arg(long, help = "Set nixpkgs revision for the pin")]
        rev: Option<String>,
        #[arg(
            long,
            help = "Set nixpkgs sha256 for the pin (auto-computed when rev/latest is set)"
        )]
        sha256: Option<String>,
    },
    #[command(about = "Remove an extra pin")]
    Remove { name: String },
    #[command(about = "List extra pins")]
    List,
}

#[derive(Debug, Subcommand)]
enum IndexCommand {
    #[command(about = "Show index status (stub)")]
    Status,
    #[command(about = "Rebuild local index from nix-env json")]
    Rebuild {
        #[arg(help = "Path to nix-env -qaP --json output")]
        input: PathBuf,
        #[arg(long, help = "Output path for the index db")]
        output: Option<PathBuf>,
    },
    #[command(about = "Fetch remote index (stub)")]
    Fetch,
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("missing default.nix at {0}")]
    MissingDefaultNix(PathBuf),
    #[error("missing state file at {0}")]
    MissingState(PathBuf),
    #[error("state file already exists at {0}")]
    StateExists(PathBuf),
    #[error("pin is incomplete in state file, update pin before syncing")]
    IncompletePin,
    #[error("missing home directory in environment")]
    MissingHome,
    #[error("state error: {0}")]
    State(#[from] mica_core::state::StateError),
    #[error("preset error: {0}")]
    Preset(#[from] mica_core::preset::PresetError),
    #[error("config error: {0}")]
    Config(#[from] mica_core::config::ConfigError),
    #[error("missing preset: {0}")]
    MissingPreset(String),
    #[error("failed to write nix file: {0}")]
    WriteNix(std::io::Error),
    #[error("failed to read nix file: {0}")]
    ReadNix(std::io::Error),
    #[error("nix parse error: {0}")]
    NixParse(mica_core::nixparse::ParseError),
    #[error("nix state parse error: {0}")]
    NixStateParse(mica_core::nixparse::StateParseError),
    #[error("index error: {0}")]
    Index(#[from] mica_index::generate::IndexError),
    #[error("missing index at {0}")]
    MissingIndex(PathBuf),
    #[error("missing remote index url in config")]
    MissingRemoteIndex,
    #[error("invalid pin name: {0}")]
    InvalidPinName(String),
    #[error("pin already exists: {0}")]
    PinExists(String),
    #[error("pin not found: {0}")]
    PinNotFound(String),
    #[error("invalid github repo url: {0}")]
    InvalidGitHubUrl(String),
    #[error("github api request failed ({0}): {1}")]
    GitHubApiStatus(reqwest::StatusCode, String),
    #[error("github api response missing sha")]
    GitHubApiMissingSha,
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("nix-prefetch-url not found in PATH, install Nix or pass --sha256")]
    MissingNixPrefetch,
    #[error("failed to run nix-prefetch-url: {0}")]
    NixPrefetchIo(std::io::Error),
    #[error("nix-prefetch-url failed: {0}")]
    NixPrefetchFailed(String),
    #[error("nix-prefetch-url did not return a nix sha256 hash")]
    NixPrefetchMissingHash,
    #[error("nix-env not found in PATH, install Nix to auto-build the index")]
    MissingNixEnv,
    #[error("failed to run nix-env: {0}")]
    NixEnvIo(std::io::Error),
    #[error("nix-env failed: {0}")]
    NixEnvFailed(String),
}

#[derive(Debug, Deserialize)]
struct GitHubCommit {
    sha: String,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{}", err);
        std::process::exit(1);
    }
}

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    match cli.command {
        Command::Tui => run_tui(cli.global),
        Command::Init { repo } => {
            if cli.global {
                init_profile_state(repo)?;
                let state = load_profile_state()?;
                sync_and_install_profile(&state)?;
            } else {
                init_project_state(repo)?;
            }
            Ok(())
        }
        Command::Add { packages } => {
            if cli.global {
                let mut state = load_profile_state()?;
                for pkg in packages {
                    if !state.packages.added.contains(&pkg) {
                        state.packages.added.push(pkg.clone());
                    }
                    state.packages.removed.retain(|item| item != &pkg);
                }
                update_profile_modified(&mut state);
                save_profile_state(&state)?;
                sync_and_install_profile(&state)?;
            } else {
                let mut state = load_project_state()?;
                for pkg in packages {
                    if !state.packages.added.contains(&pkg) {
                        state.packages.added.push(pkg.clone());
                    }
                    state.packages.removed.retain(|item| item != &pkg);
                }
                update_project_modified(&mut state);
                sync_project_nix(&state)?;
            }
            Ok(())
        }
        Command::Remove { packages } => {
            if cli.global {
                let mut state = load_profile_state()?;
                for pkg in packages {
                    if !state.packages.removed.contains(&pkg) {
                        state.packages.removed.push(pkg.clone());
                    }
                    state.packages.added.retain(|item| item != &pkg);
                }
                update_profile_modified(&mut state);
                save_profile_state(&state)?;
                sync_and_install_profile(&state)?;
            } else {
                let mut state = load_project_state()?;
                for pkg in packages {
                    if !state.packages.removed.contains(&pkg) {
                        state.packages.removed.push(pkg.clone());
                    }
                    state.packages.added.retain(|item| item != &pkg);
                }
                update_project_modified(&mut state);
                sync_project_nix(&state)?;
            }
            Ok(())
        }
        Command::Search { query } => {
            let index_path = index_db_path()?;
            if !index_path.exists() {
                return Err(CliError::MissingIndex(index_path));
            }
            let conn = open_db(&index_path)?;
            let results = search_packages(&conn, &query, 25)?;
            for pkg in results {
                let version = pkg.version.unwrap_or_else(|| "-".to_string());
                let description = pkg.description.unwrap_or_default();
                println!(
                    "{} {} {}",
                    normalize_attr_path(&pkg.attr_path),
                    version,
                    description
                );
            }
            Ok(())
        }
        Command::Env { command } => {
            if cli.global {
                println!("env is only supported in project mode for now");
            } else {
                let mut state = load_project_state()?;
                match command {
                    EnvCommand::Set { key, value } => {
                        state.env.insert(key, value);
                    }
                    EnvCommand::Unset { key } => {
                        state.env.remove(&key);
                    }
                }
                update_project_modified(&mut state);
                sync_project_nix(&state)?;
            }
            Ok(())
        }
        Command::Shell { command } => {
            if cli.global {
                println!("shell hook is only supported in project mode for now");
            } else {
                let mut state = load_project_state()?;
                match command {
                    ShellCommand::Set { content } => {
                        state.shell.hook = Some(content);
                    }
                    ShellCommand::Clear => {
                        state.shell.hook = None;
                    }
                }
                update_project_modified(&mut state);
                sync_project_nix(&state)?;
            }
            Ok(())
        }
        Command::Apply { presets } => {
            if cli.global {
                let mut state = load_profile_state()?;
                for preset in presets {
                    if !state.presets.active.contains(&preset) {
                        state.presets.active.push(preset);
                    }
                }
                update_profile_modified(&mut state);
                save_profile_state(&state)?;
                sync_and_install_profile(&state)?;
            } else {
                let mut state = load_project_state()?;
                for preset in presets {
                    if !state.presets.active.contains(&preset) {
                        state.presets.active.push(preset);
                    }
                }
                update_project_modified(&mut state);
                sync_project_nix(&state)?;
            }
            Ok(())
        }
        Command::Unapply { presets } => {
            if cli.global {
                let mut state = load_profile_state()?;
                state
                    .presets
                    .active
                    .retain(|preset| !presets.contains(preset));
                update_profile_modified(&mut state);
                save_profile_state(&state)?;
                sync_and_install_profile(&state)?;
            } else {
                let mut state = load_project_state()?;
                state
                    .presets
                    .active
                    .retain(|preset| !presets.contains(preset));
                update_project_modified(&mut state);
                sync_project_nix(&state)?;
            }
            Ok(())
        }
        Command::List => {
            if cli.global {
                let state = load_profile_state()?;
                print_profile_state(&state);
            } else {
                let state = load_project_state()?;
                print_project_state(&state);
            }
            Ok(())
        }
        Command::Update {
            package,
            url,
            latest,
            rev,
            sha256,
            branch,
        } => {
            if cli.global {
                let mut state = load_profile_state()?;
                let base_pin = match package.as_deref() {
                    Some(name) => state
                        .packages
                        .pinned
                        .get(name)
                        .map(|pinned| &pinned.pin)
                        .unwrap_or(&state.pin),
                    None => &state.pin,
                };
                let (resolved_rev, resolved_sha256) =
                    resolve_update_rev_and_sha(base_pin, &url, &branch, rev, sha256, latest)?;
                update_profile_pin_stub(
                    &mut state,
                    package,
                    url,
                    resolved_rev,
                    resolved_sha256,
                    branch,
                )?;
                save_profile_state(&state)?;
                sync_and_install_profile(&state)?;
            } else {
                let mut state = load_project_state()?;
                let base_pin = match package.as_deref() {
                    Some(name) => state
                        .packages
                        .pinned
                        .get(name)
                        .map(|pinned| &pinned.pin)
                        .unwrap_or(&state.pin),
                    None => &state.pin,
                };
                let (resolved_rev, resolved_sha256) =
                    resolve_update_rev_and_sha(base_pin, &url, &branch, rev, sha256, latest)?;
                update_project_pin_stub(
                    &mut state,
                    package,
                    url,
                    resolved_rev,
                    resolved_sha256,
                    branch,
                )?;
                save_project_state(&state)?;
            }
            Ok(())
        }
        Command::Pin { command } => {
            if cli.global {
                println!("pins are only supported in project mode for now");
            } else {
                let mut state = load_project_state()?;
                match command {
                    PinCommand::Add {
                        name,
                        url,
                        branch,
                        tarball_name,
                        latest,
                        rev,
                        sha256,
                    } => {
                        add_extra_pin(
                            &mut state,
                            AddPinRequest {
                                name,
                                url,
                                branch,
                                tarball_name,
                                rev,
                                sha256,
                                latest,
                            },
                        )?;
                        save_project_state(&state)?;
                    }
                    PinCommand::Remove { name } => {
                        if state.pins.remove(&name).is_none() {
                            return Err(CliError::PinNotFound(name));
                        }
                        update_project_modified(&mut state);
                        save_project_state(&state)?;
                    }
                    PinCommand::List => {
                        if state.pins.is_empty() {
                            println!("no extra pins configured");
                        } else {
                            for (name, pin) in &state.pins {
                                println!("{} -> {} @ {}", name, pin.url, pin.rev);
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Index { command } => {
            match command {
                IndexCommand::Status => {
                    let index_path = index_db_path()?;
                    if !index_path.exists() {
                        return Err(CliError::MissingIndex(index_path));
                    }
                    let conn = open_db(&index_path)?;
                    let meta = get_meta(&conn)?;
                    if meta.is_empty() {
                        println!("index: {}", index_path.display());
                        println!("meta: empty");
                    } else {
                        println!("index: {}", index_path.display());
                        for (key, value) in meta {
                            println!("{}: {}", key, value);
                        }
                    }
                }
                IndexCommand::Rebuild { input, output } => {
                    let output_path = output.unwrap_or(index_db_path()?);
                    let pin = if cli.global {
                        load_profile_state().ok().map(|state| state.pin)
                    } else {
                        load_project_state().ok().map(|state| state.pin)
                    };
                    let count = rebuild_index_from_json(&input, &output_path, pin.as_ref())?;
                    println!("indexed {} packages", count);
                }
                IndexCommand::Fetch => {
                    let config = load_config_or_default()?;
                    if config.index.remote_url.trim().is_empty() {
                        return Err(CliError::MissingRemoteIndex);
                    }
                    println!(
                        "index fetch not implemented, remote_url={}",
                        config.index.remote_url
                    );
                }
            }
            Ok(())
        }
        Command::Sync { from_nix } => {
            if cli.global {
                if from_nix {
                    let mut state = load_profile_state()?;
                    update_profile_state_from_nix(&mut state)?;
                    save_profile_state(&state)?;
                }
                let state = load_profile_state()?;
                sync_and_install_profile(&state)?;
            } else {
                if from_nix {
                    let mut state = load_project_state()?;
                    update_project_state_from_nix(&mut state)?;
                }
                let state = load_project_state()?;
                sync_project_nix(&state)?;
            }
            Ok(())
        }
        Command::Diff => {
            if cli.global {
                let state = load_profile_state()?;
                diff_profile(&state)?;
            } else {
                let state = load_project_state()?;
                diff_project(&state)?;
            }
            Ok(())
        }
    }
}

fn run_tui(global: bool) -> Result<(), CliError> {
    if global {
        run_tui_global()
    } else {
        run_tui_project()
    }
}

fn run_tui_project() -> Result<(), CliError> {
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;
    use tui::app::App;

    let project_path = project_nix_path();
    if !project_path.exists() {
        eprintln!(
            "default.nix missing at {}, initializing",
            project_path.display()
        );
        init_project_state(None)?;
    }
    let mut state = load_project_state()?;
    let index_path = index_db_path()?;
    if !index_path.exists() {
        eprintln!(
            "index missing at {}, building from nix-env -qaP --json",
            index_path.display()
        );
        let pins = collect_index_pins(&state);
        let count = rebuild_index_from_pins_with_spinner(&index_path, &pins)?;
        eprintln!("index ready, {} packages", count);
    }

    let mut conn = open_db(&index_path)?;
    let mut meta = get_meta(&conn).unwrap_or_default();
    let mut has_meta = meta_has_key(&meta, "index_meta");
    if has_meta && !index_has_descriptions(&conn)? {
        has_meta = false;
    }
    if !has_meta {
        eprintln!("index missing metadata, rebuilding from nix-env -qaP --json --meta");
        let pins = collect_index_pins(&state);
        let count = rebuild_index_from_pins_with_spinner(&index_path, &pins)?;
        eprintln!("index ready, {} packages", count);
        conn = open_db(&index_path)?;
        meta = get_meta(&conn).unwrap_or_default();
    }
    let presets = load_tui_presets()?;
    let mut app = App::new(Vec::new(), presets);
    app.mode = tui::app::AppMode::Project;
    if let Ok(dir) = std::env::current_dir() {
        app.project_dir = Some(dir.to_string_lossy().to_string());
    }
    if let Ok(config) = load_config_or_default() {
        apply_columns_from_config(&mut app, &config);
    }
    app.index_info = index_info_from_meta(meta);
    apply_state_to_app(&mut app, &state);
    update_search_results(&conn, &mut app)?;
    app.refresh_preset_filter();

    enable_raw_mode().map_err(CliError::WriteNix)?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).map_err(CliError::WriteNix)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(CliError::WriteNix)?;

    let result = run_tui_loop_project(&mut terminal, &mut app, &mut state, &index_path, &mut conn);

    disable_raw_mode().map_err(CliError::WriteNix)?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(CliError::WriteNix)?;
    terminal.show_cursor().map_err(CliError::WriteNix)?;
    result
}

fn run_tui_global() -> Result<(), CliError> {
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;
    use tui::app::App;

    let profile_state = profile_state_path()?;
    if !profile_state.exists() {
        eprintln!(
            "global profile missing at {}, initializing",
            profile_state.display()
        );
        init_profile_state(None)?;
        let state = load_profile_state()?;
        sync_and_install_profile(&state)?;
    }
    let mut state = load_profile_state()?;
    let profile_nix = profile_nix_path()?;
    if !profile_nix.exists() {
        sync_profile_nix(&state)?;
    }

    let index_path = index_db_path()?;
    if !index_path.exists() {
        eprintln!(
            "index missing at {}, building from nix-env -qaP --json",
            index_path.display()
        );
        let pins = collect_index_pins_profile(&state);
        let count = rebuild_index_from_pins_with_spinner(&index_path, &pins)?;
        eprintln!("index ready, {} packages", count);
    }

    let mut conn = open_db(&index_path)?;
    let mut meta = get_meta(&conn).unwrap_or_default();
    let mut has_meta = meta_has_key(&meta, "index_meta");
    if has_meta && !index_has_descriptions(&conn)? {
        has_meta = false;
    }
    if !has_meta {
        eprintln!("index missing metadata, rebuilding from nix-env -qaP --json --meta");
        let pins = collect_index_pins_profile(&state);
        let count = rebuild_index_from_pins_with_spinner(&index_path, &pins)?;
        eprintln!("index ready, {} packages", count);
        conn = open_db(&index_path)?;
        meta = get_meta(&conn).unwrap_or_default();
    }

    let presets = load_tui_presets()?;
    let mut app = App::new(Vec::new(), presets);
    app.mode = tui::app::AppMode::Global;
    if let Ok(config) = load_config_or_default() {
        apply_columns_from_config(&mut app, &config);
    }
    app.index_info = index_info_from_meta(meta);
    apply_profile_state_to_app(&mut app, &state);
    update_search_results(&conn, &mut app)?;
    app.refresh_preset_filter();

    enable_raw_mode().map_err(CliError::WriteNix)?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).map_err(CliError::WriteNix)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(CliError::WriteNix)?;

    let result = run_tui_loop_global(&mut terminal, &mut app, &mut state, &index_path, &mut conn);

    disable_raw_mode().map_err(CliError::WriteNix)?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(CliError::WriteNix)?;
    terminal.show_cursor().map_err(CliError::WriteNix)?;
    result
}

fn run_tui_loop_project(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    state: &mut ProjectState,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
) -> Result<(), CliError> {
    use crossterm::event::{self, Event};

    loop {
        app.clear_expired_toast();
        terminal
            .draw(|frame| tui::ui::render(frame, app))
            .map_err(CliError::WriteNix)?;

        if event::poll(Duration::from_millis(200)).map_err(CliError::WriteNix)? {
            if let Event::Key(key) = event::read().map_err(CliError::WriteNix)? {
                if app.overlay.is_some() {
                    if let Err(err) =
                        handle_overlay_key(key, terminal, app, state, index_path, conn)
                    {
                        app.push_toast(tui::app::ToastLevel::Error, err.to_string());
                    }
                } else {
                    if let Err(err) = handle_main_key(key, terminal, app, state, index_path, conn) {
                        app.push_toast(tui::app::ToastLevel::Error, err.to_string());
                    }
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn run_tui_loop_global(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    state: &mut GlobalProfileState,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
) -> Result<(), CliError> {
    use crossterm::event::{self, Event};

    loop {
        app.clear_expired_toast();
        terminal
            .draw(|frame| tui::ui::render(frame, app))
            .map_err(CliError::WriteNix)?;

        if event::poll(Duration::from_millis(200)).map_err(CliError::WriteNix)? {
            if let Event::Key(key) = event::read().map_err(CliError::WriteNix)? {
                if app.overlay.is_some() {
                    if let Err(err) = handle_overlay_key_global(key, app, conn) {
                        app.push_toast(tui::app::ToastLevel::Error, err.to_string());
                    }
                } else if let Err(err) =
                    handle_main_key_global(key, terminal, app, state, index_path, conn)
                {
                    app.push_toast(tui::app::ToastLevel::Error, err.to_string());
                }
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn handle_main_key(
    key: KeyEvent,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    state: &mut ProjectState,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
) -> Result<(), CliError> {
    use tui::app::{FilterKind, Focus, Overlay};
    use tui::input::{map_key, InputAction};

    match map_key(key) {
        InputAction::Quit => app.should_quit = true,
        InputAction::Help => app.overlay = Some(Overlay::Help),
        InputAction::Toggle => app.toggle_current(),
        InputAction::ToggleFocus => app.toggle_focus(),
        InputAction::Next => app.next(),
        InputAction::Prev => app.prev(),
        InputAction::Save => {
            save_tui_selection(state, app)?;
            app.push_toast(tui::app::ToastLevel::Info, "Saved changes");
        }
        InputAction::OpenEnv => open_env_overlay(app),
        InputAction::OpenShell => open_shell_overlay(app),
        InputAction::ToggleBroken => {
            app.filters.show_broken = !app.filters.show_broken;
            update_search_results(conn, app)?;
        }
        InputAction::ToggleInsecure => {
            app.filters.show_insecure = !app.filters.show_insecure;
            update_search_results(conn, app)?;
        }
        InputAction::ToggleInstalled => {
            app.filters.show_installed_only = !app.filters.show_installed_only;
            update_search_results(conn, app)?;
        }
        InputAction::EditLicenseFilter => open_filter_overlay(app, FilterKind::License),
        InputAction::EditPlatformFilter => open_filter_overlay(app, FilterKind::Platform),
        InputAction::PreviewDiff => {
            app.overlay = Some(build_diff_overlay(state, app)?);
        }
        InputAction::ShowPackageInfo => {
            if app.focus != Focus::Packages {
                app.push_toast(tui::app::ToastLevel::Info, "Focus packages to view info");
            } else if let Some(overlay) = build_package_info_overlay(app, state) {
                app.overlay = Some(overlay);
            } else {
                app.push_toast(tui::app::ToastLevel::Info, "No package selected");
            }
        }
        InputAction::UpdatePin => {
            with_tui_suspended(terminal, || {
                let rev = run_with_spinner("fetching latest nixpkgs revision", || {
                    fetch_latest_github_rev(&state.pin.url, &state.pin.branch)
                })?;
                let sha256 = run_with_spinner("prefetching nixpkgs tarball", || {
                    fetch_nix_sha256(&state.pin.url, &rev)
                })?;
                state.pin.rev = rev;
                state.pin.sha256 = sha256;
                state.pin.updated = Utc::now().date_naive();
                update_project_modified(state);
                save_project_state(state)?;
                let pins = collect_index_pins(state);
                rebuild_index_from_pins_with_spinner(index_path, &pins)?;
                Ok(())
            })?;
            *conn = open_db(index_path)?;
            app.index_info = index_info_from_meta(get_meta(conn).unwrap_or_default());
            update_search_results(conn, app)?;
            app.push_toast(tui::app::ToastLevel::Info, "Pin updated");
        }
        InputAction::AddPin => {
            app.overlay = Some(tui::app::Overlay::PinEditor(tui::app::PinEditorState::new(
                state.pin.url.clone(),
                state.pin.branch.clone(),
            )));
        }
        InputAction::TogglePresets => {
            app.presets_collapsed = !app.presets_collapsed;
            if app.presets_collapsed {
                app.focus = Focus::Packages;
            }
        }
        InputAction::ToggleChanges => {
            app.changes_collapsed = !app.changes_collapsed;
        }
        InputAction::OpenColumns => {
            app.overlay = Some(tui::app::Overlay::Columns(tui::app::ColumnsEditorState {
                cursor: 0,
            }));
        }
        InputAction::RebuildIndex => {
            with_tui_suspended(terminal, || {
                let pins = collect_index_pins(state);
                rebuild_index_from_pins_with_spinner(index_path, &pins)?;
                Ok(())
            })?;
            *conn = open_db(index_path)?;
            app.index_info = index_info_from_meta(get_meta(conn).unwrap_or_default());
            update_search_results(conn, app)?;
            app.push_toast(tui::app::ToastLevel::Info, "Index rebuilt");
        }
        InputAction::Sync => {
            update_project_state_from_nix(state)?;
            apply_state_to_app(app, state);
            update_search_results(conn, app)?;
            app.refresh_preset_filter();
            app.push_toast(tui::app::ToastLevel::Info, "Reloaded from nix");
        }
        InputAction::Backspace => match app.focus {
            Focus::Packages => {
                app.query.pop();
                update_search_results(conn, app)?;
            }
            Focus::Presets => {
                app.preset_query.pop();
                app.refresh_preset_filter();
            }
            Focus::Changes => {}
        },
        InputAction::Clear => match app.focus {
            Focus::Packages => {
                app.query.clear();
                update_search_results(conn, app)?;
            }
            Focus::Presets => {
                app.preset_query.clear();
                app.refresh_preset_filter();
            }
            Focus::Changes => {}
        },
        InputAction::Insert(ch) => match app.focus {
            Focus::Packages => {
                app.query.push(ch);
                update_search_results(conn, app)?;
            }
            Focus::Presets => {
                app.preset_query.push(ch);
                app.refresh_preset_filter();
            }
            Focus::Changes => {}
        },
        InputAction::None => {}
    }

    Ok(())
}

fn handle_main_key_global(
    key: KeyEvent,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    state: &mut GlobalProfileState,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
) -> Result<(), CliError> {
    use tui::app::{FilterKind, Focus, Overlay};
    use tui::input::{map_key, InputAction};

    match map_key(key) {
        InputAction::Quit => app.should_quit = true,
        InputAction::Help => app.overlay = Some(Overlay::Help),
        InputAction::Toggle => app.toggle_current(),
        InputAction::ToggleFocus => app.toggle_focus(),
        InputAction::Next => app.next(),
        InputAction::Prev => app.prev(),
        InputAction::Save => {
            with_tui_suspended(terminal, || save_profile_tui_selection(state, app))?;
            app.push_toast(tui::app::ToastLevel::Info, "Saved and installed");
        }
        InputAction::OpenEnv => {
            app.push_toast(tui::app::ToastLevel::Info, "Env is project-only");
        }
        InputAction::OpenShell => {
            app.push_toast(tui::app::ToastLevel::Info, "Shell hook is project-only");
        }
        InputAction::ToggleBroken => {
            app.filters.show_broken = !app.filters.show_broken;
            update_search_results(conn, app)?;
        }
        InputAction::ToggleInsecure => {
            app.filters.show_insecure = !app.filters.show_insecure;
            update_search_results(conn, app)?;
        }
        InputAction::ToggleInstalled => {
            app.filters.show_installed_only = !app.filters.show_installed_only;
            update_search_results(conn, app)?;
        }
        InputAction::EditLicenseFilter => open_filter_overlay(app, FilterKind::License),
        InputAction::EditPlatformFilter => open_filter_overlay(app, FilterKind::Platform),
        InputAction::PreviewDiff => {
            app.overlay = Some(build_diff_overlay_profile(state, app)?);
        }
        InputAction::ShowPackageInfo => {
            if app.focus != Focus::Packages {
                app.push_toast(tui::app::ToastLevel::Info, "Focus packages to view info");
            } else {
                let pins = collect_index_pins_profile(state);
                if let Some(overlay) = build_package_info_overlay_with_pins(app, &pins) {
                    app.overlay = Some(overlay);
                } else {
                    app.push_toast(tui::app::ToastLevel::Info, "No package selected");
                }
            }
        }
        InputAction::UpdatePin => {
            with_tui_suspended(terminal, || {
                let rev = run_with_spinner("fetching latest nixpkgs revision", || {
                    fetch_latest_github_rev(&state.pin.url, &state.pin.branch)
                })?;
                let sha256 = run_with_spinner("prefetching nixpkgs tarball", || {
                    fetch_nix_sha256(&state.pin.url, &rev)
                })?;
                state.pin.rev = rev;
                state.pin.sha256 = sha256;
                state.pin.updated = Utc::now().date_naive();
                update_profile_modified(state);
                save_profile_state(state)?;
                sync_and_install_profile(state)?;
                let pins = collect_index_pins_profile(state);
                rebuild_index_from_pins_with_spinner(index_path, &pins)?;
                Ok(())
            })?;
            *conn = open_db(index_path)?;
            app.index_info = index_info_from_meta(get_meta(conn).unwrap_or_default());
            update_search_results(conn, app)?;
            app.push_toast(tui::app::ToastLevel::Info, "Pin updated");
        }
        InputAction::AddPin => {
            app.push_toast(tui::app::ToastLevel::Info, "Extra pins are project-only");
        }
        InputAction::TogglePresets => {
            app.presets_collapsed = !app.presets_collapsed;
            if app.presets_collapsed {
                app.focus = Focus::Packages;
            }
        }
        InputAction::ToggleChanges => {
            app.changes_collapsed = !app.changes_collapsed;
        }
        InputAction::OpenColumns => {
            app.overlay = Some(tui::app::Overlay::Columns(tui::app::ColumnsEditorState {
                cursor: 0,
            }));
        }
        InputAction::RebuildIndex => {
            with_tui_suspended(terminal, || {
                let pins = collect_index_pins_profile(state);
                rebuild_index_from_pins_with_spinner(index_path, &pins)?;
                Ok(())
            })?;
            *conn = open_db(index_path)?;
            app.index_info = index_info_from_meta(get_meta(conn).unwrap_or_default());
            update_search_results(conn, app)?;
            app.push_toast(tui::app::ToastLevel::Info, "Index rebuilt");
        }
        InputAction::Sync => {
            update_profile_state_from_nix(state)?;
            apply_profile_state_to_app(app, state);
            update_search_results(conn, app)?;
            app.refresh_preset_filter();
            app.push_toast(tui::app::ToastLevel::Info, "Reloaded from nix");
        }
        InputAction::Backspace => match app.focus {
            Focus::Packages => {
                app.query.pop();
                update_search_results(conn, app)?;
            }
            Focus::Presets => {
                app.preset_query.pop();
                app.refresh_preset_filter();
            }
            Focus::Changes => {}
        },
        InputAction::Clear => match app.focus {
            Focus::Packages => {
                app.query.clear();
                update_search_results(conn, app)?;
            }
            Focus::Presets => {
                app.preset_query.clear();
                app.refresh_preset_filter();
            }
            Focus::Changes => {}
        },
        InputAction::Insert(ch) => match app.focus {
            Focus::Packages => {
                app.query.push(ch);
                update_search_results(conn, app)?;
            }
            Focus::Presets => {
                app.preset_query.push(ch);
                app.refresh_preset_filter();
            }
            Focus::Changes => {}
        },
        InputAction::None => {}
    }

    Ok(())
}

fn handle_overlay_key(
    key: KeyEvent,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    state: &mut ProjectState,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
) -> Result<(), CliError> {
    use tui::app::{EnvEditMode, Overlay};

    let overlay = match app.overlay.take() {
        Some(overlay) => overlay,
        None => return Ok(()),
    };

    match overlay {
        Overlay::Help => {
            let close = matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
            );
            if !close {
                app.overlay = Some(Overlay::Help);
            }
        }
        Overlay::PackageInfo(mut state) => {
            let mut close = false;
            let max_scroll = state.lines.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => close = true,
                KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
                KeyCode::Down => state.scroll = (state.scroll + 1).min(max_scroll),
                KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
                KeyCode::PageDown => state.scroll = (state.scroll + 10).min(max_scroll),
                KeyCode::Home => state.scroll = 0,
                KeyCode::End => state.scroll = max_scroll,
                KeyCode::Char('i') if key.modifiers.contains(KeyModifiers::CONTROL) => close = true,
                _ => {}
            }
            if !close {
                app.overlay = Some(Overlay::PackageInfo(state));
            }
        }
        Overlay::PinEditor(mut editor) => {
            let mut close = false;
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Tab | KeyCode::Down => {
                    pin_editor_next_field(&mut editor);
                    editor.error = None;
                }
                KeyCode::BackTab | KeyCode::Up => {
                    pin_editor_prev_field(&mut editor);
                    editor.error = None;
                }
                KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    editor.use_latest = !editor.use_latest;
                    editor.error = None;
                }
                KeyCode::Enter => {
                    if submit_pin_editor(terminal, app, &mut editor, state, index_path, conn) {
                        close = true;
                    } else {
                        app.overlay = Some(Overlay::PinEditor(editor));
                        return Ok(());
                    }
                }
                KeyCode::Backspace => {
                    let (value, cursor) = pin_editor_active_input(&mut editor);
                    if *cursor > 0 {
                        *cursor -= 1;
                        value.remove(*cursor);
                    }
                    editor.error = None;
                }
                KeyCode::Left => {
                    let (_, cursor) = pin_editor_active_input(&mut editor);
                    if *cursor > 0 {
                        *cursor -= 1;
                    }
                }
                KeyCode::Right => {
                    let (value, cursor) = pin_editor_active_input(&mut editor);
                    if *cursor < value.len() {
                        *cursor += 1;
                    }
                }
                KeyCode::Home => {
                    let (_, cursor) = pin_editor_active_input(&mut editor);
                    *cursor = 0;
                }
                KeyCode::End => {
                    let (value, cursor) = pin_editor_active_input(&mut editor);
                    *cursor = value.len();
                }
                KeyCode::Char(ch)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    let (value, cursor) = pin_editor_active_input(&mut editor);
                    value.insert(*cursor, ch);
                    *cursor += 1;
                    editor.error = None;
                }
                _ => {}
            }
            if !close {
                app.overlay = Some(Overlay::PinEditor(editor));
            }
        }
        Overlay::Columns(mut state) => {
            let mut close = false;
            let max = tui::app::COLUMN_OPTIONS.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Up => {
                    if state.cursor > 0 {
                        state.cursor -= 1;
                    }
                }
                KeyCode::Down => {
                    state.cursor = (state.cursor + 1).min(max);
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(option) = tui::app::COLUMN_OPTIONS.get(state.cursor) {
                        toggle_column_setting(app, option.kind);
                    }
                }
                _ => {}
            }
            if !close {
                app.overlay = Some(Overlay::Columns(state));
            }
        }
        Overlay::Filter(mut state) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                let value = state.input.trim().to_string();
                match state.kind {
                    tui::app::FilterKind::License => app.filters.license = value,
                    tui::app::FilterKind::Platform => app.filters.platform = value,
                }
                update_search_results(conn, app)?;
            }
            KeyCode::Backspace => {
                if state.cursor > 0 {
                    state.cursor -= 1;
                    state.input.remove(state.cursor);
                }
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::Left => {
                if state.cursor > 0 {
                    state.cursor -= 1;
                }
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::Right => {
                if state.cursor < state.input.len() {
                    state.cursor += 1;
                }
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::Home => {
                state.cursor = 0;
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::End => {
                state.cursor = state.input.len();
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                state.input.insert(state.cursor, ch);
                state.cursor += 1;
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            _ => {
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
        },
        Overlay::Env(mut state) => {
            let mut close = false;
            match state.mode {
                EnvEditMode::List => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => close = true,
                    KeyCode::Up => {
                        if state.cursor > 0 {
                            state.cursor -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if !state.entries.is_empty() {
                            state.cursor = (state.cursor + 1).min(state.entries.len() - 1);
                        }
                    }
                    KeyCode::Char('a') => {
                        state.mode = EnvEditMode::Edit { original_key: None };
                        state.input.clear();
                        state.input_cursor = 0;
                        state.error = None;
                    }
                    KeyCode::Enter => {
                        if let Some((key, value)) = state.entries.get(state.cursor) {
                            state.input = format!("{}={}", key, value);
                            state.input_cursor = state.input.len();
                            state.mode = EnvEditMode::Edit {
                                original_key: Some(key.clone()),
                            };
                            state.error = None;
                        }
                    }
                    KeyCode::Char('d') => {
                        if !state.entries.is_empty() {
                            state.entries.remove(state.cursor);
                            if state.cursor >= state.entries.len() && state.cursor > 0 {
                                state.cursor -= 1;
                            }
                        }
                    }
                    _ => {}
                },
                EnvEditMode::Edit { .. } => match key.code {
                    KeyCode::Esc => {
                        state.mode = EnvEditMode::List;
                        state.input.clear();
                        state.input_cursor = 0;
                        state.error = None;
                    }
                    KeyCode::Enter => match apply_env_input(&mut state) {
                        Ok(()) => {}
                        Err(err) => state.error = Some(err),
                    },
                    KeyCode::Backspace => {
                        if state.input_cursor > 0 {
                            state.input_cursor -= 1;
                            state.input.remove(state.input_cursor);
                        }
                    }
                    KeyCode::Left => {
                        if state.input_cursor > 0 {
                            state.input_cursor -= 1;
                        }
                    }
                    KeyCode::Right => {
                        if state.input_cursor < state.input.len() {
                            state.input_cursor += 1;
                        }
                    }
                    KeyCode::Home => state.input_cursor = 0,
                    KeyCode::End => state.input_cursor = state.input.len(),
                    KeyCode::Char(ch)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        state.input.insert(state.input_cursor, ch);
                        state.input_cursor += 1;
                    }
                    _ => {}
                },
            }

            if close {
                apply_env_overlay(app, state);
            } else {
                app.overlay = Some(Overlay::Env(state));
            }
        }
        Overlay::Shell(mut state) => {
            let mut close = false;
            let mut cancel = false;
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    close = true;
                    cancel = true;
                }
                KeyCode::Up => {
                    if state.cursor_row > 0 {
                        state.cursor_row -= 1;
                        let line_len = state.lines[state.cursor_row].len();
                        state.cursor_col = state.cursor_col.min(line_len);
                    }
                }
                KeyCode::Down => {
                    if state.cursor_row + 1 < state.lines.len() {
                        state.cursor_row += 1;
                        let line_len = state.lines[state.cursor_row].len();
                        state.cursor_col = state.cursor_col.min(line_len);
                    }
                }
                KeyCode::Left => {
                    if state.cursor_col > 0 {
                        state.cursor_col -= 1;
                    } else if state.cursor_row > 0 {
                        state.cursor_row -= 1;
                        state.cursor_col = state.lines[state.cursor_row].len();
                    }
                }
                KeyCode::Right => {
                    let line_len = state.lines[state.cursor_row].len();
                    if state.cursor_col < line_len {
                        state.cursor_col += 1;
                    } else if state.cursor_row + 1 < state.lines.len() {
                        state.cursor_row += 1;
                        state.cursor_col = 0;
                    }
                }
                KeyCode::Enter => {
                    ensure_shell_lines(&mut state);
                    let current = state.lines.get_mut(state.cursor_row).unwrap();
                    let remainder = current.split_off(state.cursor_col);
                    state.cursor_row += 1;
                    state.cursor_col = 0;
                    state.lines.insert(state.cursor_row, remainder);
                }
                KeyCode::Backspace => {
                    ensure_shell_lines(&mut state);
                    if state.cursor_col > 0 {
                        let current = state.lines.get_mut(state.cursor_row).unwrap();
                        current.remove(state.cursor_col - 1);
                        state.cursor_col -= 1;
                    } else if state.cursor_row > 0 {
                        let current = state.lines.remove(state.cursor_row);
                        state.cursor_row -= 1;
                        let prev = state.lines.get_mut(state.cursor_row).unwrap();
                        let prev_len = prev.len();
                        prev.push_str(&current);
                        state.cursor_col = prev_len;
                    }
                }
                KeyCode::Char(ch)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    ensure_shell_lines(&mut state);
                    let current = state.lines.get_mut(state.cursor_row).unwrap();
                    current.insert(state.cursor_col, ch);
                    state.cursor_col += 1;
                }
                _ => {}
            }

            if close {
                if cancel {
                    apply_shell_overlay(app, &state.original);
                } else {
                    apply_shell_overlay(app, &state.lines);
                }
            } else {
                app.overlay = Some(Overlay::Shell(state));
            }
        }
        Overlay::Diff(mut state) => {
            let current_lines = if state.show_full {
                &state.full_lines
            } else {
                &state.change_lines
            };
            let max_scroll = current_lines.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
                KeyCode::Down => state.scroll = (state.scroll + 1).min(max_scroll),
                KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
                KeyCode::PageDown => state.scroll = (state.scroll + 10).min(max_scroll),
                KeyCode::Home => state.scroll = 0,
                KeyCode::End => state.scroll = max_scroll,
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    state.show_full = !state.show_full;
                    let new_max = if state.show_full {
                        state.full_lines.len().saturating_sub(1)
                    } else {
                        state.change_lines.len().saturating_sub(1)
                    };
                    if state.scroll > new_max {
                        state.scroll = new_max;
                    }
                }
                _ => {}
            }
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                return Ok(());
            }
            app.overlay = Some(Overlay::Diff(state));
        }
    }

    Ok(())
}

fn handle_overlay_key_global(
    key: KeyEvent,
    app: &mut tui::app::App,
    conn: &rusqlite::Connection,
) -> Result<(), CliError> {
    use tui::app::Overlay;

    let overlay = match app.overlay.take() {
        Some(overlay) => overlay,
        None => return Ok(()),
    };

    match overlay {
        Overlay::Help => {
            let close = matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter
            );
            if !close {
                app.overlay = Some(Overlay::Help);
            }
        }
        Overlay::PackageInfo(mut state) => {
            let mut close = false;
            let max_scroll = state.lines.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => close = true,
                KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
                KeyCode::Down => state.scroll = (state.scroll + 1).min(max_scroll),
                KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
                KeyCode::PageDown => state.scroll = (state.scroll + 10).min(max_scroll),
                KeyCode::Home => state.scroll = 0,
                KeyCode::End => state.scroll = max_scroll,
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => close = true,
                KeyCode::Char('i') if key.modifiers.contains(KeyModifiers::CONTROL) => close = true,
                _ => {}
            }
            if !close {
                app.overlay = Some(Overlay::PackageInfo(state));
            }
        }
        Overlay::Columns(mut state) => {
            let mut close = false;
            let max = tui::app::COLUMN_OPTIONS.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Up => {
                    if state.cursor > 0 {
                        state.cursor -= 1;
                    }
                }
                KeyCode::Down => {
                    state.cursor = (state.cursor + 1).min(max);
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(option) = tui::app::COLUMN_OPTIONS.get(state.cursor) {
                        toggle_column_setting(app, option.kind);
                    }
                }
                _ => {}
            }
            if !close {
                app.overlay = Some(Overlay::Columns(state));
            }
        }
        Overlay::Filter(mut state) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                let value = state.input.trim().to_string();
                match state.kind {
                    tui::app::FilterKind::License => app.filters.license = value,
                    tui::app::FilterKind::Platform => app.filters.platform = value,
                }
                update_search_results(conn, app)?;
            }
            KeyCode::Backspace => {
                if state.cursor > 0 {
                    state.cursor -= 1;
                    state.input.remove(state.cursor);
                }
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::Left => {
                if state.cursor > 0 {
                    state.cursor -= 1;
                }
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::Right => {
                if state.cursor < state.input.len() {
                    state.cursor += 1;
                }
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::Home => {
                state.cursor = 0;
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::End => {
                state.cursor = state.input.len();
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                state.input.insert(state.cursor, ch);
                state.cursor += 1;
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
            _ => {
                app.overlay = Some(Overlay::Filter(state));
                return Ok(());
            }
        },
        Overlay::Diff(mut state) => {
            let current_lines = if state.show_full {
                &state.full_lines
            } else {
                &state.change_lines
            };
            let max_scroll = current_lines.len().saturating_sub(1);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Up => state.scroll = state.scroll.saturating_sub(1),
                KeyCode::Down => state.scroll = (state.scroll + 1).min(max_scroll),
                KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
                KeyCode::PageDown => state.scroll = (state.scroll + 10).min(max_scroll),
                KeyCode::Home => state.scroll = 0,
                KeyCode::End => state.scroll = max_scroll,
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    state.show_full = !state.show_full;
                    let new_max = if state.show_full {
                        state.full_lines.len().saturating_sub(1)
                    } else {
                        state.change_lines.len().saturating_sub(1)
                    };
                    if state.scroll > new_max {
                        state.scroll = new_max;
                    }
                }
                _ => {}
            }
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                return Ok(());
            }
            app.overlay = Some(Overlay::Diff(state));
        }
        Overlay::Env(_) | Overlay::Shell(_) | Overlay::PinEditor(_) => {
            app.push_toast(tui::app::ToastLevel::Info, "Not available in global mode");
        }
    }

    Ok(())
}

fn pin_editor_active_input(editor: &mut tui::app::PinEditorState) -> (&mut String, &mut usize) {
    let (value, cursor) = match editor.active {
        tui::app::PinField::Name => (&mut editor.name, &mut editor.name_cursor),
        tui::app::PinField::Url => (&mut editor.url, &mut editor.url_cursor),
        tui::app::PinField::Branch => (&mut editor.branch, &mut editor.branch_cursor),
        tui::app::PinField::Rev => (&mut editor.rev, &mut editor.rev_cursor),
        tui::app::PinField::Sha256 => (&mut editor.sha256, &mut editor.sha256_cursor),
        tui::app::PinField::TarballName => {
            (&mut editor.tarball_name, &mut editor.tarball_name_cursor)
        }
    };
    if *cursor > value.len() {
        *cursor = value.len();
    }
    (value, cursor)
}

fn pin_editor_next_field(editor: &mut tui::app::PinEditorState) {
    let idx = tui::app::PIN_FIELDS
        .iter()
        .position(|field| *field == editor.active)
        .unwrap_or(0);
    let next = (idx + 1) % tui::app::PIN_FIELDS.len();
    editor.active = tui::app::PIN_FIELDS[next];
}

fn pin_editor_prev_field(editor: &mut tui::app::PinEditorState) {
    let idx = tui::app::PIN_FIELDS
        .iter()
        .position(|field| *field == editor.active)
        .unwrap_or(0);
    let prev = if idx == 0 {
        tui::app::PIN_FIELDS.len() - 1
    } else {
        idx - 1
    };
    editor.active = tui::app::PIN_FIELDS[prev];
}

fn submit_pin_editor(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    editor: &mut tui::app::PinEditorState,
    state: &mut ProjectState,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
) -> bool {
    editor.error = None;

    let name = editor.name.trim();
    if name.is_empty() {
        editor.error = Some("Name is required".to_string());
        return false;
    }
    if !is_valid_pin_name(name) {
        editor.error = Some("Name must be a valid identifier".to_string());
        return false;
    }
    let url = editor.url.trim();
    if url.is_empty() {
        editor.error = Some("URL is required".to_string());
        return false;
    }
    if !editor.use_latest && editor.rev.trim().is_empty() {
        editor.error = Some("Revision is required when latest is off".to_string());
        return false;
    }

    let name = name.to_string();
    let url = url.to_string();
    let branch = {
        let value = editor.branch.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    };
    let use_latest = editor.use_latest;
    let rev = if use_latest {
        None
    } else {
        Some(editor.rev.trim().to_string())
    };
    let sha256 = {
        let value = editor.sha256.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    };
    let tarball_name = {
        let value = editor.tarball_name.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    };

    if let Err(err) = with_tui_suspended(terminal, || {
        add_extra_pin(
            state,
            AddPinRequest {
                name,
                url,
                branch,
                tarball_name,
                rev,
                sha256,
                latest: use_latest,
            },
        )?;
        save_project_state(state)?;
        let pins = collect_index_pins(state);
        rebuild_index_from_pins_with_spinner(index_path, &pins)?;
        Ok(())
    }) {
        editor.error = Some(err.to_string());
        return false;
    }

    match open_db(index_path) {
        Ok(new_conn) => {
            *conn = new_conn;
            app.index_info = index_info_from_meta(get_meta(conn).unwrap_or_default());
            if let Err(err) = update_search_results(conn, app) {
                app.push_toast(tui::app::ToastLevel::Error, err.to_string());
            }
        }
        Err(err) => {
            app.push_toast(tui::app::ToastLevel::Error, err.to_string());
        }
    }

    app.push_toast(tui::app::ToastLevel::Info, "Pin added");
    true
}

fn with_tui_suspended<T>(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    action: impl FnOnce() -> Result<T, CliError>,
) -> Result<T, CliError> {
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };

    disable_raw_mode().map_err(CliError::WriteNix)?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(CliError::WriteNix)?;
    terminal.show_cursor().map_err(CliError::WriteNix)?;

    let result = action();

    crossterm::execute!(terminal.backend_mut(), EnterAlternateScreen)
        .map_err(CliError::WriteNix)?;
    enable_raw_mode().map_err(CliError::WriteNix)?;
    let _ = terminal.hide_cursor();
    terminal.clear().map_err(CliError::WriteNix)?;
    result
}

fn update_search_results(
    conn: &rusqlite::Connection,
    app: &mut tui::app::App,
) -> Result<(), CliError> {
    let limit = 1000usize;
    let query = app.query.trim();
    let packages = if query.is_empty() {
        list_packages(conn, limit + 1)?
    } else {
        search_packages(conn, query, limit + 1)?
    };

    let total_fetched = packages.len();
    let entries: Vec<tui::app::PackageEntry> = packages
        .into_iter()
        .take(limit)
        .map(|pkg| tui::app::PackageEntry {
            attr_path: pkg.attr_path.clone(),
            name: normalize_attr_path(&pkg.attr_path),
            version: pkg.version,
            description: pkg.description,
            homepage: pkg.homepage,
            license: pkg.license,
            platforms: pkg.platforms,
            main_program: pkg.main_program,
            position: pkg.position,
            broken: pkg.broken,
            insecure: pkg.insecure,
        })
        .filter(|pkg| {
            app.filters.matches(pkg)
                && (!app.filters.show_installed_only || app.is_installed(&pkg.name))
        })
        .collect();

    let display_total = if total_fetched > limit {
        Some(limit + 1)
    } else {
        Some(total_fetched)
    };

    app.packages = entries;
    app.index_info.displayed_count = display_total;
    app.cursor = 0;
    if app.packages.is_empty() {
        app.packages_state.select(None);
    } else {
        app.packages_state.select(Some(0));
    }
    Ok(())
}

fn apply_state_to_app(app: &mut tui::app::App, state: &ProjectState) {
    app.added = state.packages.added.iter().cloned().collect();
    app.removed = state.packages.removed.iter().cloned().collect();
    app.active_presets = state.presets.active.iter().cloned().collect();
    app.env = state.env.clone();
    app.shell_hook = state.shell.hook.clone();
    app.rebuild_preset_packages();
    app.commit_baseline();
}

fn apply_profile_state_to_app(app: &mut tui::app::App, state: &GlobalProfileState) {
    app.added = state.packages.added.iter().cloned().collect();
    app.removed = state.packages.removed.iter().cloned().collect();
    app.active_presets = state.presets.active.iter().cloned().collect();
    app.env.clear();
    app.shell_hook = None;
    app.rebuild_preset_packages();
    app.commit_baseline();
}

fn apply_columns_from_config(app: &mut tui::app::App, config: &Config) {
    app.columns = tui::app::ColumnSettings {
        show_version: config.tui.columns.version,
        show_description: config.tui.columns.description,
        show_license: config.tui.columns.license,
        show_platforms: config.tui.columns.platforms,
        show_main_program: config.tui.columns.main_program,
    };
}

fn save_columns_to_config(columns: &tui::app::ColumnSettings) -> Result<(), CliError> {
    ensure_config_dir()?;
    let mut config = load_config_or_default()?;
    config.tui.columns = mica_core::config::TuiColumns {
        version: columns.show_version,
        description: columns.show_description,
        license: columns.show_license,
        platforms: columns.show_platforms,
        main_program: columns.show_main_program,
    };
    config
        .save_to_path(&config_path()?)
        .map_err(CliError::Config)
}

fn toggle_column_setting(app: &mut tui::app::App, column: tui::app::ColumnKind) {
    app.toggle_column(column);
    if let Err(err) = save_columns_to_config(&app.columns) {
        app.push_toast(tui::app::ToastLevel::Error, err.to_string());
    }
}

fn open_env_overlay(app: &mut tui::app::App) {
    let mut entries: Vec<(String, String)> = app
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    app.overlay = Some(tui::app::Overlay::Env(tui::app::EnvEditorState {
        entries,
        cursor: 0,
        input: String::new(),
        input_cursor: 0,
        mode: tui::app::EnvEditMode::List,
        error: None,
    }));
}

fn open_shell_overlay(app: &mut tui::app::App) {
    let lines: Vec<String> = app
        .shell_hook
        .as_deref()
        .unwrap_or("")
        .lines()
        .map(|line| line.to_string())
        .collect();
    let lines = if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    };
    app.overlay = Some(tui::app::Overlay::Shell(tui::app::ShellEditorState {
        original: lines.clone(),
        lines,
        cursor_row: 0,
        cursor_col: 0,
    }));
}

fn open_filter_overlay(app: &mut tui::app::App, kind: tui::app::FilterKind) {
    let input = match kind {
        tui::app::FilterKind::License => app.filters.license.clone(),
        tui::app::FilterKind::Platform => app.filters.platform.clone(),
    };
    app.overlay = Some(tui::app::Overlay::Filter(tui::app::FilterEditorState {
        cursor: input.len(),
        input,
        kind,
    }));
}

fn build_diff_overlay(
    state: &ProjectState,
    app: &tui::app::App,
) -> Result<tui::app::Overlay, CliError> {
    let mut temp_state = state.clone();
    temp_state.packages.added = app.added.iter().cloned().collect();
    temp_state.packages.removed = app.removed.iter().cloned().collect();
    temp_state.presets.active = app.active_presets.iter().cloned().collect();
    temp_state.env = app.env.clone();
    temp_state.shell.hook = app.shell_hook.clone();

    let generated = format_mica_nix(&build_project_nix(&temp_state)?);
    let existing = std::fs::read_to_string(project_nix_path()).map_err(CliError::ReadNix)?;
    let full_diff = diff_lines(&existing, &generated);
    let mut changes_only = diff_lines_changes_only(&existing, &generated);
    if changes_only.is_empty() {
        changes_only.push("No changes".to_string());
    }

    Ok(tui::app::Overlay::Diff(tui::app::DiffViewerState {
        full_lines: full_diff,
        change_lines: changes_only,
        show_full: false,
        scroll: 0,
    }))
}

fn build_diff_overlay_profile(
    state: &GlobalProfileState,
    app: &tui::app::App,
) -> Result<tui::app::Overlay, CliError> {
    let mut temp_state = state.clone();
    temp_state.packages.added = app.added.iter().cloned().collect();
    temp_state.packages.removed = app.removed.iter().cloned().collect();
    temp_state.presets.active = app.active_presets.iter().cloned().collect();

    let presets = load_all_presets()?;
    let mut preset_map = BTreeMap::new();
    for preset in presets {
        preset_map.insert(preset.name.clone(), preset);
    }
    let mut active_presets = Vec::new();
    for name in &temp_state.presets.active {
        match preset_map.get(name) {
            Some(preset) => active_presets.push(preset.clone()),
            None => return Err(CliError::MissingPreset(name.clone())),
        }
    }
    let merged = merge_profile_presets(&active_presets, &temp_state);
    let generated = generate_profile_nix(&temp_state, &merged, Utc::now());
    let generated = format_mica_nix(&generated);
    let existing = std::fs::read_to_string(profile_nix_path()?).map_err(CliError::ReadNix)?;

    let full_diff = diff_lines(&existing, &generated);
    let mut changes_only = diff_lines_changes_only(&existing, &generated);
    if changes_only.is_empty() {
        changes_only.push("No changes".to_string());
    }

    Ok(tui::app::Overlay::Diff(tui::app::DiffViewerState {
        full_lines: full_diff,
        change_lines: changes_only,
        show_full: false,
        scroll: 0,
    }))
}

fn build_package_info_overlay(
    app: &tui::app::App,
    state: &ProjectState,
) -> Option<tui::app::Overlay> {
    let pins = collect_index_pins(state);
    build_package_info_overlay_with_pins(app, &pins)
}

fn build_package_info_overlay_with_pins(
    app: &tui::app::App,
    pins: &[IndexPin],
) -> Option<tui::app::Overlay> {
    let pkg = app.packages.get(app.cursor)?;
    let mut lines = Vec::new();
    lines.push(format!("Name: {}", pkg.name));
    lines.push(format!("Attr path: {}", pkg.attr_path));
    lines.push(format!(
        "Version: {}",
        pkg.version.as_deref().unwrap_or("unknown")
    ));

    let mut pin_label = "primary".to_string();
    let mut pin_url = pins
        .first()
        .map(|pin| pin.pin.url.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let mut pin_rev = pins
        .first()
        .map(|pin| pin.pin.rev.clone())
        .unwrap_or_else(|| "unknown".to_string());
    for pin in pins {
        let Some(label) = pin.name.as_ref() else {
            continue;
        };
        if pkg.attr_path.starts_with(&format!("{}.", label)) {
            pin_label = label.clone();
            pin_url = pin.pin.url.clone();
            pin_rev = pin.pin.rev.clone();
            break;
        }
    }

    lines.push(format!("Pin: {}", pin_label));
    lines.push(format!("Pin URL: {}", pin_url));
    lines.push(format!("Pin rev: {}", pin_rev));
    lines.push(format!(
        "Source: {}",
        pkg.position.as_deref().unwrap_or("unknown")
    ));

    if let Some(homepage) = pkg.homepage.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("Homepage: {}", homepage));
    }
    if let Some(main_program) = pkg.main_program.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("Main program: {}", main_program));
    }
    if let Some(license) = pkg.license.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("License: {}", license));
    }
    if let Some(platforms) = pkg.platforms.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("Platforms: {}", platforms));
    }
    if pkg.broken || pkg.insecure {
        let mut flags = Vec::new();
        if pkg.broken {
            flags.push("broken");
        }
        if pkg.insecure {
            flags.push("insecure");
        }
        lines.push(format!("Flags: {}", flags.join(", ")));
    }

    if let Some(description) = pkg.description.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push("Description:".to_string());
        for line in description.lines() {
            lines.push(format!("  {}", line));
        }
    }

    Some(tui::app::Overlay::PackageInfo(tui::app::PackageInfoState {
        lines,
        scroll: 0,
    }))
}

fn apply_env_input(state: &mut tui::app::EnvEditorState) -> Result<(), String> {
    let input = state.input.trim();
    if input.is_empty() {
        return Err("entry cannot be empty".to_string());
    }
    let (raw_key, raw_value) = input
        .split_once('=')
        .ok_or_else(|| "use KEY=VALUE".to_string())?;
    let key = raw_key.trim();
    if key.is_empty() {
        return Err("key cannot be empty".to_string());
    }
    let value = raw_value.trim().to_string();

    let original = match &state.mode {
        tui::app::EnvEditMode::Edit { original_key } => original_key.clone(),
        _ => None,
    };

    if let Some(original_key) = &original {
        if original_key != key && state.entries.iter().any(|(entry_key, _)| entry_key == key) {
            return Err("key already exists".to_string());
        }
        if let Some(entry) = state
            .entries
            .iter_mut()
            .find(|(entry_key, _)| entry_key == original_key)
        {
            entry.0 = key.to_string();
            entry.1 = value;
        } else {
            state.entries.push((key.to_string(), value));
        }
    } else {
        if state.entries.iter().any(|(entry_key, _)| entry_key == key) {
            return Err("key already exists".to_string());
        }
        state.entries.push((key.to_string(), value));
    }

    state.entries.sort_by(|a, b| a.0.cmp(&b.0));
    state.cursor = state
        .entries
        .iter()
        .position(|(entry_key, _)| entry_key == key)
        .unwrap_or(0);
    state.mode = tui::app::EnvEditMode::List;
    state.input.clear();
    state.input_cursor = 0;
    state.error = None;
    Ok(())
}

fn apply_env_overlay(app: &mut tui::app::App, state: tui::app::EnvEditorState) {
    let mut env = BTreeMap::new();
    for (key, value) in state.entries {
        if !key.trim().is_empty() {
            env.insert(key, value);
        }
    }
    app.env = env;
    app.update_dirty();
}

fn apply_shell_overlay(app: &mut tui::app::App, lines: &[String]) {
    let combined = lines.join("\n");
    if combined.trim().is_empty() {
        app.shell_hook = None;
    } else {
        app.shell_hook = Some(combined);
    }
    app.update_dirty();
}

fn ensure_shell_lines(state: &mut tui::app::ShellEditorState) {
    if state.lines.is_empty() {
        state.lines.push(String::new());
    }
    if state.cursor_row >= state.lines.len() {
        state.cursor_row = state.lines.len().saturating_sub(1);
    }
    let line_len = state.lines[state.cursor_row].len();
    if state.cursor_col > line_len {
        state.cursor_col = line_len;
    }
}

fn diff_lines(old: &str, new: &str) -> Vec<String> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut dp = vec![vec![0usize; new_lines.len() + 1]; old_lines.len() + 1];

    for i in (0..old_lines.len()).rev() {
        for j in (0..new_lines.len()).rev() {
            if old_lines[i] == new_lines[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    let mut out = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < old_lines.len() && j < new_lines.len() {
        if old_lines[i] == new_lines[j] {
            out.push(format!("  {}", old_lines[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(format!("- {}", old_lines[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", new_lines[j]));
            j += 1;
        }
    }

    while i < old_lines.len() {
        out.push(format!("- {}", old_lines[i]));
        i += 1;
    }
    while j < new_lines.len() {
        out.push(format!("+ {}", new_lines[j]));
        j += 1;
    }

    out
}

fn diff_lines_changes_only(old: &str, new: &str) -> Vec<String> {
    diff_lines(old, new)
        .into_iter()
        .filter(|line| line.starts_with('+') || line.starts_with('-'))
        .collect()
}

fn index_info_from_meta(meta: Vec<(String, String)>) -> tui::app::IndexInfo {
    let mut info = tui::app::IndexInfo::default();
    for (key, value) in meta {
        match key.as_str() {
            "nixpkgs_url" => info.url = value,
            "nixpkgs_commit" => info.rev = value,
            "package_count" => info.count = value.parse().ok(),
            "generated_at" => info.generated_at = Some(value),
            _ => {}
        }
    }
    info
}

fn meta_has_key(meta: &[(String, String)], needle: &str) -> bool {
    meta.iter()
        .any(|(key, value)| key == needle && value == "true")
}

fn index_has_descriptions(conn: &rusqlite::Connection) -> Result<bool, CliError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(1) FROM packages WHERE description IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|err| CliError::Index(mica_index::generate::IndexError::Db(err)))?;
    Ok(count > 0)
}

fn load_tui_presets() -> Result<Vec<tui::app::PresetEntry>, CliError> {
    let mut presets: Vec<_> = load_all_presets()?
        .into_iter()
        .map(|preset| tui::app::PresetEntry {
            name: preset.name,
            description: preset.description,
            order: preset.order,
            packages_required: preset.packages_required,
        })
        .collect();
    presets.sort_by_key(|preset| preset.order);
    Ok(presets)
}

fn save_tui_selection(state: &mut ProjectState, app: &mut tui::app::App) -> Result<(), CliError> {
    state.packages.added = app.added.iter().cloned().collect();
    state.packages.removed = app.removed.iter().cloned().collect();
    state.presets.active = app.active_presets.iter().cloned().collect();
    state.env = app.env.clone();
    state.shell.hook = app.shell_hook.clone();
    update_project_modified(state);
    save_project_state(state)?;
    app.commit_baseline();
    Ok(())
}

fn save_profile_tui_selection(
    state: &mut GlobalProfileState,
    app: &mut tui::app::App,
) -> Result<(), CliError> {
    state.packages.added = app.added.iter().cloned().collect();
    state.packages.removed = app.removed.iter().cloned().collect();
    state.presets.active = app.active_presets.iter().cloned().collect();
    update_profile_modified(state);
    save_profile_state(state)?;
    sync_and_install_profile(state)?;
    app.commit_baseline();
    Ok(())
}

fn init_project_state(repo: Option<String>) -> Result<(), CliError> {
    let path = project_nix_path();
    if path.exists() {
        return Err(CliError::StateExists(path));
    }
    let config = load_config_or_default()?;
    let now = Utc::now();
    let url = resolve_init_repo(repo, &config);
    let branch = config.nixpkgs.default_branch.clone();
    let rev = fetch_latest_github_rev(&url, &branch)?;
    let sha256 = fetch_nix_sha256(&url, &rev)?;
    let state = ProjectState {
        mica: MicaMetadata {
            version: "0.1.0".to_string(),
            created: now,
            modified: now,
        },
        pin: Pin {
            name: None,
            url,
            rev,
            sha256,
            branch,
            updated: now.date_naive(),
        },
        pins: BTreeMap::new(),
        presets: PresetState::default(),
        packages: Default::default(),
        env: BTreeMap::new(),
        shell: ShellState::default(),
        nix: NixBlocks::default(),
    };
    sync_project_nix(&state)?;
    Ok(())
}

fn init_profile_state(repo: Option<String>) -> Result<(), CliError> {
    let path = profile_state_path()?;
    if path.exists() {
        return Err(CliError::StateExists(path));
    }
    ensure_config_dir()?;
    let config = load_config_or_default()?;
    let now = Utc::now();
    let url = resolve_init_repo(repo, &config);
    let branch = config.nixpkgs.default_branch.clone();
    let rev = fetch_latest_github_rev(&url, &branch)?;
    let sha256 = fetch_nix_sha256(&url, &rev)?;
    let state = GlobalProfileState {
        mica: MicaMetadata {
            version: "0.1.0".to_string(),
            created: now,
            modified: now,
        },
        pin: Pin {
            name: None,
            url,
            rev,
            sha256,
            branch,
            updated: now.date_naive(),
        },
        presets: PresetState::default(),
        packages: Default::default(),
        generations: Default::default(),
    };
    state.save_to_path(&path).map_err(CliError::State)
}

fn resolve_init_repo(repo: Option<String>, config: &Config) -> String {
    if let Some(repo) = repo {
        let trimmed = repo.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(repo) = std::env::var("MICA_NIXPKGS_REPO") {
        let trimmed = repo.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    config.nixpkgs.default_url.clone()
}

#[derive(Debug, Clone)]
struct IndexPin {
    name: Option<String>,
    pin: Pin,
}

fn collect_index_pins(state: &ProjectState) -> Vec<IndexPin> {
    let mut pins = Vec::new();
    pins.push(IndexPin {
        name: None,
        pin: state.pin.clone(),
    });

    let mut seen = BTreeSet::new();
    seen.insert((
        state.pin.url.clone(),
        state.pin.rev.clone(),
        state.pin.sha256.clone(),
    ));

    let mut used_labels = BTreeSet::new();
    for (name, pin) in &state.pins {
        let key = (pin.url.clone(), pin.rev.clone(), pin.sha256.clone());
        if !seen.insert(key) {
            continue;
        }
        let label = unique_pin_label(&sanitize_pin_label(name), &mut used_labels);
        pins.push(IndexPin {
            name: Some(label),
            pin: pin.clone(),
        });
    }
    for (pkg, pinned) in &state.packages.pinned {
        let key = (
            pinned.pin.url.clone(),
            pinned.pin.rev.clone(),
            pinned.pin.sha256.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        let base_label = format!("pin-{}", sanitize_pin_label(pkg));
        let label = unique_pin_label(&base_label, &mut used_labels);
        pins.push(IndexPin {
            name: Some(label),
            pin: pinned.pin.clone(),
        });
    }

    pins
}

fn collect_index_pins_profile(state: &GlobalProfileState) -> Vec<IndexPin> {
    let mut pins = Vec::new();
    pins.push(IndexPin {
        name: None,
        pin: state.pin.clone(),
    });

    let mut seen = BTreeSet::new();
    seen.insert((
        state.pin.url.clone(),
        state.pin.rev.clone(),
        state.pin.sha256.clone(),
    ));

    let mut used_labels = BTreeSet::new();
    for (pkg, pinned) in &state.packages.pinned {
        let key = (
            pinned.pin.url.clone(),
            pinned.pin.rev.clone(),
            pinned.pin.sha256.clone(),
        );
        if !seen.insert(key) {
            continue;
        }
        let base_label = format!("pin-{}", sanitize_pin_label(pkg));
        let label = unique_pin_label(&base_label, &mut used_labels);
        pins.push(IndexPin {
            name: Some(label),
            pin: pinned.pin.clone(),
        });
    }

    pins
}

fn sanitize_pin_label(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "pin".to_string()
    } else {
        out
    }
}

fn normalize_attr_paths(packages: &mut [mica_index::generate::NixPackage]) {
    for pkg in packages {
        pkg.attr_path = normalize_attr_path(&pkg.attr_path);
    }
}

fn normalize_attr_path(value: &str) -> String {
    value
        .strip_prefix("nixos.")
        .or_else(|| value.strip_prefix("pkgs."))
        .unwrap_or(value)
        .to_string()
}

fn packages_have_meta(packages: &[mica_index::generate::NixPackage]) -> bool {
    packages.iter().any(|pkg| {
        pkg.description.is_some()
            || pkg.homepage.is_some()
            || pkg.license.is_some()
            || pkg.platforms.is_some()
            || pkg.main_program.is_some()
            || pkg.broken.unwrap_or(false)
            || pkg.insecure.unwrap_or(false)
    })
}

fn build_index_skip_list(extra: &[String]) -> Vec<String> {
    let mut skip = vec![
        "home-packages".to_string(),
        "json-crack".to_string(),
        "nim1".to_string(),
        "nim-1_0".to_string(),
        "watcher".to_string(),
        "nixosTests".to_string(),
        "pkgs*".to_string(),
        "by-name".to_string(),
        "by-name*".to_string(),
        "lib".to_string(),
        "lib*".to_string(),
        "darwin".to_string(),
        "darwin*".to_string(),
        "pypy*Packages".to_string(),
        "python*Packages".to_string(),
    ];
    for entry in extra {
        if !skip.iter().any(|existing| existing == entry) {
            skip.push(entry.clone());
        }
    }
    skip.sort();
    skip.dedup();
    skip
}

fn glob_to_regex(pattern: &str) -> String {
    let mut out = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '.' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('$');
    out
}

fn keep_index_temp_files() -> bool {
    match std::env::var("MICA_KEEP_INDEX_NIX") {
        Ok(value) => matches!(
            value.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn parse_skip_list(value: &str) -> Vec<String> {
    let mut items: Vec<String> = value
        .split(',')
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.to_string())
        .collect();
    items.sort();
    items.dedup();
    items
}

fn parse_failed_attr(stderr: &str) -> Option<String> {
    let needle = "while evaluating the attribute '";
    for line in stderr.lines() {
        if let Some(start) = line.find(needle) {
            let rest = &line[start + needle.len()..];
            if let Some(end) = rest.find('\'') {
                let attr = rest[..end].trim();
                if !attr.is_empty() && !attr.contains('.') {
                    return Some(attr.to_string());
                }
            }
        }
    }
    let by_name = "/pkgs/by-name/";
    for line in stderr.lines() {
        if let Some(start) = line.find(by_name) {
            let rest = &line[start + by_name.len()..];
            let mut parts = rest.split('/');
            let _ = parts.next();
            if let Some(name) = parts.next() {
                let trimmed = name.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    for line in stderr.lines() {
        if let Some(start) = line.find("/pkgs/") {
            let rest = &line[start + "/pkgs/".len()..];
            let path = rest.split(':').next().unwrap_or(rest);
            let path = path.trim_end_matches(')');
            if let Some(file) = path.split('/').next_back() {
                if file == "default.nix" || file == "package.nix" {
                    if let Some(parent) = path.split('/').rev().nth(1) {
                        if !parent.is_empty() {
                            return Some(parent.to_string());
                        }
                    }
                } else if let Some(stem) = file.strip_suffix(".nix") {
                    if !stem.is_empty() {
                        return Some(stem.to_string());
                    }
                }
            }
        }
    }
    let missing = "error: attribute '";
    for line in stderr.lines() {
        if let Some(start) = line.find(missing) {
            let rest = &line[start + missing.len()..];
            if let Some(end) = rest.find('\'') {
                let attr = rest[..end].trim();
                if !attr.is_empty() && !attr.contains('.') {
                    return Some(attr.to_string());
                }
            }
        }
    }
    None
}

fn nix_env_show_trace() -> bool {
    match std::env::var("MICA_NIX_SHOW_TRACE") {
        Ok(value) => matches!(
            value.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn unique_pin_label(base: &str, used: &mut BTreeSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut idx = 2;
    loop {
        let candidate = format!("{}-{}", base, idx);
        if used.insert(candidate.clone()) {
            return candidate;
        }
        idx += 1;
    }
}

fn rebuild_index_from_json(
    input: &Path,
    output_path: &Path,
    pin: Option<&Pin>,
) -> Result<usize, CliError> {
    let mut packages = load_packages_from_json(input)?;
    normalize_attr_paths(&mut packages);
    let index_has_meta = packages_have_meta(&packages);
    rebuild_index_with_packages(output_path, &packages, pin, index_has_meta)
}

fn rebuild_index_from_pins(output_path: &Path, pins: &[IndexPin]) -> Result<usize, CliError> {
    let mut packages = Vec::new();
    for (idx, index_pin) in pins.iter().enumerate() {
        if idx == 0 {
            ensure_pin_complete(&index_pin.pin)?;
        } else if ensure_pin_complete(&index_pin.pin).is_err() {
            continue;
        }
        let mut pin_packages = load_packages_from_pin(&index_pin.pin)?;
        normalize_attr_paths(&mut pin_packages);
        if let Some(prefix) = &index_pin.name {
            for pkg in &mut pin_packages {
                pkg.attr_path = format!("{}.{}", prefix, pkg.attr_path);
            }
        } else if idx != 0 {
            for pkg in &mut pin_packages {
                pkg.attr_path = format!("pin.{}", pkg.attr_path);
            }
        }
        packages.extend(pin_packages);
    }

    let primary = pins.first().map(|entry| &entry.pin);
    rebuild_index_with_packages(output_path, &packages, primary, true)
}

fn rebuild_index_from_pins_with_spinner(
    output_path: &Path,
    pins: &[IndexPin],
) -> Result<usize, CliError> {
    run_with_spinner("building index", || {
        rebuild_index_from_pins(output_path, pins)
    })
}

fn load_packages_from_pin(pin: &Pin) -> Result<Vec<mica_index::generate::NixPackage>, CliError> {
    let expr_path = temp_index_nix_path();
    let json_path = temp_index_json_path();
    let mut skip = parse_skip_list(
        std::env::var("MICA_NIX_SKIP_ATTRS")
            .unwrap_or_default()
            .as_str(),
    );
    let mut attempts = 0usize;
    let max_attempts = 12usize;
    let mut use_show_trace = nix_env_show_trace();
    loop {
        attempts += 1;
        let skipped_label = if skip.is_empty() {
            "none".to_string()
        } else {
            skip.join(",")
        };
        eprintln!(
            "index attempt {}/{} (skipped: {}, show-trace: {})",
            attempts, max_attempts, skipped_label, use_show_trace
        );
        let all_skip = build_index_skip_list(&skip);
        let all_skip_label = if all_skip.is_empty() {
            "none".to_string()
        } else {
            all_skip.join(",")
        };
        eprintln!("index skip list: {}", all_skip_label);
        std::fs::write(&expr_path, nix_env_expression(pin, &all_skip))
            .map_err(CliError::WriteNix)?;

        let file = std::fs::File::create(&json_path).map_err(CliError::WriteNix)?;
        let mut args = vec![
            "-f",
            expr_path.to_str().unwrap_or_default(),
            "-qaP",
            "--json",
            "--meta",
        ];
        if use_show_trace {
            args.push("--show-trace");
        }
        let mut command = ProcessCommand::new("nix-env");
        command
            .args(args)
            .stdout(Stdio::from(file))
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                CliError::MissingNixEnv
            } else {
                CliError::NixEnvIo(err)
            }
        })?;

        let output = child.wait_with_output().map_err(CliError::NixEnvIo)?;
        if output.status.success() {
            let packages = load_packages_from_json(&json_path)?;
            if !keep_index_temp_files() {
                let _ = std::fs::remove_file(&expr_path);
                let _ = std::fs::remove_file(&json_path);
            }
            return Ok(packages);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if attempts < max_attempts {
            if let Some(attr) = parse_failed_attr(&stderr) {
                if !skip.iter().any(|entry| entry == &attr) {
                    skip.push(attr.clone());
                    eprintln!("index retry: skipping attr '{}'", attr);
                    continue;
                }
            } else if !use_show_trace {
                use_show_trace = true;
                eprintln!("index retry: enabling --show-trace");
                continue;
            }
        }

        let mut message = format!("status={}, stderr={}", output.status, stderr.trim());
        if keep_index_temp_files() {
            message.push_str(&format!(
                ", expr={}, json={}",
                expr_path.display(),
                json_path.display()
            ));
        }
        if !skip.is_empty() {
            message.push_str(&format!(", skipped={}", skip.join(",")));
        }
        if !keep_index_temp_files() {
            let _ = std::fs::remove_file(&expr_path);
            let _ = std::fs::remove_file(&json_path);
        }
        return Err(CliError::NixEnvFailed(message));
    }
}

fn rebuild_index_with_packages(
    output_path: &Path,
    packages: &[mica_index::generate::NixPackage],
    pin: Option<&Pin>,
    index_has_meta: bool,
) -> Result<usize, CliError> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(CliError::WriteNix)?;
    }
    let mut conn = init_db(output_path)?;
    ingest_packages(&mut conn, packages)?;
    let generated_at = Utc::now().to_rfc3339();
    set_meta(&conn, "generated_at", &generated_at)?;
    set_meta(&conn, "package_count", &packages.len().to_string())?;
    set_meta(&conn, "mica_version", "0.1.0")?;
    if index_has_meta {
        set_meta(&conn, "index_meta", "true")?;
    } else {
        set_meta(&conn, "index_meta", "false")?;
    }
    if let Some(pin) = pin {
        set_meta(&conn, "nixpkgs_url", &pin.url)?;
        set_meta(&conn, "nixpkgs_commit", &pin.rev)?;
    } else {
        set_meta(&conn, "nixpkgs_url", "unknown")?;
        set_meta(&conn, "nixpkgs_commit", "unknown")?;
    }
    Ok(packages.len())
}

fn temp_index_json_path() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!("mica-index-{}.json", suffix));
    path
}

fn temp_index_nix_path() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!("mica-index-{}.nix", suffix));
    path
}

fn nix_string_list(items: &[String]) -> String {
    if items.is_empty() {
        return "[ ]".to_string();
    }
    let mut out = String::from("[");
    for item in items {
        out.push(' ');
        out.push('"');
        out.push_str(&escape_nix_string(item));
        out.push('"');
    }
    out.push_str(" ]");
    out
}

fn nix_env_expression(pin: &Pin, skip: &[String]) -> String {
    let url = format!("{}/archive/{}.tar.gz", pin.url, pin.rev);
    let skip_regex: Vec<String> = skip.iter().map(|entry| glob_to_regex(entry)).collect();
    let skip_list = nix_string_list(&skip_regex);
    format!(
        r#"let
  src = builtins.fetchTarball {{
    url = "{url}";
    sha256 = "{sha256}";
  }};
  lockPath = src + "/flake.lock";
  lock = if builtins.pathExists lockPath
    then builtins.fromJSON (builtins.readFile lockPath)
    else null;
  nixpkgsLocked = if lock != null
    && lock ? nodes
    && lock.nodes ? nixpkgs
    && lock.nodes.nixpkgs ? locked
    then lock.nodes.nixpkgs.locked
    else null;
  nixpkgsSrc = if nixpkgsLocked != null
    && nixpkgsLocked ? owner
    && nixpkgsLocked ? repo
    && nixpkgsLocked ? rev
    && nixpkgsLocked ? narHash
    then builtins.fetchTarball {{
      url = "https://github.com/${{nixpkgsLocked.owner}}/${{nixpkgsLocked.repo}}/archive/${{nixpkgsLocked.rev}}.tar.gz";
      sha256 = nixpkgsLocked.narHash;
    }}
    else src;
  baseAttempt = builtins.tryEval (import src {{ }});
  baseFallback = import nixpkgsSrc {{ }};
  base = if baseAttempt.success then baseAttempt.value else baseFallback;
  isAttrSet = v: builtins.typeOf v == "set";
  isDerivation = v: isAttrSet v && v ? type && v.type == "derivation";
  pkgs = if base != null && isAttrSet base && base ? pkgs
    then base.pkgs
    else if base != null && isAttrSet base
    then base
    else baseFallback;
  sanitize = attrs:
    if attrs == null || !isAttrSet attrs
      then {{ }}
      else
        let namesAttempt = builtins.tryEval (builtins.attrNames attrs);
            skip = {skip_list};
            matchesSkip = name:
              builtins.any (pattern: builtins.match pattern name != null) skip;
            names = if namesAttempt.success
              then builtins.filter (name: !(matchesSkip name)) namesAttempt.value
              else [];
        in builtins.foldl' (acc: name:
             let attempt = builtins.tryEval attrs.${{name}};
             in if !attempt.success then acc
                else if isDerivation attempt.value
                  then acc // {{ ${{name}} = attempt.value; }}
                else if isAttrSet attempt.value
                  then acc // {{ ${{name}} = sanitize attempt.value; }}
                else acc
           ) {{ }} names;
in sanitize pkgs
"#,
        url = url,
        sha256 = pin.sha256,
        skip_list = skip_list
    )
}

fn run_with_spinner<T>(
    message: &str,
    action: impl FnOnce() -> Result<T, CliError>,
) -> Result<T, CliError> {
    if !io::stderr().is_terminal() {
        return action();
    }

    let done = Arc::new(AtomicBool::new(false));
    let done_handle = done.clone();
    let message = message.to_string();
    let message_thread = message.clone();
    let handle = thread::spawn(move || {
        let frames = ['|', '/', '-', '\\'];
        let mut index = 0usize;
        while !done_handle.load(Ordering::Relaxed) {
            eprint!("\r{} {}", message_thread, frames[index % frames.len()]);
            let _ = io::stderr().flush();
            index = index.wrapping_add(1);
            thread::sleep(Duration::from_millis(120));
        }
    });

    let result = action();
    done.store(true, Ordering::Relaxed);
    let _ = handle.join();
    match &result {
        Ok(_) => eprintln!("\r{} done", message),
        Err(_) => eprintln!("\r{} failed", message),
    }
    let _ = io::stderr().flush();
    result
}

fn load_project_state() -> Result<ProjectState, CliError> {
    let path = project_nix_path();
    if !path.exists() {
        return Err(CliError::MissingDefaultNix(path));
    }
    let content = std::fs::read_to_string(&path).map_err(CliError::ReadNix)?;
    let parsed = parse_project_state_from_nix(&content).map_err(CliError::NixStateParse)?;
    let now = Utc::now();
    let mut state = ProjectState {
        mica: MicaMetadata {
            version: "0.1.0".to_string(),
            created: now,
            modified: now,
        },
        pin: parsed.pin,
        pins: parsed.pins,
        presets: PresetState {
            active: parsed.presets,
        },
        packages: Default::default(),
        env: parsed.env,
        shell: ShellState {
            hook: parsed.shell_hook,
        },
        nix: parsed.nix,
    };

    state.pin.updated = now.date_naive();
    state.packages.added = compute_added_packages(parsed.packages, &state.presets.active)?;
    Ok(state)
}

fn load_profile_state() -> Result<GlobalProfileState, CliError> {
    let path = profile_state_path()?;
    if !path.exists() {
        return Err(CliError::MissingState(path));
    }
    GlobalProfileState::load_from_path(&path).map_err(CliError::State)
}

fn save_profile_state(state: &GlobalProfileState) -> Result<(), CliError> {
    state
        .save_to_path(&profile_state_path()?)
        .map_err(CliError::State)
}

fn save_project_state(state: &ProjectState) -> Result<(), CliError> {
    sync_project_nix(state)
}

fn build_project_nix(state: &ProjectState) -> Result<String, CliError> {
    ensure_pin_complete(&state.pin)?;
    let presets = load_all_presets()?;
    let mut preset_map = BTreeMap::new();
    for preset in presets {
        preset_map.insert(preset.name.clone(), preset);
    }
    let mut active_presets = Vec::new();
    for name in &state.presets.active {
        match preset_map.get(name) {
            Some(preset) => active_presets.push(preset.clone()),
            None => return Err(CliError::MissingPreset(name.clone())),
        }
    }
    let merged = merge_presets(&active_presets, state);
    let project_name = project_dir_name();
    let generated = generate_project_nix(state, &merged, &project_name, Utc::now());
    let output = if project_nix_path().exists() {
        let existing = std::fs::read_to_string(project_nix_path()).map_err(CliError::ReadNix)?;
        if let Ok(parsed_existing) = parse_nix_file(&existing) {
            if let Ok(parsed_generated) = parse_nix_file(&generated) {
                assemble_project_nix(ProjectNixParts {
                    preamble: &parsed_existing.preamble,
                    pin_section: &parsed_generated.pin_section,
                    pins_section: parsed_generated.pins_section.as_deref().unwrap_or(""),
                    let_section: parsed_generated.let_section.as_deref().unwrap_or(""),
                    packages_section: &parsed_generated.packages_section,
                    scripts_section: parsed_generated.scripts_section.as_deref().unwrap_or(""),
                    env_section: &parsed_generated.env_section,
                    shell_section: &parsed_generated.shell_hook_section,
                    override_section: parsed_generated.override_section.as_deref().unwrap_or(""),
                    override_shellhook_section: parsed_generated
                        .override_shellhook_section
                        .as_deref()
                        .unwrap_or(""),
                    override_merge_section: parsed_generated
                        .override_merge_section
                        .as_deref()
                        .unwrap_or(""),
                    project_name: &project_name,
                    postamble: if parsed_existing.override_merge_section.is_some()
                        || parsed_existing.override_section.is_some()
                        || parsed_existing.override_shellhook_section.is_some()
                    {
                        &parsed_existing.postamble
                    } else {
                        &parsed_generated.postamble
                    },
                })
            } else {
                generated
            }
        } else {
            generated
        }
    } else {
        generated
    };
    Ok(output)
}

fn format_mica_nix(source: &str) -> String {
    let cleaned = cleanup_mica_markers(source);
    let parsed = rnix::Root::parse(&cleaned);
    if parsed.errors().is_empty() {
        cleaned
    } else {
        source.to_string()
    }
}

fn cleanup_mica_markers(source: &str) -> String {
    let had_trailing_newline = source.ends_with('\n');
    let mut lines: Vec<String> = source
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect();

    strip_empty_marker_block(&mut lines, "mica:let:begin", "mica:let:end");
    strip_empty_marker_block(&mut lines, "mica:pins:begin", "mica:pins:end");
    strip_empty_marker_block(
        &mut lines,
        "mica:packages-raw:begin",
        "mica:packages-raw:end",
    );
    strip_empty_marker_block(&mut lines, "mica:scripts:begin", "mica:scripts:end");
    strip_empty_marker_block(&mut lines, "mica:env-raw:begin", "mica:env-raw:end");
    strip_empty_marker_block(&mut lines, "mica:override:begin", "mica:override:end");
    strip_empty_marker_block(
        &mut lines,
        "mica:override-shellhook:begin",
        "mica:override-shellhook:end",
    );
    strip_empty_marker_block(
        &mut lines,
        "mica:override-merge:begin",
        "mica:override-merge:end",
    );
    collapse_marker_whitespace(&mut lines);
    trim_trailing_blank_lines(&mut lines);

    let mut output = lines.join("\n");
    if had_trailing_newline {
        output.push('\n');
    }
    output
}

fn strip_empty_marker_block(lines: &mut Vec<String>, start: &str, end: &str) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].contains(start) {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].contains(end) {
                j += 1;
            }
            if j < lines.len() {
                let is_empty = lines[i + 1..j].iter().all(|line| line.trim().is_empty());
                if is_empty {
                    lines.drain(i..=j);
                    while i < lines.len() && lines[i].trim().is_empty() {
                        lines.remove(i);
                    }
                    continue;
                }
            }
        }
        i += 1;
    }
}

fn collapse_marker_whitespace(lines: &mut Vec<String>) {
    let mut i = 0;
    while i < lines.len() {
        if lines[i].contains("# mica:") {
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j > i + 2 {
                lines.drain(i + 2..j);
            }
        }
        i += 1;
    }
}

fn trim_trailing_blank_lines(lines: &mut Vec<String>) {
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
}

fn sync_project_nix(state: &ProjectState) -> Result<(), CliError> {
    let output = build_project_nix(state)?;
    let formatted = format_mica_nix(&output);
    std::fs::write(project_nix_path(), formatted).map_err(CliError::WriteNix)
}

fn sync_profile_nix(state: &GlobalProfileState) -> Result<(), CliError> {
    ensure_pin_complete(&state.pin)?;
    let presets = load_all_presets()?;
    let mut preset_map = BTreeMap::new();
    for preset in presets {
        preset_map.insert(preset.name.clone(), preset);
    }
    let mut active_presets = Vec::new();
    for name in &state.presets.active {
        match preset_map.get(name) {
            Some(preset) => active_presets.push(preset.clone()),
            None => return Err(CliError::MissingPreset(name.clone())),
        }
    }
    let merged = merge_profile_presets(&active_presets, state);
    let generated = generate_profile_nix(state, &merged, Utc::now());
    let formatted = format_mica_nix(&generated);
    std::fs::write(profile_nix_path()?, formatted).map_err(CliError::WriteNix)
}

fn sync_and_install_profile(state: &GlobalProfileState) -> Result<(), CliError> {
    sync_profile_nix(state)?;
    run_with_spinner("installing global profile", install_profile_nix)
}

fn install_profile_nix() -> Result<(), CliError> {
    let path = profile_nix_path()?;
    let mut command = ProcessCommand::new("nix-env");
    command
        .arg("-if")
        .arg(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command
        .spawn()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                CliError::MissingNixEnv
            } else {
                CliError::NixEnvIo(err)
            }
        })?
        .wait_with_output()
        .map_err(CliError::NixEnvIo)?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = format!(
            "status={}, stdout={}, stderr={}",
            output.status,
            stdout.trim(),
            stderr.trim()
        );
        return Err(CliError::NixEnvFailed(message));
    }

    Ok(())
}

fn diff_project(state: &ProjectState) -> Result<(), CliError> {
    ensure_pin_complete(&state.pin)?;
    let presets = load_all_presets()?;
    let mut preset_map = BTreeMap::new();
    for preset in presets {
        preset_map.insert(preset.name.clone(), preset);
    }
    let mut active_presets = Vec::new();
    for name in &state.presets.active {
        match preset_map.get(name) {
            Some(preset) => active_presets.push(preset.clone()),
            None => return Err(CliError::MissingPreset(name.clone())),
        }
    }
    let merged = merge_presets(&active_presets, state);
    let project_name = project_dir_name();
    let generated = generate_project_nix(state, &merged, &project_name, Utc::now());
    let existing = std::fs::read_to_string(project_nix_path()).map_err(CliError::ReadNix)?;
    let parsed_generated = parse_nix_file(&generated).map_err(CliError::NixParse)?;
    let parsed_existing = parse_nix_file(&existing).map_err(CliError::NixParse)?;

    let pin_changed = parsed_generated.pin_section != parsed_existing.pin_section;
    let let_changed = parsed_generated.let_section != parsed_existing.let_section;
    let packages_changed = parsed_generated.packages_section != parsed_existing.packages_section;
    let env_changed = parsed_generated.env_section != parsed_existing.env_section;
    let shell_changed = parsed_generated.shell_hook_section != parsed_existing.shell_hook_section;
    let override_changed = parsed_generated.override_section != parsed_existing.override_section;
    let override_shellhook_changed =
        parsed_generated.override_shellhook_section != parsed_existing.override_shellhook_section;
    let override_merge_changed =
        parsed_generated.override_merge_section != parsed_existing.override_merge_section;

    if !(pin_changed
        || let_changed
        || packages_changed
        || env_changed
        || shell_changed
        || override_changed
        || override_shellhook_changed
        || override_merge_changed)
    {
        println!("no drift detected");
    } else {
        println!("drift detected:");
        println!("  pin: {}", if pin_changed { "changed" } else { "ok" });
        println!("  let: {}", if let_changed { "changed" } else { "ok" });
        println!(
            "  packages: {}",
            if packages_changed { "changed" } else { "ok" }
        );
        println!("  env: {}", if env_changed { "changed" } else { "ok" });
        println!(
            "  shellHook: {}",
            if shell_changed { "changed" } else { "ok" }
        );
        println!(
            "  override: {}",
            if override_changed { "changed" } else { "ok" }
        );
        println!(
            "  override shellHook: {}",
            if override_shellhook_changed {
                "changed"
            } else {
                "ok"
            }
        );
        println!(
            "  override merge: {}",
            if override_merge_changed {
                "changed"
            } else {
                "ok"
            }
        );
    }
    Ok(())
}

fn diff_profile(state: &GlobalProfileState) -> Result<(), CliError> {
    ensure_pin_complete(&state.pin)?;
    let presets = load_all_presets()?;
    let mut preset_map = BTreeMap::new();
    for preset in presets {
        preset_map.insert(preset.name.clone(), preset);
    }
    let mut active_presets = Vec::new();
    for name in &state.presets.active {
        match preset_map.get(name) {
            Some(preset) => active_presets.push(preset.clone()),
            None => return Err(CliError::MissingPreset(name.clone())),
        }
    }
    let merged = merge_profile_presets(&active_presets, state);
    let generated = generate_profile_nix(state, &merged, Utc::now());
    let existing = std::fs::read_to_string(profile_nix_path()?).map_err(CliError::ReadNix)?;
    let parsed_generated = parse_profile_nix(&generated).map_err(CliError::NixParse)?;
    let parsed_existing = parse_profile_nix(&existing).map_err(CliError::NixParse)?;

    let pins_changed = parsed_generated.pins_section != parsed_existing.pins_section;
    let paths_changed = parsed_generated.paths_section != parsed_existing.paths_section;

    if !(pins_changed || paths_changed) {
        println!("no drift detected");
    } else {
        println!("drift detected:");
        println!("  pins: {}", if pins_changed { "changed" } else { "ok" });
        println!("  paths: {}", if paths_changed { "changed" } else { "ok" });
    }
    Ok(())
}

fn update_project_state_from_nix(state: &mut ProjectState) -> Result<(), CliError> {
    let content = std::fs::read_to_string(project_nix_path()).map_err(CliError::ReadNix)?;
    let parsed = parse_project_state_from_nix(&content).map_err(CliError::NixStateParse)?;
    state.pin = parsed.pin;
    state.pins = parsed.pins;
    state.packages.added = compute_added_packages(parsed.packages, &parsed.presets)?;
    state.env = parsed.env;
    state.shell.hook = parsed.shell_hook;
    state.presets.active = parsed.presets;
    state.nix = parsed.nix;
    update_project_modified(state);
    Ok(())
}

fn update_profile_state_from_nix(state: &mut GlobalProfileState) -> Result<(), CliError> {
    let content = std::fs::read_to_string(profile_nix_path()?).map_err(CliError::ReadNix)?;
    let parsed = parse_profile_state_from_nix(&content).map_err(CliError::NixStateParse)?;
    state.pin = parsed.pin;
    state.packages.added = parsed.packages;
    update_profile_modified(state);
    Ok(())
}

fn update_project_modified(state: &mut ProjectState) {
    state.mica.modified = Utc::now();
}

fn update_profile_modified(state: &mut GlobalProfileState) {
    state.mica.modified = Utc::now();
}

fn update_pin_fields(
    pin: &mut Pin,
    url: Option<String>,
    rev: Option<String>,
    sha256: Option<String>,
    branch: Option<String>,
) {
    if let Some(url) = url {
        pin.url = url;
    }
    if let Some(rev) = rev {
        pin.rev = rev;
    }
    if let Some(sha256) = sha256 {
        pin.sha256 = sha256;
    }
    if let Some(branch) = branch {
        pin.branch = branch;
    }
}

fn is_valid_pin_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

struct AddPinRequest {
    name: String,
    url: String,
    branch: Option<String>,
    tarball_name: Option<String>,
    rev: Option<String>,
    sha256: Option<String>,
    latest: bool,
}

fn add_extra_pin(state: &mut ProjectState, request: AddPinRequest) -> Result<(), CliError> {
    let name = request.name.trim();
    if !is_valid_pin_name(name) {
        return Err(CliError::InvalidPinName(name.to_string()));
    }
    if state.pins.contains_key(name) {
        return Err(CliError::PinExists(name.to_string()));
    }
    let url = request.url.trim().to_string();
    let mut branch = request.branch.unwrap_or_else(|| state.pin.branch.clone());
    if branch.trim().is_empty() {
        branch = "main".to_string();
    }
    let use_latest = request.latest || request.rev.is_none();
    let (resolved_rev, resolved_sha256) = resolve_update_rev_and_sha(
        &state.pin,
        &Some(url.clone()),
        &Some(branch.clone()),
        request.rev,
        request.sha256,
        use_latest,
    )?;
    let rev = resolved_rev.ok_or(CliError::IncompletePin)?;
    let sha256 = resolved_sha256.ok_or(CliError::IncompletePin)?;
    let tarball_name = request.tarball_name.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    state.pins.insert(
        name.to_string(),
        Pin {
            name: tarball_name,
            url,
            rev,
            sha256,
            branch,
            updated: Utc::now().date_naive(),
        },
    );
    update_project_modified(state);
    Ok(())
}

fn resolve_update_rev_and_sha(
    base_pin: &Pin,
    url: &Option<String>,
    branch: &Option<String>,
    rev: Option<String>,
    sha256: Option<String>,
    latest: bool,
) -> Result<(Option<String>, Option<String>), CliError> {
    let resolved_rev = if latest {
        Some(latest_rev_from_github(url, branch, base_pin)?)
    } else {
        rev
    };
    let resolved_sha256 = if sha256.is_some() {
        sha256
    } else if let Some(ref resolved_rev) = resolved_rev {
        let effective_url = url.clone().unwrap_or_else(|| base_pin.url.clone());
        Some(fetch_nix_sha256(&effective_url, resolved_rev)?)
    } else {
        None
    };
    Ok((resolved_rev, resolved_sha256))
}

fn latest_rev_from_github(
    url: &Option<String>,
    branch: &Option<String>,
    base_pin: &Pin,
) -> Result<String, CliError> {
    let effective_url = url.clone().unwrap_or_else(|| base_pin.url.clone());
    let mut effective_branch = branch.clone().unwrap_or_else(|| base_pin.branch.clone());
    if effective_branch.trim().is_empty() {
        effective_branch = "main".to_string();
    }
    fetch_latest_github_rev(&effective_url, &effective_branch)
}

fn fetch_latest_github_rev(url: &str, branch: &str) -> Result<String, CliError> {
    let (owner, repo) = parse_github_repo(url)?;
    let branch = if branch.trim().is_empty() {
        "main"
    } else {
        branch
    };
    let ref_encoded = encode_github_ref(branch);
    let api_url = format!(
        "https://api.github.com/repos/{}/{}/commits/{}",
        owner, repo, ref_encoded
    );
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    let response = client
        .get(&api_url)
        .header("User-Agent", format!("mica/{}", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(CliError::GitHubApiStatus(status, body));
    }

    let commit: GitHubCommit = response.json()?;
    if commit.sha.trim().is_empty() {
        return Err(CliError::GitHubApiMissingSha);
    }
    Ok(commit.sha)
}

fn fetch_nix_sha256(url: &str, rev: &str) -> Result<String, CliError> {
    let tarball_url = format!("{}/archive/{}.tar.gz", url, rev);
    prefetch_nix_sha256(&tarball_url)
}

fn prefetch_nix_sha256(url: &str) -> Result<String, CliError> {
    let output = ProcessCommand::new("nix-prefetch-url")
        .arg("--unpack")
        .arg(url)
        .output()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                CliError::MissingNixPrefetch
            } else {
                CliError::NixPrefetchIo(err)
            }
        })?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = format!(
            "status={}, stdout={}, stderr={}",
            output.status,
            stdout.trim(),
            stderr.trim()
        );
        return Err(CliError::NixPrefetchFailed(message));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if let Some(hash) =
        extract_nix_base32_hash(stdout.trim()).or_else(|| extract_nix_base32_hash(stderr.trim()))
    {
        return Ok(hash);
    }

    Err(CliError::NixPrefetchMissingHash)
}

fn extract_nix_base32_hash(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(|line| line.trim())
        .find(|line| is_nix_base32_hash(line))
        .map(|line| line.to_string())
}

fn is_nix_base32_hash(value: &str) -> bool {
    if value.len() != 52 {
        return false;
    }
    value.chars().all(|ch| {
        matches!(
            ch,
            '0'..='9'
                | 'a'
                | 'b'
                | 'c'
                | 'd'
                | 'f'
                | 'g'
                | 'h'
                | 'i'
                | 'j'
                | 'k'
                | 'l'
                | 'm'
                | 'n'
                | 'p'
                | 'q'
                | 'r'
                | 's'
                | 'v'
                | 'w'
                | 'x'
                | 'y'
                | 'z'
        )
    })
}

fn parse_github_repo(url: &str) -> Result<(String, String), CliError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(CliError::InvalidGitHubUrl(url.to_string()));
    }

    let rest = if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("github.com/") {
        rest
    } else {
        return Err(CliError::InvalidGitHubUrl(trimmed.to_string()));
    };

    let rest = rest.trim_end_matches('/');
    let mut split = rest.split(['?', '#']);
    let rest = match split.next() {
        Some(value) => value,
        None => rest,
    };
    let mut parts = rest.split('/').filter(|part| !part.is_empty());
    let owner = parts
        .next()
        .ok_or_else(|| CliError::InvalidGitHubUrl(trimmed.to_string()))?;
    let repo = parts
        .next()
        .ok_or_else(|| CliError::InvalidGitHubUrl(trimmed.to_string()))?;
    if parts.next().is_some() {
        return Err(CliError::InvalidGitHubUrl(trimmed.to_string()));
    }
    let repo = match repo.strip_suffix(".git") {
        Some(stripped) => stripped,
        None => repo,
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(CliError::InvalidGitHubUrl(trimmed.to_string()));
    }

    Ok((owner.to_string(), repo.to_string()))
}

fn encode_github_ref(reference: &str) -> String {
    let mut out = String::new();
    for byte in reference.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

struct ProjectNixParts<'a> {
    preamble: &'a str,
    pin_section: &'a str,
    pins_section: &'a str,
    let_section: &'a str,
    packages_section: &'a str,
    scripts_section: &'a str,
    env_section: &'a str,
    shell_section: &'a str,
    override_section: &'a str,
    override_shellhook_section: &'a str,
    override_merge_section: &'a str,
    project_name: &'a str,
    postamble: &'a str,
}

fn assemble_project_nix(parts: ProjectNixParts<'_>) -> String {
    let mut output = String::new();
    output.push_str(parts.preamble);
    if !parts.preamble.ends_with('\n') {
        output.push('\n');
    }
    push_marker_block(&mut output, "    ", "mica:pin", parts.pin_section);
    output.push_str("  }) {}\n");
    push_marker_block(&mut output, "  ", "mica:pins", parts.pins_section);
    output.push_str("}:\n\n");
    output.push_str("let\n");
    output.push_str(&format!(
        "  name = \"{}\";\n\n",
        escape_nix_string(parts.project_name)
    ));
    push_marker_block(&mut output, "  ", "mica:let", parts.let_section);
    output.push('\n');
    output.push_str("  scripts = with pkgs; {\n");
    push_marker_block(&mut output, "    ", "mica:scripts", parts.scripts_section);
    output.push_str("  };\n\n");
    push_marker_block(&mut output, "  ", "mica:packages", parts.packages_section);
    output.push('\n');
    output.push_str("  paths = pkgs.lib.flatten [ tools ];\n");
    output.push_str("  env = pkgs.buildEnv {\n");
    output.push_str("    inherit name paths; buildInputs = paths;\n");
    push_marker_block(&mut output, "    ", "mica:env", parts.env_section);
    output.push('\n');
    push_marker_block(&mut output, "    ", "mica:shellhook", parts.shell_section);
    output.push_str("  };\n");
    output.push_str("in\n");
    output.push_str("env.overrideAttrs (prev: {\n");
    push_marker_block(&mut output, "  ", "mica:override", parts.override_section);
    if !parts.override_shellhook_section.trim().is_empty() {
        push_marker_block(
            &mut output,
            "  ",
            "mica:override-shellhook",
            parts.override_shellhook_section,
        );
    }
    output.push_str("}\n");
    push_marker_block(
        &mut output,
        "  ",
        "mica:override-merge",
        parts.override_merge_section,
    );
    output.push_str("  // { inherit scripts; }\n");
    output.push_str(parts.postamble);
    output
}

fn escape_nix_string(value: &str) -> String {
    let mut out = value.replace('\\', "\\\\").replace('\"', "\\\"");
    if out.contains("${") {
        out = out.replace("${", "\\${");
    }
    out
}

fn push_marker_block(output: &mut String, indent: &str, name: &str, section: &str) {
    output.push_str(indent);
    output.push_str("# ");
    output.push_str(name);
    output.push_str(":begin\n");
    let trimmed = section.trim_matches('\n');
    if !trimmed.is_empty() {
        output.push_str(trimmed);
        output.push('\n');
    }
    output.push_str(indent);
    output.push_str("# ");
    output.push_str(name);
    output.push_str(":end\n");
}

fn update_project_pin_stub(
    state: &mut ProjectState,
    package: Option<String>,
    url: Option<String>,
    rev: Option<String>,
    sha256: Option<String>,
    branch: Option<String>,
) -> Result<(), CliError> {
    let now = Utc::now();
    match package {
        None => {
            update_pin_fields(&mut state.pin, url, rev, sha256, branch);
            state.pin.updated = now.date_naive();
        }
        Some(name) => {
            let entry = state
                .packages
                .pinned
                .entry(name)
                .or_insert_with(|| PinnedPackage {
                    version: "CHANGEME".to_string(),
                    pin: state.pin.clone(),
                });
            update_pin_fields(&mut entry.pin, url, rev, sha256, branch);
            entry.pin.updated = now.date_naive();
        }
    }
    update_project_modified(state);
    Ok(())
}

fn update_profile_pin_stub(
    state: &mut GlobalProfileState,
    package: Option<String>,
    url: Option<String>,
    rev: Option<String>,
    sha256: Option<String>,
    branch: Option<String>,
) -> Result<(), CliError> {
    let now = Utc::now();
    match package {
        None => {
            update_pin_fields(&mut state.pin, url, rev, sha256, branch);
            state.pin.updated = now.date_naive();
        }
        Some(name) => {
            let entry = state
                .packages
                .pinned
                .entry(name)
                .or_insert_with(|| PinnedPackage {
                    version: "CHANGEME".to_string(),
                    pin: state.pin.clone(),
                });
            update_pin_fields(&mut entry.pin, url, rev, sha256, branch);
            entry.pin.updated = now.date_naive();
        }
    }
    update_profile_modified(state);
    Ok(())
}

fn project_nix_path() -> PathBuf {
    PathBuf::from("default.nix")
}

fn project_dir_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "dev-environment".to_string())
}

fn presets_path() -> PathBuf {
    Path::new("presets").to_path_buf()
}

fn ensure_pin_complete(pin: &Pin) -> Result<(), CliError> {
    if pin.rev.trim().is_empty() || pin.sha256.trim().is_empty() {
        return Err(CliError::IncompletePin);
    }
    if pin.rev == "CHANGEME" || pin.sha256 == "CHANGEME" {
        return Err(CliError::IncompletePin);
    }
    Ok(())
}

fn load_config_or_default() -> Result<Config, CliError> {
    let path = config_path()?;
    if path.exists() {
        Config::load_from_path(&path).map_err(CliError::Config)
    } else {
        Ok(Config::default())
    }
}

fn compute_added_packages(
    packages: Vec<String>,
    presets: &[String],
) -> Result<Vec<String>, CliError> {
    if presets.is_empty() {
        return Ok(packages);
    }
    let presets_def = load_all_presets()?;
    let mut preset_map = BTreeMap::new();
    for preset in presets_def {
        preset_map.insert(preset.name.clone(), preset);
    }
    let mut preset_packages = std::collections::BTreeSet::new();
    for name in presets {
        if let Some(preset) = preset_map.get(name) {
            for pkg in &preset.packages_required {
                preset_packages.insert(pkg.clone());
            }
        }
    }
    Ok(packages
        .into_iter()
        .filter(|pkg| !preset_packages.contains(pkg))
        .collect())
}

fn load_all_presets() -> Result<Vec<Preset>, CliError> {
    let config = load_config_or_default()?;
    let mut preset_map: BTreeMap<String, Preset> = BTreeMap::new();
    for preset in load_embedded_presets()? {
        preset_map.insert(preset.name.clone(), preset);
    }
    for preset in load_presets_from_dir(&presets_path())? {
        preset_map.insert(preset.name.clone(), preset);
    }
    for extra in config.presets.extra_dirs {
        let expanded = expand_tilde(&extra)?;
        for preset in load_presets_from_dir(&expanded)? {
            preset_map.insert(preset.name.clone(), preset);
        }
    }
    Ok(preset_map.into_values().collect())
}

fn expand_tilde(path: &str) -> Result<PathBuf, CliError> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(PathBuf::from(path))
}

fn ensure_config_dir() -> Result<(), CliError> {
    let path = config_dir()?;
    std::fs::create_dir_all(path).map_err(CliError::WriteNix)
}

fn config_dir() -> Result<PathBuf, CliError> {
    home_dir().map(|home| home.join(".config").join("mica"))
}

fn config_path() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("config.toml"))
}

fn profile_state_path() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("profile.toml"))
}

fn profile_nix_path() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("profile.nix"))
}

fn index_db_path() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("cache").join("index.db"))
}

fn home_dir() -> Result<PathBuf, CliError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| CliError::MissingHome)
}

fn print_project_state(state: &ProjectState) {
    println!("mode: project");
    println!("pin: {} @ {}", state.pin.url, state.pin.rev);
    if !state.pins.is_empty() {
        println!("pins:");
        for (name, pin) in &state.pins {
            println!("  {} -> {} ({})", name, pin.url, pin.rev);
        }
    }
    println!("presets: {}", state.presets.active.join(", "));
    println!("packages (added): {}", state.packages.added.join(", "));
    println!("packages (removed): {}", state.packages.removed.join(", "));
    if !state.packages.pinned.is_empty() {
        println!("packages (pinned):");
        for (name, pinned) in &state.packages.pinned {
            println!("  {} -> {} ({})", name, pinned.version, pinned.pin.rev);
        }
    }
    if !state.env.is_empty() {
        println!("env:");
        for (key, value) in &state.env {
            println!("  {}={}", key, value);
        }
    }
    if let Some(hook) = &state.shell.hook {
        println!("shellHook:");
        println!("{}", hook);
    }
}

fn print_profile_state(state: &GlobalProfileState) {
    println!("mode: global");
    println!("pin: {} @ {}", state.pin.url, state.pin.rev);
    println!("presets: {}", state.presets.active.join(", "));
    println!("packages (added): {}", state.packages.added.join(", "));
    println!("packages (removed): {}", state.packages.removed.join(", "));
    if !state.packages.pinned.is_empty() {
        println!("packages (pinned):");
        for (name, pinned) in &state.packages.pinned {
            println!("  {} -> {} ({})", name, pinned.version, pinned.pin.rev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_github_repo, CliError};

    #[test]
    fn parse_github_repo_https() {
        let (owner, repo) =
            parse_github_repo("https://github.com/jpetrucciani/nix").expect("parse failed");
        assert_eq!(owner, "jpetrucciani");
        assert_eq!(repo, "nix");
    }

    #[test]
    fn parse_github_repo_git_ssh() {
        let (owner, repo) =
            parse_github_repo("git@github.com:jpetrucciani/nix.git").expect("parse failed");
        assert_eq!(owner, "jpetrucciani");
        assert_eq!(repo, "nix");
    }

    #[test]
    fn parse_github_repo_rejects_non_github() {
        let result = parse_github_repo("https://example.com/jpetrucciani/nix");
        assert!(matches!(result, Err(CliError::InvalidGitHubUrl(_))));
    }
}
