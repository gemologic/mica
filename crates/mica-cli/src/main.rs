use chrono::{DateTime, Utc};
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
    GenerationEntry, GlobalProfileState, MicaMetadata, NixBlocks, Pin, PinnedPackage, PresetState,
    ProjectState, ShellState, NIX_EXPR_PREFIX,
};
use mica_index::generate::{
    get_meta, ingest_packages, init_db, list_packages, load_packages_from_json, open_db,
    search_packages_with_mode, set_meta, SearchMode as IndexSearchMode,
};
use mica_index::versions::{
    init_versions_db, latest_version_for_source, list_versions, open_versions_db, record_versions,
    version_for_commit, VersionSource,
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
    #[arg(
        short = 'f',
        long = "file",
        value_name = "PATH",
        conflicts_with = "dir",
        help = "Target specific nix file"
    )]
    file: Option<PathBuf>,
    #[arg(
        short = 'd',
        long = "dir",
        value_name = "PATH",
        conflicts_with = "file",
        help = "Target directory (uses default.nix)"
    )]
    dir: Option<PathBuf>,
    #[arg(
        short = 'n',
        long = "dry-run",
        help = "Show changes without writing files"
    )]
    dry_run: bool,
    #[arg(
        short = 'v',
        long = "verbose",
        help = "Increase verbosity",
        conflicts_with = "quiet"
    )]
    verbose: bool,
    #[arg(short = 'q', long = "quiet", help = "Suppress non-error output")]
    quiet: bool,
    #[command(subcommand)]
    command: Option<Command>,
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
    #[command(about = "List available presets")]
    Presets,
    #[command(about = "Add packages to environment")]
    Add { packages: Vec<String> },
    #[command(about = "Remove packages from environment")]
    Remove { packages: Vec<String> },
    #[command(about = "Search packages (index required)")]
    Search {
        query: String,
        #[arg(
            long,
            value_enum,
            help = "Search mode (name, description, binary, all)"
        )]
        mode: Option<SearchModeArg>,
    },
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
    #[command(about = "Update nixpkgs pin")]
    Update {
        #[arg(help = "Optional package name for version pinning")]
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
    #[command(about = "Manage global generations")]
    Generations {
        #[command(subcommand)]
        command: GenerationsCommand,
    },
    #[command(about = "Output standalone nix file to stdout")]
    Export,
    #[command(about = "Manage package index")]
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    #[command(about = "Regenerate nix file from state")]
    Sync {
        #[arg(long, help = "Update state from existing nix file (limited parsing)")]
        from_nix: bool,
    },
    #[command(about = "Validate current configuration")]
    Eval,
    #[command(about = "Check for drift between state and nix file")]
    Diff,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum SearchModeArg {
    Name,
    Description,
    Binary,
    All,
}

impl SearchModeArg {
    fn to_search_mode(self) -> mica_core::config::SearchMode {
        match self {
            SearchModeArg::Name => mica_core::config::SearchMode::Name,
            SearchModeArg::Description => mica_core::config::SearchMode::Description,
            SearchModeArg::Binary => mica_core::config::SearchMode::Binary,
            SearchModeArg::All => mica_core::config::SearchMode::All,
        }
    }
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
enum GenerationsCommand {
    #[command(about = "List generations")]
    List,
    #[command(about = "Rollback to a generation (defaults to previous)")]
    Rollback { id: Option<u64> },
}

#[derive(Debug, Subcommand)]
enum IndexCommand {
    #[command(about = "Show index status")]
    Status,
    #[command(about = "Rebuild local index from nix-env json")]
    Rebuild {
        #[arg(help = "Path to nix-env -qaP --json output")]
        input: PathBuf,
        #[arg(long, help = "Output path for the index db")]
        output: Option<PathBuf>,
    },
    #[command(about = "Evaluate a local nix repo and rebuild index")]
    RebuildLocal {
        #[arg(help = "Path to local nix repo root")]
        repo: PathBuf,
        #[arg(long, help = "Output path for the index db")]
        output: Option<PathBuf>,
        #[arg(
            long = "skip-attr",
            value_name = "ATTR_OR_GLOB",
            value_delimiter = ',',
            help = "Extra attrs/globs to skip (in addition to defaults and MICA_NIX_SKIP_ATTRS)"
        )]
        skip_attr: Vec<String>,
        #[arg(long, help = "Enable --show-trace for nix evaluation")]
        show_trace: bool,
    },
    #[command(about = "Fetch remote index")]
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
    #[error("--file/--dir are not supported with --global")]
    InvalidGlobalTarget,
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
    #[error("remote index fetch failed ({0}): {1}")]
    RemoteIndexFailed(reqwest::StatusCode, String),
    #[error("generation history is empty")]
    NoGenerations,
    #[error("generation {0} not found")]
    GenerationNotFound(u64),
    #[error("generation snapshot missing at {0}")]
    GenerationSnapshotMissing(PathBuf),
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
    #[error("github api response missing default branch")]
    GitHubApiMissingDefaultBranch,
    #[error("github api response missing commit date")]
    GitHubApiMissingDate,
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
    #[error("nix-instantiate not found in PATH, install Nix to run eval")]
    MissingNixInstantiate,
    #[error("nix-instantiate failed: {0}")]
    NixInstantiateFailed(String),
    #[error("nix-build not found in PATH, install Nix to run eval")]
    MissingNixBuild,
    #[error("nix-build failed: {0}")]
    NixBuildFailed(String),
    #[error("failed to create temp nix file: {0}")]
    TempNixFile(std::io::Error),
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
    #[serde(default)]
    commit: GitHubCommitInfo,
}

#[derive(Debug, Deserialize, Default)]
struct GitHubCommitInfo {
    #[serde(default)]
    author: Option<GitHubCommitAuthor>,
    #[serde(default)]
    committer: Option<GitHubCommitAuthor>,
}

#[derive(Debug, Deserialize)]
struct GitHubCommitAuthor {
    date: String,
}

#[derive(Debug, Deserialize, Default)]
struct GitHubRepoInfo {
    #[serde(default)]
    default_branch: String,
}

#[derive(Debug, Clone, Copy)]
struct Output {
    quiet: bool,
    verbose: bool,
}

impl Output {
    fn info(&self, message: impl AsRef<str>) {
        if !self.quiet {
            println!("{}", message.as_ref());
        }
    }

    fn status(&self, message: impl AsRef<str>) {
        if !self.quiet {
            eprintln!("{}", message.as_ref());
        }
    }

    fn warn(&self, message: impl AsRef<str>) {
        if !self.quiet {
            eprintln!("{}", message.as_ref());
        }
    }

    fn verbose(&self, message: impl AsRef<str>) {
        if self.verbose && !self.quiet {
            eprintln!("{}", message.as_ref());
        }
    }
}

#[derive(Debug, Clone)]
struct ProjectPaths {
    nix_path: PathBuf,
    root_dir: PathBuf,
}

impl ProjectPaths {
    fn new(file: Option<PathBuf>, dir: Option<PathBuf>) -> Result<Self, CliError> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match (file, dir) {
            (Some(file), None) => {
                let nix_path = if file.is_absolute() {
                    file
                } else {
                    cwd.join(file)
                };
                let parent = nix_path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| cwd.clone());
                let root_dir = std::fs::canonicalize(&parent).unwrap_or(parent);
                Ok(ProjectPaths { nix_path, root_dir })
            }
            (None, Some(dir)) => {
                let root = if dir.is_absolute() {
                    dir
                } else {
                    cwd.join(dir)
                };
                let root_dir = std::fs::canonicalize(&root).unwrap_or(root);
                let nix_path = root_dir.join("default.nix");
                Ok(ProjectPaths { nix_path, root_dir })
            }
            (None, None) => {
                let root_dir = std::fs::canonicalize(&cwd).unwrap_or(cwd);
                let nix_path = root_dir.join("default.nix");
                Ok(ProjectPaths { nix_path, root_dir })
            }
            _ => Ok(ProjectPaths {
                nix_path: cwd.join("default.nix"),
                root_dir: cwd,
            }),
        }
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{}", err);
        std::process::exit(1);
    }
}

fn run() -> Result<(), CliError> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Tui);
    let output = Output {
        quiet: cli.quiet,
        verbose: cli.verbose,
    };
    if cli.global && (cli.file.is_some() || cli.dir.is_some()) {
        return Err(CliError::InvalidGlobalTarget);
    }
    let project_paths = if cli.global {
        None
    } else {
        Some(ProjectPaths::new(cli.file.clone(), cli.dir.clone())?)
    };

    match command {
        Command::Tui => {
            if cli.dry_run {
                output.info("dry-run ignored for TUI");
            }
            run_tui(cli.global, project_paths.as_ref(), &output)
        }
        Command::Init { repo } => {
            if cli.global {
                if cli.dry_run {
                    let state = build_initial_profile_state(repo)?;
                    output.info(format!(
                        "dry-run: would initialize {}",
                        profile_state_path()?.display()
                    ));
                    if output.verbose {
                        output.info(build_profile_nix(&state)?);
                    }
                } else {
                    init_profile_state(repo)?;
                    let state = load_profile_state()?;
                    sync_and_install_profile(&output, &state)?;
                }
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                if cli.dry_run {
                    if paths.nix_path.exists() {
                        return Err(CliError::StateExists(paths.nix_path.to_path_buf()));
                    }
                    let state = build_initial_project_state(repo)?;
                    output.info(format!(
                        "dry-run: would initialize {}",
                        paths.nix_path.display()
                    ));
                    if output.verbose {
                        output.info(build_project_nix(paths, &state)?);
                    }
                } else {
                    init_project_state(paths, repo)?;
                }
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
                apply_profile_changes(&output, cli.dry_run, &state)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
                for pkg in packages {
                    if !state.packages.added.contains(&pkg) {
                        state.packages.added.push(pkg.clone());
                    }
                    state.packages.removed.retain(|item| item != &pkg);
                }
                update_project_modified(&mut state);
                apply_project_changes(&output, paths, cli.dry_run, &state)?;
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
                apply_profile_changes(&output, cli.dry_run, &state)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
                for pkg in packages {
                    if !state.packages.removed.contains(&pkg) {
                        state.packages.removed.push(pkg.clone());
                    }
                    state.packages.added.retain(|item| item != &pkg);
                }
                update_project_modified(&mut state);
                apply_project_changes(&output, paths, cli.dry_run, &state)?;
            }
            Ok(())
        }
        Command::Search { query, mode } => {
            let index_path = index_db_path()?;
            if !index_path.exists() {
                return Err(CliError::MissingIndex(index_path));
            }
            let conn = open_db(&index_path)?;
            let config = load_config_or_default()?;
            let search_mode = mode
                .map(|mode| mode.to_search_mode())
                .unwrap_or(config.tui.search_mode);
            let results =
                search_packages_with_mode(&conn, &query, 25, to_index_search_mode(&search_mode))?;
            for pkg in results {
                let version = pkg.version.unwrap_or_else(|| "-".to_string());
                let description = pkg.description.unwrap_or_default();
                output.info(format!(
                    "{} {} {}",
                    normalize_attr_path(&pkg.attr_path),
                    version,
                    description
                ));
            }
            Ok(())
        }
        Command::Env { command } => {
            if cli.global {
                output.info("env is only supported in project mode for now");
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
                match command {
                    EnvCommand::Set { key, value } => {
                        state.env.insert(key, value);
                    }
                    EnvCommand::Unset { key } => {
                        state.env.remove(&key);
                    }
                }
                update_project_modified(&mut state);
                apply_project_changes(&output, paths, cli.dry_run, &state)?;
            }
            Ok(())
        }
        Command::Shell { command } => {
            if cli.global {
                output.info("shell hook is only supported in project mode for now");
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
                match command {
                    ShellCommand::Set { content } => {
                        state.shell.hook = Some(content);
                    }
                    ShellCommand::Clear => {
                        state.shell.hook = None;
                    }
                }
                update_project_modified(&mut state);
                apply_project_changes(&output, paths, cli.dry_run, &state)?;
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
                apply_profile_changes(&output, cli.dry_run, &state)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
                for preset in presets {
                    if !state.presets.active.contains(&preset) {
                        state.presets.active.push(preset);
                    }
                }
                update_project_modified(&mut state);
                apply_project_changes(&output, paths, cli.dry_run, &state)?;
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
                apply_profile_changes(&output, cli.dry_run, &state)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
                state
                    .presets
                    .active
                    .retain(|preset| !presets.contains(preset));
                update_project_modified(&mut state);
                apply_project_changes(&output, paths, cli.dry_run, &state)?;
            }
            Ok(())
        }
        Command::List => {
            if cli.global {
                let state = load_profile_state()?;
                print_profile_state(&output, &state);
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let state = load_project_state(paths)?;
                print_project_state(&output, &state);
            }
            Ok(())
        }
        Command::Presets => {
            let mut presets = load_all_presets()?;
            presets.sort_by(|left, right| {
                left.order
                    .cmp(&right.order)
                    .then_with(|| left.name.cmp(&right.name))
            });

            for preset in presets {
                let description = preset.description.trim();
                if description.is_empty() {
                    output.info(format!(
                        "{} [order:{} req:{} opt:{}] {}",
                        preset.name,
                        preset.order,
                        preset.packages_required.len(),
                        preset.packages_optional.len(),
                        preset.source.display()
                    ));
                } else {
                    output.info(format!(
                        "{} [order:{} req:{} opt:{}] {} - {}",
                        preset.name,
                        preset.order,
                        preset.packages_required.len(),
                        preset.packages_optional.len(),
                        preset.source.display(),
                        description
                    ));
                }
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
                apply_profile_changes(&output, cli.dry_run, &state)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
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
                apply_project_changes(&output, paths, cli.dry_run, &state)?;
            }
            Ok(())
        }
        Command::Pin { command } => {
            if cli.global {
                output.info("pins are only supported in project mode for now");
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
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
                        apply_project_changes(&output, paths, cli.dry_run, &state)?;
                    }
                    PinCommand::Remove { name } => {
                        if state.pins.remove(&name).is_none() {
                            return Err(CliError::PinNotFound(name));
                        }
                        update_project_modified(&mut state);
                        apply_project_changes(&output, paths, cli.dry_run, &state)?;
                    }
                    PinCommand::List => {
                        if state.pins.is_empty() {
                            output.info("no extra pins configured");
                        } else {
                            for (name, pin) in &state.pins {
                                output.info(format!("{} -> {} @ {}", name, pin.url, pin.rev));
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Generations { command } => {
            if !cli.global {
                output.info("generations are only available in global mode");
                return Ok(());
            }
            match command {
                GenerationsCommand::List => {
                    let state = load_profile_state()?;
                    list_generations(&output, &state)?;
                }
                GenerationsCommand::Rollback { id } => {
                    rollback_generation(&output, id, cli.dry_run)?;
                }
            }
            Ok(())
        }
        Command::Export => {
            if cli.global {
                let state = load_profile_state()?;
                let generated = build_profile_nix(&state)?;
                let formatted = format_mica_nix(&generated);
                io::stdout()
                    .write_all(formatted.as_bytes())
                    .map_err(CliError::WriteNix)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let state = load_project_state(paths)?;
                let generated = build_project_nix(paths, &state)?;
                let formatted = format_mica_nix(&generated);
                io::stdout()
                    .write_all(formatted.as_bytes())
                    .map_err(CliError::WriteNix)?;
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
                        output.info(format!("index: {}", index_path.display()));
                        output.info("meta: empty");
                    } else {
                        output.info(format!("index: {}", index_path.display()));
                        for (key, value) in meta {
                            output.info(format!("{}: {}", key, value));
                        }
                    }
                }
                IndexCommand::Rebuild {
                    input,
                    output: output_path_override,
                } => {
                    if cli.dry_run {
                        output.info("dry-run: skipping index rebuild");
                        return Ok(());
                    }
                    let output_path = output_path_override.unwrap_or(index_db_path()?);
                    let pin = if cli.global {
                        load_profile_state().ok().map(|state| state.pin)
                    } else {
                        project_paths
                            .as_ref()
                            .and_then(|paths| load_project_state(paths).ok().map(|state| state.pin))
                    };
                    let count =
                        rebuild_index_from_json(&output, &input, &output_path, pin.as_ref())?;
                    output.info(format!("indexed {} packages", count));
                }
                IndexCommand::RebuildLocal {
                    repo,
                    output: output_path_override,
                    skip_attr,
                    show_trace,
                } => {
                    if cli.dry_run {
                        output.info("dry-run: skipping local index rebuild");
                        return Ok(());
                    }
                    let output_path = output_path_override.unwrap_or(index_db_path()?);
                    let count = rebuild_index_from_local_repo_with_spinner(
                        &output,
                        &repo,
                        &output_path,
                        &skip_attr,
                        show_trace,
                    )?;
                    output.info(format!("indexed {} packages", count));
                }
                IndexCommand::Fetch => {
                    if cli.dry_run {
                        output.info("dry-run: skipping index fetch");
                        return Ok(());
                    }
                    let config = load_config_or_default()?;
                    if config.index.remote_url.trim().is_empty() {
                        return Err(CliError::MissingRemoteIndex);
                    }
                    let index_path = index_db_path()?;
                    let pins = if cli.global {
                        load_profile_state()
                            .ok()
                            .map(|state| collect_index_pins_profile(&state))
                    } else {
                        project_paths.as_ref().and_then(|paths| {
                            load_project_state(paths)
                                .ok()
                                .map(|state| collect_index_pins(&state))
                        })
                    };
                    let fetched = try_fetch_remote_index(
                        &output,
                        &config.index.remote_url,
                        &index_path,
                        pins.as_ref().and_then(|entries| primary_pin_rev(entries)),
                    )?;
                    if !fetched {
                        let Some(pins) = pins.as_ref() else {
                            return Err(CliError::RemoteIndexFailed(
                                reqwest::StatusCode::NOT_FOUND,
                                "no remote index found and no local state available for rebuild"
                                    .to_string(),
                            ));
                        };
                        output.status("remote index unavailable, rebuilding locally");
                        let count =
                            rebuild_index_from_pins_with_spinner(&output, &index_path, pins)?;
                        output.info(format!("indexed {} packages", count));
                    }
                    output.info(format!("index fetched to {}", index_path.display()));
                    if let Ok(conn) = open_db(&index_path) {
                        if let Ok(meta) = get_meta(&conn) {
                            for (key, value) in meta {
                                output.info(format!("{}: {}", key, value));
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        Command::Sync { from_nix } => {
            if cli.global {
                let mut state = load_profile_state()?;
                if from_nix {
                    update_profile_state_from_nix(&mut state)?;
                }
                apply_profile_changes(&output, cli.dry_run, &state)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let mut state = load_project_state(paths)?;
                if from_nix {
                    update_project_state_from_nix(paths, &mut state)?;
                }
                apply_project_changes(&output, paths, cli.dry_run, &state)?;
            }
            Ok(())
        }
        Command::Eval => {
            if cli.global {
                let state = load_profile_state()?;
                let generated = build_profile_nix(&state)?;
                eval_nix_contents(&output, &generated)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let state = load_project_state(paths)?;
                let generated = build_project_nix(paths, &state)?;
                eval_nix_contents(&output, &generated)?;
            }
            Ok(())
        }
        Command::Diff => {
            if cli.global {
                let state = load_profile_state()?;
                diff_profile(&output, &state)?;
            } else {
                let paths = project_paths.as_ref().expect("project paths missing");
                let state = load_project_state(paths)?;
                diff_project(&output, paths, &state)?;
            }
            Ok(())
        }
    }
}

fn run_tui(
    global: bool,
    project_paths: Option<&ProjectPaths>,
    output: &Output,
) -> Result<(), CliError> {
    if global {
        run_tui_global(output)
    } else {
        let paths = project_paths.expect("project paths missing");
        run_tui_project(paths, output)
    }
}

fn run_tui_project(paths: &ProjectPaths, output: &Output) -> Result<(), CliError> {
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;
    use tui::app::App;

    let project_path = &paths.nix_path;
    if !project_path.exists() {
        output.status(format!(
            "default.nix missing at {}, initializing",
            project_path.display()
        ));
        init_project_state(paths, None)?;
    }
    let mut state = load_project_state(paths)?;
    let config = load_config_or_default().ok();
    let index_path = index_db_path()?;
    if !index_path.exists() {
        let pins = collect_index_pins(&state);
        let fetched = try_fetch_remote_index_for_pins(output, config.as_ref(), &index_path, &pins)?;
        if !fetched {
            output.status(format!(
                "index missing at {}, building from nix-env -qaP --json",
                index_path.display()
            ));
            let count = rebuild_index_from_pins_with_spinner(output, &index_path, &pins)?;
            output.status(format!("index ready, {} packages", count));
        }
    }
    if let Some(config) = &config {
        let pins = collect_index_pins(&state);
        let _ = maybe_refresh_remote_index(output, config, &index_path, primary_pin_rev(&pins))?;
    }

    let mut conn = open_db(&index_path)?;
    let mut meta = get_meta(&conn).unwrap_or_default();
    let mut has_meta = meta_has_key(&meta, "index_meta");
    if has_meta && !index_has_descriptions(&conn)? {
        has_meta = false;
    }
    if !has_meta {
        let pins = collect_index_pins(&state);
        let fetched = try_fetch_remote_index_for_pins(output, config.as_ref(), &index_path, &pins)?;
        if fetched {
            conn = open_db(&index_path)?;
            meta = get_meta(&conn).unwrap_or_default();
            has_meta = meta_has_key(&meta, "index_meta");
            if has_meta && !index_has_descriptions(&conn)? {
                has_meta = false;
            }
        }
        if !has_meta {
            output.status("index missing metadata, rebuilding from nix-env -qaP --json --meta");
            let count = rebuild_index_from_pins_with_spinner(output, &index_path, &pins)?;
            output.status(format!("index ready, {} packages", count));
            conn = open_db(&index_path)?;
            meta = get_meta(&conn).unwrap_or_default();
        }
    }
    let presets = load_tui_presets()?;
    let mut app = App::new(Vec::new(), presets);
    app.mode = tui::app::AppMode::Project;
    app.project_dir = Some(paths.root_dir.to_string_lossy().to_string());
    if let Some(config) = &config {
        apply_columns_from_config(&mut app, config);
        apply_search_mode_from_config(&mut app, config);
        apply_show_details_from_config(&mut app, config);
    }
    let pins = collect_index_pins(&state);
    app.index_info = index_info_with_pin_fallback(index_info_from_meta(meta), &pins);
    apply_state_to_app(&mut app, &state);
    update_search_results(&conn, &mut app)?;
    app.refresh_preset_filter();

    enable_raw_mode().map_err(CliError::WriteNix)?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).map_err(CliError::WriteNix)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(CliError::WriteNix)?;

    let result = run_tui_loop_project(
        &mut terminal,
        &mut app,
        &mut state,
        paths,
        &index_path,
        &mut conn,
        output,
    );

    disable_raw_mode().map_err(CliError::WriteNix)?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .map_err(CliError::WriteNix)?;
    terminal.show_cursor().map_err(CliError::WriteNix)?;
    result
}

fn run_tui_global(output: &Output) -> Result<(), CliError> {
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;
    use tui::app::App;

    let profile_state = profile_state_path()?;
    if !profile_state.exists() {
        output.status(format!(
            "global profile missing at {}, initializing",
            profile_state.display()
        ));
        init_profile_state(None)?;
        let state = load_profile_state()?;
        sync_and_install_profile(output, &state)?;
    }
    let mut state = load_profile_state()?;
    let profile_nix = profile_nix_path()?;
    if !profile_nix.exists() {
        sync_profile_nix(&state)?;
    }

    let config = load_config_or_default().ok();
    let index_path = index_db_path()?;
    if !index_path.exists() {
        let pins = collect_index_pins_profile(&state);
        let fetched = try_fetch_remote_index_for_pins(output, config.as_ref(), &index_path, &pins)?;
        if !fetched {
            output.status(format!(
                "index missing at {}, building from nix-env -qaP --json",
                index_path.display()
            ));
            let count = rebuild_index_from_pins_with_spinner(output, &index_path, &pins)?;
            output.status(format!("index ready, {} packages", count));
        }
    }
    if let Some(config) = &config {
        let pins = collect_index_pins_profile(&state);
        let _ = maybe_refresh_remote_index(output, config, &index_path, primary_pin_rev(&pins))?;
    }

    let mut conn = open_db(&index_path)?;
    let mut meta = get_meta(&conn).unwrap_or_default();
    let mut has_meta = meta_has_key(&meta, "index_meta");
    if has_meta && !index_has_descriptions(&conn)? {
        has_meta = false;
    }
    if !has_meta {
        let pins = collect_index_pins_profile(&state);
        let fetched = try_fetch_remote_index_for_pins(output, config.as_ref(), &index_path, &pins)?;
        if fetched {
            conn = open_db(&index_path)?;
            meta = get_meta(&conn).unwrap_or_default();
            has_meta = meta_has_key(&meta, "index_meta");
            if has_meta && !index_has_descriptions(&conn)? {
                has_meta = false;
            }
        }
        if !has_meta {
            output.status("index missing metadata, rebuilding from nix-env -qaP --json --meta");
            let count = rebuild_index_from_pins_with_spinner(output, &index_path, &pins)?;
            output.status(format!("index ready, {} packages", count));
            conn = open_db(&index_path)?;
            meta = get_meta(&conn).unwrap_or_default();
        }
    }

    let presets = load_tui_presets()?;
    let mut app = App::new(Vec::new(), presets);
    app.mode = tui::app::AppMode::Global;
    if let Some(config) = &config {
        apply_columns_from_config(&mut app, config);
        apply_search_mode_from_config(&mut app, config);
        apply_show_details_from_config(&mut app, config);
    }
    let pins = collect_index_pins_profile(&state);
    app.index_info = index_info_with_pin_fallback(index_info_from_meta(meta), &pins);
    apply_profile_state_to_app(&mut app, &state);
    update_search_results(&conn, &mut app)?;
    app.refresh_preset_filter();

    enable_raw_mode().map_err(CliError::WriteNix)?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).map_err(CliError::WriteNix)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(CliError::WriteNix)?;

    let result = run_tui_loop_global(
        &mut terminal,
        &mut app,
        &mut state,
        &index_path,
        &mut conn,
        output,
    );

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
    paths: &ProjectPaths,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
    output: &Output,
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
                    if let Err(err) = handle_overlay_key(
                        key, terminal, app, state, paths, index_path, conn, output,
                    ) {
                        app.push_toast(tui::app::ToastLevel::Error, err.to_string());
                    }
                } else if let Err(err) =
                    handle_main_key(key, terminal, app, state, paths, index_path, conn, output)
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

fn run_tui_loop_global(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    state: &mut GlobalProfileState,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
    output: &Output,
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
                    if let Err(err) = handle_overlay_key_global(key, terminal, app, conn, output) {
                        app.push_toast(tui::app::ToastLevel::Error, err.to_string());
                    }
                } else if let Err(err) =
                    handle_main_key_global(key, terminal, app, state, index_path, conn, output)
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

#[allow(clippy::too_many_arguments)]
fn handle_main_key(
    key: KeyEvent,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    state: &mut ProjectState,
    paths: &ProjectPaths,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
    output: &Output,
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
            save_tui_selection(paths, state, app)?;
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
        InputAction::ToggleSearchMode => {
            app.cycle_search_mode();
            if let Err(err) = save_search_mode_to_config(&app.search_mode) {
                app.push_toast(tui::app::ToastLevel::Error, err.to_string());
            }
            update_search_results(conn, app)?;
            app.push_toast(
                tui::app::ToastLevel::Info,
                format!("Search mode: {}", app.search_mode_label()),
            );
        }
        InputAction::ToggleDetails => {
            app.show_details = !app.show_details;
            if let Err(err) = save_show_details_to_config(app.show_details) {
                app.push_toast(tui::app::ToastLevel::Error, err.to_string());
            }
            app.push_toast(
                tui::app::ToastLevel::Info,
                format!(
                    "Details panel: {}",
                    if app.show_details { "on" } else { "off" }
                ),
            );
        }
        InputAction::EditLicenseFilter => open_filter_overlay(app, FilterKind::License),
        InputAction::EditPlatformFilter => open_filter_overlay(app, FilterKind::Platform),
        InputAction::PreviewDiff => {
            app.overlay = Some(build_diff_overlay(paths, state, app)?);
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
        InputAction::OpenVersionPicker => {
            if app.focus != Focus::Packages {
                app.push_toast(
                    tui::app::ToastLevel::Info,
                    "Focus packages to view versions",
                );
            } else {
                match build_version_picker_overlay(app) {
                    Ok(Some(overlay)) => app.overlay = Some(overlay),
                    Ok(None) => app.push_toast(
                        tui::app::ToastLevel::Info,
                        "No version history for selection",
                    ),
                    Err(err) => app.push_toast(tui::app::ToastLevel::Error, err.to_string()),
                }
            }
        }
        InputAction::UpdatePin => {
            with_tui_suspended(terminal, || {
                let rev = run_with_spinner(output, "fetching latest nixpkgs revision", || {
                    fetch_latest_github_rev(&state.pin.url, &state.pin.branch)
                })?;
                let sha256 = run_with_spinner(output, "prefetching nixpkgs tarball", || {
                    fetch_nix_sha256(&state.pin.url, &rev)
                })?;
                state.pin.rev = rev;
                state.pin.sha256 = sha256;
                state.pin.updated = Utc::now().date_naive();
                update_project_modified(state);
                save_project_state(paths, state)?;
                let pins = collect_index_pins(state);
                let config = load_config_or_default().ok();
                let fetched =
                    try_fetch_remote_index_for_pins(output, config.as_ref(), index_path, &pins)?;
                if !fetched {
                    rebuild_index_from_pins_with_spinner(output, index_path, &pins)?;
                }
                Ok(())
            })?;
            *conn = open_db(index_path)?;
            let pins = collect_index_pins(state);
            app.index_info = index_info_with_pin_fallback(
                index_info_from_meta(get_meta(conn).unwrap_or_default()),
                &pins,
            );
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
                let config = load_config_or_default().ok();
                let fetched =
                    try_fetch_remote_index_for_pins(output, config.as_ref(), index_path, &pins)?;
                if !fetched {
                    rebuild_index_from_pins_with_spinner(output, index_path, &pins)?;
                }
                Ok(())
            })?;
            *conn = open_db(index_path)?;
            let pins = collect_index_pins(state);
            app.index_info = index_info_with_pin_fallback(
                index_info_from_meta(get_meta(conn).unwrap_or_default()),
                &pins,
            );
            update_search_results(conn, app)?;
            app.push_toast(tui::app::ToastLevel::Info, "Index rebuilt");
        }
        InputAction::Sync => {
            update_project_state_from_nix(paths, state)?;
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
    output: &Output,
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
            with_tui_suspended(terminal, || save_profile_tui_selection(output, state, app))?;
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
        InputAction::ToggleSearchMode => {
            app.cycle_search_mode();
            if let Err(err) = save_search_mode_to_config(&app.search_mode) {
                app.push_toast(tui::app::ToastLevel::Error, err.to_string());
            }
            update_search_results(conn, app)?;
            app.push_toast(
                tui::app::ToastLevel::Info,
                format!("Search mode: {}", app.search_mode_label()),
            );
        }
        InputAction::ToggleDetails => {
            app.show_details = !app.show_details;
            if let Err(err) = save_show_details_to_config(app.show_details) {
                app.push_toast(tui::app::ToastLevel::Error, err.to_string());
            }
            app.push_toast(
                tui::app::ToastLevel::Info,
                format!(
                    "Details panel: {}",
                    if app.show_details { "on" } else { "off" }
                ),
            );
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
        InputAction::OpenVersionPicker => {
            if app.focus != Focus::Packages {
                app.push_toast(
                    tui::app::ToastLevel::Info,
                    "Focus packages to view versions",
                );
            } else {
                match build_version_picker_overlay(app) {
                    Ok(Some(overlay)) => app.overlay = Some(overlay),
                    Ok(None) => app.push_toast(
                        tui::app::ToastLevel::Info,
                        "No version history for selection",
                    ),
                    Err(err) => app.push_toast(tui::app::ToastLevel::Error, err.to_string()),
                }
            }
        }
        InputAction::UpdatePin => {
            with_tui_suspended(terminal, || {
                let rev = run_with_spinner(output, "fetching latest nixpkgs revision", || {
                    fetch_latest_github_rev(&state.pin.url, &state.pin.branch)
                })?;
                let sha256 = run_with_spinner(output, "prefetching nixpkgs tarball", || {
                    fetch_nix_sha256(&state.pin.url, &rev)
                })?;
                state.pin.rev = rev;
                state.pin.sha256 = sha256;
                state.pin.updated = Utc::now().date_naive();
                update_profile_modified(state);
                save_profile_state(state)?;
                sync_and_install_profile(output, state)?;
                let pins = collect_index_pins_profile(state);
                let config = load_config_or_default().ok();
                let fetched =
                    try_fetch_remote_index_for_pins(output, config.as_ref(), index_path, &pins)?;
                if !fetched {
                    rebuild_index_from_pins_with_spinner(output, index_path, &pins)?;
                }
                Ok(())
            })?;
            *conn = open_db(index_path)?;
            let pins = collect_index_pins_profile(state);
            app.index_info = index_info_with_pin_fallback(
                index_info_from_meta(get_meta(conn).unwrap_or_default()),
                &pins,
            );
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
                let config = load_config_or_default().ok();
                let fetched =
                    try_fetch_remote_index_for_pins(output, config.as_ref(), index_path, &pins)?;
                if !fetched {
                    rebuild_index_from_pins_with_spinner(output, index_path, &pins)?;
                }
                Ok(())
            })?;
            *conn = open_db(index_path)?;
            let pins = collect_index_pins_profile(state);
            app.index_info = index_info_with_pin_fallback(
                index_info_from_meta(get_meta(conn).unwrap_or_default()),
                &pins,
            );
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

#[allow(clippy::too_many_arguments)]
fn handle_overlay_key(
    key: KeyEvent,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    state: &mut ProjectState,
    paths: &ProjectPaths,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
    output: &Output,
) -> Result<(), CliError> {
    use tui::app::{EnvEditMode, EnvValueMode, Overlay};

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
        Overlay::VersionPicker(mut state) => {
            let mut close = false;
            let max = state.entries.len().saturating_sub(1);
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
                KeyCode::Enter => {
                    if let Some(entry) = state.entries.get(state.cursor).cloned() {
                        let package = state.package.clone();
                        with_tui_suspended(terminal, || {
                            apply_version_selection(output, app, &package, entry)
                        })?;
                        close = true;
                    }
                }
                _ => {}
            }
            if !close {
                app.overlay = Some(Overlay::VersionPicker(state));
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
                    if submit_pin_editor(
                        terminal,
                        app,
                        &mut editor,
                        state,
                        paths,
                        index_path,
                        conn,
                        output,
                    ) {
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
            if matches!(state.mode, EnvEditMode::List) {
                match key.code {
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
                        state.mode = EnvEditMode::Edit {
                            original_key: None,
                            value_mode: EnvValueMode::String,
                        };
                        state.input.clear();
                        state.input_cursor = 0;
                        state.error = None;
                    }
                    KeyCode::Enter => {
                        if let Some(entry) = state.entries.get(state.cursor) {
                            state.input =
                                format!("{}={}", entry.key, env_value_for_editor(&entry.value));
                            state.input_cursor = state.input.len();
                            state.mode = EnvEditMode::Edit {
                                original_key: Some(entry.key.clone()),
                                value_mode: env_value_mode_from_stored(&entry.value),
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
                }
            } else {
                match key.code {
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
                    KeyCode::Tab => {
                        if let EnvEditMode::Edit { value_mode, .. } = &mut state.mode {
                            *value_mode = value_mode.toggle();
                            state.error = None;
                        }
                    }
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
                }
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
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    conn: &rusqlite::Connection,
    output: &Output,
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
        Overlay::VersionPicker(mut state) => {
            let mut close = false;
            let max = state.entries.len().saturating_sub(1);
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
                KeyCode::Enter => {
                    if let Some(entry) = state.entries.get(state.cursor).cloned() {
                        let package = state.package.clone();
                        with_tui_suspended(terminal, || {
                            apply_version_selection(output, app, &package, entry)
                        })?;
                        close = true;
                    }
                }
                _ => {}
            }
            if !close {
                app.overlay = Some(Overlay::VersionPicker(state));
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

#[allow(clippy::too_many_arguments)]
fn submit_pin_editor(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut tui::app::App,
    editor: &mut tui::app::PinEditorState,
    state: &mut ProjectState,
    paths: &ProjectPaths,
    index_path: &Path,
    conn: &mut rusqlite::Connection,
    output: &Output,
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
        save_project_state(paths, state)?;
        let pins = collect_index_pins(state);
        let config = load_config_or_default().ok();
        let fetched = try_fetch_remote_index_for_pins(output, config.as_ref(), index_path, &pins)?;
        if !fetched {
            rebuild_index_from_pins_with_spinner(output, index_path, &pins)?;
        }
        Ok(())
    }) {
        editor.error = Some(err.to_string());
        return false;
    }

    match open_db(index_path) {
        Ok(new_conn) => {
            *conn = new_conn;
            let pins = collect_index_pins(state);
            app.index_info = index_info_with_pin_fallback(
                index_info_from_meta(get_meta(conn).unwrap_or_default()),
                &pins,
            );
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
        search_packages_with_mode(
            conn,
            query,
            limit + 1,
            to_index_search_mode(&app.search_mode),
        )?
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
    app.pinned = state.packages.pinned.clone();
    app.env = state.env.clone();
    app.shell_hook = state.shell.hook.clone();
    apply_pin_map_to_app(app, &collect_index_pins(state));
    app.rebuild_preset_packages();
    app.commit_baseline();
}

fn apply_profile_state_to_app(app: &mut tui::app::App, state: &GlobalProfileState) {
    app.added = state.packages.added.iter().cloned().collect();
    app.removed = state.packages.removed.iter().cloned().collect();
    app.active_presets = state.presets.active.iter().cloned().collect();
    app.pinned = state.packages.pinned.clone();
    app.env.clear();
    app.shell_hook = None;
    apply_pin_map_to_app(app, &collect_index_pins_profile(state));
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

fn apply_search_mode_from_config(app: &mut tui::app::App, config: &Config) {
    app.search_mode = config.tui.search_mode.clone();
}

fn apply_show_details_from_config(app: &mut tui::app::App, config: &Config) {
    app.show_details = config.tui.show_details;
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

fn save_search_mode_to_config(mode: &mica_core::config::SearchMode) -> Result<(), CliError> {
    ensure_config_dir()?;
    let mut config = load_config_or_default()?;
    config.tui.search_mode = mode.clone();
    config
        .save_to_path(&config_path()?)
        .map_err(CliError::Config)
}

fn save_show_details_to_config(show_details: bool) -> Result<(), CliError> {
    ensure_config_dir()?;
    let mut config = load_config_or_default()?;
    config.tui.show_details = show_details;
    config
        .save_to_path(&config_path()?)
        .map_err(CliError::Config)
}

fn to_index_search_mode(mode: &mica_core::config::SearchMode) -> IndexSearchMode {
    match mode {
        mica_core::config::SearchMode::Name => IndexSearchMode::Name,
        mica_core::config::SearchMode::Description => IndexSearchMode::Description,
        mica_core::config::SearchMode::Binary => IndexSearchMode::Binary,
        mica_core::config::SearchMode::All => IndexSearchMode::All,
    }
}

fn toggle_column_setting(app: &mut tui::app::App, column: tui::app::ColumnKind) {
    app.toggle_column(column);
    if let Err(err) = save_columns_to_config(&app.columns) {
        app.push_toast(tui::app::ToastLevel::Error, err.to_string());
    }
}

fn open_env_overlay(app: &mut tui::app::App) {
    let mut entries: Vec<tui::app::EnvEntry> = app
        .env
        .iter()
        .map(|(key, value)| tui::app::EnvEntry {
            key: key.clone(),
            value: value.clone(),
        })
        .collect();
    entries.sort_by(|a, b| a.key.cmp(&b.key));
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
    paths: &ProjectPaths,
    state: &ProjectState,
    app: &tui::app::App,
) -> Result<tui::app::Overlay, CliError> {
    let mut temp_state = state.clone();
    temp_state.packages.added = app.added.iter().cloned().collect();
    temp_state.packages.removed = app.removed.iter().cloned().collect();
    temp_state.packages.pinned = app.pinned.clone();
    temp_state.presets.active = app.active_presets.iter().cloned().collect();
    temp_state.env = app.env.clone();
    temp_state.shell.hook = app.shell_hook.clone();

    let generated = format_mica_nix(&build_project_nix(paths, &temp_state)?);
    let existing = std::fs::read_to_string(&paths.nix_path).map_err(CliError::ReadNix)?;
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
    temp_state.packages.pinned = app.pinned.clone();
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

fn build_version_picker_overlay(
    app: &tui::app::App,
) -> Result<Option<tui::app::Overlay>, CliError> {
    let pkg = match app.packages.get(app.cursor) {
        Some(pkg) => pkg,
        None => return Ok(None),
    };
    let base_attr = app.base_attr_for(&pkg.attr_path);
    let versions_path = versions_db_path()?;
    if !versions_path.exists() {
        return Ok(None);
    }
    let conn = open_versions_db(&versions_path).map_err(CliError::Index)?;
    let versions = list_versions(&conn, &base_attr, 200).map_err(CliError::Index)?;
    if versions.is_empty() {
        return Ok(None);
    }
    let entries = versions
        .into_iter()
        .map(|entry| tui::app::VersionPickerEntry {
            source: entry.source,
            version: entry.version,
            commit: entry.commit,
            commit_date: entry.commit_date,
            branch: entry.branch,
            url: entry.url,
        })
        .collect();

    Ok(Some(tui::app::Overlay::VersionPicker(
        tui::app::VersionPickerState {
            entries,
            cursor: 0,
            package: base_attr,
        },
    )))
}

fn apply_version_selection(
    output: &Output,
    app: &mut tui::app::App,
    package: &str,
    entry: tui::app::VersionPickerEntry,
) -> Result<(), CliError> {
    let sha256 = run_with_spinner(output, "prefetching nix tarball", || {
        fetch_nix_sha256(&entry.url, &entry.commit)
    })?;
    let pin = Pin {
        name: None,
        url: entry.url,
        rev: entry.commit,
        sha256,
        branch: entry.branch,
        updated: Utc::now().date_naive(),
    };
    app.pinned.insert(
        package.to_string(),
        PinnedPackage {
            version: entry.version,
            pin,
        },
    );
    app.added.remove(package);
    app.removed.remove(package);
    app.update_dirty();
    app.push_toast(tui::app::ToastLevel::Info, "Pinned package version");
    Ok(())
}

fn resolve_pinned_version(package: &str, pin: &Pin) -> Result<Option<String>, CliError> {
    let versions_path = versions_db_path()?;
    if !versions_path.exists() {
        return Ok(None);
    }
    let conn = open_versions_db(&versions_path).map_err(CliError::Index)?;
    let source = pin_source_label(pin);
    if let Some(entry) =
        version_for_commit(&conn, package, &source, &pin.rev).map_err(CliError::Index)?
    {
        return Ok(Some(entry.version));
    }
    if let Some(entry) =
        latest_version_for_source(&conn, package, &source).map_err(CliError::Index)?
    {
        return Ok(Some(entry.version));
    }
    Ok(None)
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
    let raw_value = raw_value.trim();

    let (original, value_mode) = match &state.mode {
        tui::app::EnvEditMode::Edit {
            original_key,
            value_mode,
        } => (original_key.clone(), *value_mode),
        _ => (None, tui::app::EnvValueMode::String),
    };
    let value = encode_env_editor_value(raw_value, value_mode)?;

    if let Some(original_key) = &original {
        if original_key != key && state.entries.iter().any(|entry| entry.key == key) {
            return Err("key already exists".to_string());
        }
        if let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| &entry.key == original_key)
        {
            entry.key = key.to_string();
            entry.value = value;
        } else {
            state.entries.push(tui::app::EnvEntry {
                key: key.to_string(),
                value,
            });
        }
    } else {
        if state.entries.iter().any(|entry| entry.key == key) {
            return Err("key already exists".to_string());
        }
        state.entries.push(tui::app::EnvEntry {
            key: key.to_string(),
            value,
        });
    }

    state.entries.sort_by(|a, b| a.key.cmp(&b.key));
    state.cursor = state
        .entries
        .iter()
        .position(|entry| entry.key == key)
        .unwrap_or(0);
    state.mode = tui::app::EnvEditMode::List;
    state.input.clear();
    state.input_cursor = 0;
    state.error = None;
    Ok(())
}

fn apply_env_overlay(app: &mut tui::app::App, state: tui::app::EnvEditorState) {
    let mut env = BTreeMap::new();
    for entry in state.entries {
        let key = entry.key;
        let value = entry.value;
        if !key.trim().is_empty() {
            env.insert(key, value);
        }
    }
    app.env = env;
    app.update_dirty();
}

fn env_value_mode_from_stored(value: &str) -> tui::app::EnvValueMode {
    if value.starts_with(NIX_EXPR_PREFIX) || is_legacy_nix_expression_value(value) {
        tui::app::EnvValueMode::NixExpression
    } else {
        tui::app::EnvValueMode::String
    }
}

fn env_value_for_editor(value: &str) -> String {
    value
        .strip_prefix(NIX_EXPR_PREFIX)
        .unwrap_or(value)
        .to_string()
}

fn encode_env_editor_value(raw: &str, mode: tui::app::EnvValueMode) -> Result<String, String> {
    match mode {
        tui::app::EnvValueMode::String => Ok(raw.to_string()),
        tui::app::EnvValueMode::NixExpression => {
            if raw.trim().is_empty() {
                return Err("expression cannot be empty".to_string());
            }
            Ok(format!("{}{}", NIX_EXPR_PREFIX, raw.trim()))
        }
    }
}

fn is_legacy_nix_expression_value(value: &str) -> bool {
    let trimmed = value.trim();
    (trimmed.len() >= 2
        && trimmed.starts_with('\"')
        && trimmed.ends_with('\"')
        && contains_unescaped_nix_interpolation(trimmed))
        || (trimmed.len() >= 4 && trimmed.starts_with("''") && trimmed.ends_with("''"))
}

fn contains_unescaped_nix_interpolation(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut idx = 0usize;
    while idx + 1 < bytes.len() {
        if bytes[idx] == b'$' && bytes[idx + 1] == b'{' && (idx == 0 || bytes[idx - 1] != b'\\') {
            return true;
        }
        idx += 1;
    }
    false
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

fn index_info_unknown(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty() || trimmed.eq_ignore_ascii_case("unknown")
}

fn index_info_with_pin_fallback(
    mut info: tui::app::IndexInfo,
    pins: &[IndexPin],
) -> tui::app::IndexInfo {
    let Some(primary) = pins.first() else {
        return info;
    };
    if index_info_unknown(&info.url) {
        info.url = primary.pin.url.clone();
    }
    if index_info_unknown(&info.rev) {
        info.rev = primary.pin.rev.clone();
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
            packages_optional: preset.packages_optional,
        })
        .collect();
    presets.sort_by_key(|preset| preset.order);
    Ok(presets)
}

fn save_tui_selection(
    paths: &ProjectPaths,
    state: &mut ProjectState,
    app: &mut tui::app::App,
) -> Result<(), CliError> {
    state.packages.added = app.added.iter().cloned().collect();
    state.packages.removed = app.removed.iter().cloned().collect();
    state.packages.pinned = app.pinned.clone();
    state.presets.active = app.active_presets.iter().cloned().collect();
    state.env = app.env.clone();
    state.shell.hook = app.shell_hook.clone();
    update_project_modified(state);
    save_project_state(paths, state)?;
    app.commit_baseline();
    Ok(())
}

fn save_profile_tui_selection(
    output: &Output,
    state: &mut GlobalProfileState,
    app: &mut tui::app::App,
) -> Result<(), CliError> {
    state.packages.added = app.added.iter().cloned().collect();
    state.packages.removed = app.removed.iter().cloned().collect();
    state.packages.pinned = app.pinned.clone();
    state.presets.active = app.active_presets.iter().cloned().collect();
    update_profile_modified(state);
    save_profile_state(state)?;
    sync_and_install_profile(output, state)?;
    app.commit_baseline();
    Ok(())
}

fn build_initial_project_state(repo: Option<String>) -> Result<ProjectState, CliError> {
    let config = load_config_or_default()?;
    let now = Utc::now();
    let url = resolve_init_repo(repo, &config);
    let branch = config.nixpkgs.default_branch.clone();
    let rev = fetch_latest_github_rev(&url, &branch)?;
    let sha256 = fetch_nix_sha256(&url, &rev)?;
    Ok(ProjectState {
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
    })
}

fn init_project_state(paths: &ProjectPaths, repo: Option<String>) -> Result<(), CliError> {
    let path = &paths.nix_path;
    if path.exists() {
        return Err(CliError::StateExists(path.to_path_buf()));
    }
    let state = build_initial_project_state(repo)?;
    sync_project_nix(paths, &state)?;
    Ok(())
}

fn build_initial_profile_state(repo: Option<String>) -> Result<GlobalProfileState, CliError> {
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
    Ok(GlobalProfileState {
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
    })
}

fn init_profile_state(repo: Option<String>) -> Result<(), CliError> {
    let state = build_initial_profile_state(repo)?;
    let path = profile_state_path()?;
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

fn apply_pin_map_to_app(app: &mut tui::app::App, pins: &[IndexPin]) {
    app.pin_map.clear();
    for pin in pins {
        if let Some(label) = pin.name.as_ref() {
            app.pin_map.insert(label.clone(), pin.pin.clone());
        }
    }
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
    output: &Output,
    input: &Path,
    output_path: &Path,
    pin: Option<&Pin>,
) -> Result<usize, CliError> {
    let mut packages = load_packages_from_json(input)?;
    normalize_attr_paths(&mut packages);
    let index_has_meta = packages_have_meta(&packages);
    if let Some(pin) = pin {
        let versions_path = versions_db_path()?;
        if let Some(parent) = versions_path.parent() {
            std::fs::create_dir_all(parent).map_err(CliError::WriteNix)?;
        }
        let mut versions_conn = init_versions_db(&versions_path)?;
        let indexed_at = Utc::now().to_rfc3339();
        let source = pin_source_label(pin);
        let commit_date = pin_commit_date(output, pin);
        let branch = pin_branch_label(pin);
        let version_source = VersionSource {
            source,
            url: pin.url.clone(),
            branch,
            commit: pin.rev.clone(),
            commit_date,
            indexed_at: indexed_at.clone(),
        };
        record_versions(&mut versions_conn, &version_source, &packages).map_err(CliError::Index)?;
    }
    rebuild_index_with_packages(output_path, &packages, pin, index_has_meta)
}

fn rebuild_index_from_local_repo(
    output: &Output,
    repo_path: &Path,
    output_path: &Path,
    extra_skip: &[String],
    show_trace: bool,
) -> Result<usize, CliError> {
    let mut packages = load_packages_from_local_repo(output, repo_path, extra_skip, show_trace)?;
    normalize_attr_paths(&mut packages);
    let index_has_meta = packages_have_meta(&packages);
    rebuild_index_with_packages(output_path, &packages, None, index_has_meta)
}

fn rebuild_index_from_pins(
    output: &Output,
    output_path: &Path,
    pins: &[IndexPin],
) -> Result<usize, CliError> {
    let versions_path = versions_db_path()?;
    if let Some(parent) = versions_path.parent() {
        std::fs::create_dir_all(parent).map_err(CliError::WriteNix)?;
    }
    let mut versions_conn = init_versions_db(&versions_path)?;
    let indexed_at = Utc::now().to_rfc3339();
    let mut packages = Vec::new();
    for (idx, index_pin) in pins.iter().enumerate() {
        if idx == 0 {
            ensure_pin_complete(&index_pin.pin)?;
        } else if ensure_pin_complete(&index_pin.pin).is_err() {
            continue;
        }
        let pin_label = index_pin.name.as_deref().unwrap_or("nixpkgs");
        let mut pin_packages = match load_packages_from_pin(output, &index_pin.pin) {
            Ok(packages) => packages,
            Err(err) if idx > 0 => {
                output.warn(format!(
                    "warning: skipping supplemental pin '{}' ({}@{}): {}",
                    pin_label, index_pin.pin.url, index_pin.pin.rev, err
                ));
                continue;
            }
            Err(err) => return Err(err),
        };
        normalize_attr_paths(&mut pin_packages);
        let source = pin_source_label(&index_pin.pin);
        let commit_date = pin_commit_date(output, &index_pin.pin);
        let branch = pin_branch_label(&index_pin.pin);
        let version_source = VersionSource {
            source,
            url: index_pin.pin.url.clone(),
            branch,
            commit: index_pin.pin.rev.clone(),
            commit_date,
            indexed_at: indexed_at.clone(),
        };
        record_versions(&mut versions_conn, &version_source, &pin_packages)
            .map_err(CliError::Index)?;
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
    output: &Output,
    output_path: &Path,
    pins: &[IndexPin],
) -> Result<usize, CliError> {
    run_with_spinner(output, "building index", || {
        rebuild_index_from_pins(output, output_path, pins)
    })
}

fn rebuild_index_from_local_repo_with_spinner(
    output: &Output,
    repo_path: &Path,
    output_path: &Path,
    extra_skip: &[String],
    show_trace: bool,
) -> Result<usize, CliError> {
    run_with_spinner(output, "building index", || {
        rebuild_index_from_local_repo(output, repo_path, output_path, extra_skip, show_trace)
    })
}

fn resolve_remote_index_urls(remote_url: &str, commit: Option<&str>) -> Vec<String> {
    let trimmed = remote_url.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.ends_with(".db") {
        return vec![trimmed.to_string()];
    }
    let base = trimmed.trim_end_matches('/');
    let mut urls = Vec::new();
    if let Some(commit) = commit.map(str::trim).filter(|value| !value.is_empty()) {
        urls.push(format!("{}/{}.db", base, commit));
    }
    urls
}

fn fetch_remote_index_url(url: &str, output_path: &Path) -> Result<(), CliError> {
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let response = client.get(url).send()?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(CliError::RemoteIndexFailed(status, body));
    }
    let bytes = response.bytes()?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(CliError::WriteNix)?;
    }
    let tmp_path = output_path.with_extension("tmp");
    std::fs::write(&tmp_path, &bytes).map_err(CliError::WriteNix)?;
    std::fs::rename(&tmp_path, output_path).map_err(CliError::WriteNix)?;
    Ok(())
}

fn try_fetch_remote_index(
    output: &Output,
    remote_url: &str,
    output_path: &Path,
    commit: Option<&str>,
) -> Result<bool, CliError> {
    let urls = resolve_remote_index_urls(remote_url, commit);
    if urls.is_empty() {
        return Ok(false);
    }

    let mut last_error: Option<CliError> = None;
    for url in urls {
        output.status(format!("fetching remote index from {}", url));
        match fetch_remote_index_url(&url, output_path) {
            Ok(()) => {
                output.status("remote index fetched");
                return Ok(true);
            }
            Err(CliError::RemoteIndexFailed(status, _))
                if status == reqwest::StatusCode::NOT_FOUND =>
            {
                output.verbose(format!("remote index not found at {}", url));
            }
            Err(err) => {
                output.verbose(format!("remote index fetch failed at {}: {}", url, err));
                last_error = Some(err);
            }
        }
    }

    if let Some(err) = last_error {
        output.warn(format!("remote index fetch failed: {}", err));
    }
    Ok(false)
}

fn primary_pin_rev(pins: &[IndexPin]) -> Option<&str> {
    pins.first()
        .map(|entry| entry.pin.rev.trim())
        .filter(|value| !value.is_empty())
}

fn try_fetch_remote_index_for_pins(
    output: &Output,
    config: Option<&Config>,
    index_path: &Path,
    pins: &[IndexPin],
) -> Result<bool, CliError> {
    let Some(config) = config else {
        return Ok(false);
    };
    let fetched = try_fetch_remote_index(
        output,
        &config.index.remote_url,
        index_path,
        primary_pin_rev(pins),
    )?;
    if !config.index.remote_url.trim().is_empty() {
        record_index_check_time(output);
    }
    Ok(fetched)
}

fn index_check_path() -> Result<PathBuf, CliError> {
    Ok(cache_dir()?.join("index.last_check"))
}

fn read_index_check_time() -> Result<Option<DateTime<Utc>>, CliError> {
    let path = index_check_path()?;
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(CliError::ReadNix(err)),
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match DateTime::parse_from_rfc3339(trimmed) {
        Ok(dt) => Ok(Some(dt.with_timezone(&Utc))),
        Err(_) => Ok(None),
    }
}

fn write_index_check_time(now: DateTime<Utc>) -> Result<(), CliError> {
    let path = index_check_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(CliError::WriteNix)?;
    }
    std::fs::write(path, now.to_rfc3339()).map_err(CliError::WriteNix)
}

fn record_index_check_time(output: &Output) {
    if let Err(err) = write_index_check_time(Utc::now()) {
        output.verbose(format!("index check timestamp write failed: {}", err));
    }
}

fn should_check_remote_index(config: &Config) -> Result<bool, CliError> {
    if config.index.remote_url.trim().is_empty() {
        return Ok(false);
    }
    if config.index.update_check_interval == 0 {
        return Ok(false);
    }
    let now = Utc::now();
    if let Some(last) = read_index_check_time()? {
        let elapsed = now.signed_duration_since(last);
        let interval = chrono::Duration::hours(config.index.update_check_interval as i64);
        if elapsed < interval {
            return Ok(false);
        }
    }
    Ok(true)
}

fn maybe_refresh_remote_index(
    output: &Output,
    config: &Config,
    index_path: &Path,
    commit: Option<&str>,
) -> Result<bool, CliError> {
    if !should_check_remote_index(config)? {
        return Ok(false);
    }
    output.status("checking remote index for updates");
    let fetched = try_fetch_remote_index(output, &config.index.remote_url, index_path, commit)?;
    record_index_check_time(output);
    Ok(fetched)
}

fn index_skip_overrides(extra: &[String]) -> Vec<String> {
    let mut skip = parse_skip_list(
        std::env::var("MICA_NIX_SKIP_ATTRS")
            .unwrap_or_default()
            .as_str(),
    );
    for entry in extra {
        if !skip.iter().any(|existing| existing == entry) {
            skip.push(entry.clone());
        }
    }
    skip.sort();
    skip.dedup();
    skip
}

fn load_packages_from_pin(
    output: &Output,
    pin: &Pin,
) -> Result<Vec<mica_index::generate::NixPackage>, CliError> {
    let skip = index_skip_overrides(&[]);
    load_packages_from_nix_expression(output, skip, nix_env_show_trace(), |all_skip| {
        nix_env_expression(pin, all_skip)
    })
}

fn load_packages_from_local_repo(
    output: &Output,
    repo_path: &Path,
    extra_skip: &[String],
    show_trace: bool,
) -> Result<Vec<mica_index::generate::NixPackage>, CliError> {
    let repo_path = std::fs::canonicalize(repo_path).map_err(CliError::ReadNix)?;
    let skip = index_skip_overrides(extra_skip);
    load_packages_from_nix_expression(
        output,
        skip,
        show_trace || nix_env_show_trace(),
        |all_skip| nix_env_expression_from_local_repo(&repo_path, all_skip),
    )
}

fn load_packages_from_nix_expression(
    output: &Output,
    mut skip: Vec<String>,
    mut use_show_trace: bool,
    expression_builder: impl Fn(&[String]) -> String,
) -> Result<Vec<mica_index::generate::NixPackage>, CliError> {
    let expr_path = temp_index_nix_path();
    let json_path = temp_index_json_path();
    let mut attempts = 0usize;
    let max_attempts = 12usize;
    loop {
        attempts += 1;
        let skipped_label = if skip.is_empty() {
            "none".to_string()
        } else {
            skip.join(",")
        };
        output.status(format!(
            "index attempt {}/{} (skipped: {}, show-trace: {})",
            attempts, max_attempts, skipped_label, use_show_trace
        ));
        let all_skip = build_index_skip_list(&skip);
        let all_skip_label = if all_skip.is_empty() {
            "none".to_string()
        } else {
            all_skip.join(",")
        };
        output.verbose(format!("index skip list: {}", all_skip_label));
        std::fs::write(&expr_path, expression_builder(&all_skip)).map_err(CliError::WriteNix)?;

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

        let command_output = child.wait_with_output().map_err(CliError::NixEnvIo)?;
        if command_output.status.success() {
            let packages = load_packages_from_json(&json_path)?;
            if !keep_index_temp_files() {
                let _ = std::fs::remove_file(&expr_path);
                let _ = std::fs::remove_file(&json_path);
            }
            return Ok(packages);
        }

        let stderr = String::from_utf8_lossy(&command_output.stderr);
        if attempts < max_attempts {
            if let Some(attr) = parse_failed_attr(&stderr) {
                if !skip.iter().any(|entry| entry == &attr) {
                    skip.push(attr.clone());
                    output.status(format!("index retry: skipping attr '{}'", attr));
                    continue;
                }
            } else if !use_show_trace {
                use_show_trace = true;
                output.status("index retry: enabling --show-trace");
                continue;
            }
        }

        let mut message = format!("status={}, stderr={}", command_output.status, stderr.trim());
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
  baseAttempt =
    let imported = import src;
    in builtins.tryEval (
      if builtins.isFunction imported
      then imported {{ }}
      else imported
    );
  baseFallback =
    let imported = import nixpkgsSrc;
    in if builtins.isFunction imported
      then imported {{ }}
      else imported;
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

fn nix_env_expression_from_local_repo(repo_path: &Path, skip: &[String]) -> String {
    let repo_path = escape_nix_string(repo_path.to_string_lossy().as_ref());
    let skip_regex: Vec<String> = skip.iter().map(|entry| glob_to_regex(entry)).collect();
    let skip_list = nix_string_list(&skip_regex);
    format!(
        r#"let
  src = builtins.toPath "{repo_path}";
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
  baseAttempt =
    let imported = import src;
    in builtins.tryEval (
      if builtins.isFunction imported
      then imported {{ }}
      else imported
    );
  baseFallback =
    let imported = import nixpkgsSrc;
    in if builtins.isFunction imported
      then imported {{ }}
      else imported;
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
        repo_path = repo_path,
        skip_list = skip_list
    )
}

fn run_with_spinner<T>(
    output: &Output,
    message: &str,
    action: impl FnOnce() -> Result<T, CliError>,
) -> Result<T, CliError> {
    if output.quiet || !io::stderr().is_terminal() {
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
        Ok(_) => output.status(format!("\r{} done", message)),
        Err(err) => {
            output.status(format!("\r{} failed", message));
            output.warn(format!("{} error: {}", message, err));
        }
    }
    result
}

fn load_project_state(paths: &ProjectPaths) -> Result<ProjectState, CliError> {
    let path = &paths.nix_path;
    if !path.exists() {
        return Err(CliError::MissingDefaultNix(path.to_path_buf()));
    }
    let content = std::fs::read_to_string(path).map_err(CliError::ReadNix)?;
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
    state.packages.pinned = parsed.pinned;
    state.packages.added = compute_added_packages(
        parsed.packages,
        &state.presets.active,
        &state.packages.pinned,
    )?;
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

fn save_project_state(paths: &ProjectPaths, state: &ProjectState) -> Result<(), CliError> {
    sync_project_nix(paths, state)
}

fn build_project_nix(paths: &ProjectPaths, state: &ProjectState) -> Result<String, CliError> {
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
    let project_name = project_dir_name(paths);
    let generated = generate_project_nix(state, &merged, &project_name, Utc::now());
    let output = if paths.nix_path.exists() {
        let existing = std::fs::read_to_string(&paths.nix_path).map_err(CliError::ReadNix)?;
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

fn sync_project_nix(paths: &ProjectPaths, state: &ProjectState) -> Result<(), CliError> {
    let output = build_project_nix(paths, state)?;
    let formatted = format_mica_nix(&output);
    std::fs::write(&paths.nix_path, formatted).map_err(CliError::WriteNix)
}

fn build_profile_nix(state: &GlobalProfileState) -> Result<String, CliError> {
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
    Ok(generate_profile_nix(state, &merged, Utc::now()))
}

fn sync_profile_nix(state: &GlobalProfileState) -> Result<(), CliError> {
    let generated = build_profile_nix(state)?;
    let formatted = format_mica_nix(&generated);
    std::fs::write(profile_nix_path()?, formatted).map_err(CliError::WriteNix)
}

fn apply_project_changes(
    output: &Output,
    paths: &ProjectPaths,
    dry_run: bool,
    state: &ProjectState,
) -> Result<(), CliError> {
    if dry_run {
        output.info("dry-run: skipping write");
        if paths.nix_path.exists() {
            diff_project(output, paths, state)?;
        } else {
            output.info(format!("would write {}", paths.nix_path.display()));
        }
        Ok(())
    } else {
        save_project_state(paths, state)
    }
}

fn apply_profile_changes(
    output: &Output,
    dry_run: bool,
    state: &GlobalProfileState,
) -> Result<(), CliError> {
    if dry_run {
        output.info("dry-run: skipping install");
        let path = profile_nix_path()?;
        if path.exists() {
            diff_profile(output, state)?;
        } else {
            output.info(format!("would write {}", path.display()));
        }
        Ok(())
    } else {
        save_profile_state(state)?;
        sync_and_install_profile(output, state)?;
        Ok(())
    }
}

fn generations_dir() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("generations"))
}

fn latest_nix_env_generation() -> Result<Option<u64>, CliError> {
    let output = ProcessCommand::new("nix-env")
        .arg("--list-generations")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                CliError::MissingNixEnv
            } else {
                CliError::NixEnvIo(err)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CliError::NixEnvFailed(format!(
            "status={}, stderr={}",
            output.status,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut last = None;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(id) = trimmed.split_whitespace().next() {
            if let Ok(parsed) = id.parse::<u64>() {
                last = Some(parsed);
            }
        }
    }
    Ok(last)
}

fn profile_installed_packages(state: &GlobalProfileState) -> Result<Vec<String>, CliError> {
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
    let mut packages: BTreeSet<String> = merged.all_packages.into_iter().collect();
    for pkg in state.packages.pinned.keys() {
        packages.insert(pkg.clone());
    }
    Ok(packages.into_iter().collect())
}

fn snapshot_generation(state: &GlobalProfileState, id: u64) -> Result<(), CliError> {
    let dir = generations_dir()?.join(id.to_string());
    std::fs::create_dir_all(&dir).map_err(CliError::WriteNix)?;
    let snapshot_path = dir.join("profile.toml");
    state
        .save_to_path(&snapshot_path)
        .map_err(CliError::State)?;
    let profile_nix = profile_nix_path()?;
    if profile_nix.exists() {
        let _ = std::fs::copy(&profile_nix, dir.join("profile.nix"));
    }
    Ok(())
}

fn record_profile_generation(output: &Output, state: &GlobalProfileState) -> Result<(), CliError> {
    let packages = profile_installed_packages(state)?;
    let fallback = state
        .generations
        .history
        .last()
        .map(|entry| entry.id + 1)
        .unwrap_or(1);
    let id = match latest_nix_env_generation() {
        Ok(Some(id)) => id,
        Ok(None) => fallback,
        Err(err) => {
            output.warn(format!(
                "warning: failed to read nix-env generations: {}",
                err
            ));
            fallback
        }
    };

    let mut record_state = state.clone();
    let timestamp = Utc::now();
    let entry = GenerationEntry {
        id,
        timestamp,
        packages,
    };
    if let Some(existing) = record_state
        .generations
        .history
        .iter_mut()
        .find(|entry| entry.id == id)
    {
        *existing = entry;
    } else {
        record_state.generations.history.push(entry);
    }
    record_state
        .generations
        .history
        .sort_by_key(|entry| entry.id);
    if record_state.generations.history.len() > 50 {
        let keep_from = record_state.generations.history.len() - 50;
        record_state.generations.history = record_state.generations.history.split_off(keep_from);
    }
    record_state.mica.modified = timestamp;
    save_profile_state(&record_state)?;
    snapshot_generation(&record_state, id)?;
    Ok(())
}

fn list_generations(output: &Output, state: &GlobalProfileState) -> Result<(), CliError> {
    if state.generations.history.is_empty() {
        output.info("no generations recorded");
        return Ok(());
    }
    for entry in &state.generations.history {
        output.info(format!(
            "{} {} ({} pkgs)",
            entry.id,
            entry.timestamp.to_rfc3339(),
            entry.packages.len()
        ));
    }
    Ok(())
}

fn rollback_generation(
    output: &Output,
    target_id: Option<u64>,
    dry_run: bool,
) -> Result<(), CliError> {
    let current = load_profile_state()?;
    if current.generations.history.is_empty() {
        return Err(CliError::NoGenerations);
    }
    let target = match target_id {
        Some(id) => id,
        None => {
            if current.generations.history.len() < 2 {
                return Err(CliError::NoGenerations);
            }
            current.generations.history[current.generations.history.len() - 2].id
        }
    };
    if !current
        .generations
        .history
        .iter()
        .any(|entry| entry.id == target)
    {
        return Err(CliError::GenerationNotFound(target));
    }
    let snapshot_path = generations_dir()?
        .join(target.to_string())
        .join("profile.toml");
    if !snapshot_path.exists() {
        return Err(CliError::GenerationSnapshotMissing(snapshot_path));
    }
    let snapshot = GlobalProfileState::load_from_path(&snapshot_path).map_err(CliError::State)?;
    let mut next_state = snapshot;
    next_state.generations = current.generations.clone();
    next_state.mica.modified = Utc::now();

    if dry_run {
        output.info(format!("dry-run: would rollback to generation {}", target));
        diff_profile(output, &next_state)?;
        return Ok(());
    }

    save_profile_state(&next_state)?;
    sync_and_install_profile(output, &next_state)?;
    output.info(format!("rolled back to generation {}", target));
    Ok(())
}

fn sync_and_install_profile(output: &Output, state: &GlobalProfileState) -> Result<(), CliError> {
    sync_profile_nix(state)?;
    run_with_spinner(output, "installing global profile", install_profile_nix)?;
    if let Err(err) = record_profile_generation(output, state) {
        output.warn(format!("warning: failed to record generation: {}", err));
    }
    Ok(())
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

fn create_temp_nix_file(contents: &str) -> Result<PathBuf, CliError> {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    for attempt in 0..20u32 {
        let path = dir.join(format!("mica-eval-{}-{}.nix", pid, attempt));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())
                    .map_err(CliError::TempNixFile)?;
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(CliError::TempNixFile(err)),
        }
    }
    Err(CliError::TempNixFile(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create temp nix file",
    )))
}

fn eval_nix_file(path: &Path) -> Result<(), CliError> {
    let parse_output = ProcessCommand::new("nix-instantiate")
        .args(["--parse"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                CliError::MissingNixInstantiate
            } else {
                CliError::NixInstantiateFailed(err.to_string())
            }
        })?;
    if !parse_output.status.success() {
        let stdout = String::from_utf8_lossy(&parse_output.stdout);
        let stderr = String::from_utf8_lossy(&parse_output.stderr);
        return Err(CliError::NixInstantiateFailed(format!(
            "status={}, stdout={}, stderr={}",
            parse_output.status,
            stdout.trim(),
            stderr.trim()
        )));
    }

    let build_output = ProcessCommand::new("nix-build")
        .args(["--dry-run"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                CliError::MissingNixBuild
            } else {
                CliError::NixBuildFailed(err.to_string())
            }
        })?;
    if !build_output.status.success() {
        let stdout = String::from_utf8_lossy(&build_output.stdout);
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        return Err(CliError::NixBuildFailed(format!(
            "status={}, stdout={}, stderr={}",
            build_output.status,
            stdout.trim(),
            stderr.trim()
        )));
    }

    Ok(())
}

fn eval_nix_contents(output: &Output, contents: &str) -> Result<(), CliError> {
    let path = create_temp_nix_file(contents)?;
    let result = eval_nix_file(&path);
    let _ = std::fs::remove_file(&path);
    if result.is_ok() {
        output.info("validation ok");
    }
    result
}

fn diff_project(
    output: &Output,
    paths: &ProjectPaths,
    state: &ProjectState,
) -> Result<(), CliError> {
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
    let project_name = project_dir_name(paths);
    let generated = generate_project_nix(state, &merged, &project_name, Utc::now());
    let existing = std::fs::read_to_string(&paths.nix_path).map_err(CliError::ReadNix)?;
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
        output.info("no drift detected");
    } else {
        output.info("drift detected:");
        output.info(format!(
            "  pin: {}",
            if pin_changed { "changed" } else { "ok" }
        ));
        output.info(format!(
            "  let: {}",
            if let_changed { "changed" } else { "ok" }
        ));
        output.info(format!(
            "  packages: {}",
            if packages_changed { "changed" } else { "ok" }
        ));
        output.info(format!(
            "  env: {}",
            if env_changed { "changed" } else { "ok" }
        ));
        output.info(format!(
            "  shellHook: {}",
            if shell_changed { "changed" } else { "ok" }
        ));
        output.info(format!(
            "  override: {}",
            if override_changed { "changed" } else { "ok" }
        ));
        output.info(format!(
            "  override shellHook: {}",
            if override_shellhook_changed {
                "changed"
            } else {
                "ok"
            }
        ));
        output.info(format!(
            "  override merge: {}",
            if override_merge_changed {
                "changed"
            } else {
                "ok"
            }
        ));
    }
    Ok(())
}

fn diff_profile(output: &Output, state: &GlobalProfileState) -> Result<(), CliError> {
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
        output.info("no drift detected");
    } else {
        output.info("drift detected:");
        output.info(format!(
            "  pins: {}",
            if pins_changed { "changed" } else { "ok" }
        ));
        output.info(format!(
            "  paths: {}",
            if paths_changed { "changed" } else { "ok" }
        ));
    }
    Ok(())
}

fn update_project_state_from_nix(
    paths: &ProjectPaths,
    state: &mut ProjectState,
) -> Result<(), CliError> {
    let content = std::fs::read_to_string(&paths.nix_path).map_err(CliError::ReadNix)?;
    let parsed = parse_project_state_from_nix(&content).map_err(CliError::NixStateParse)?;
    state.pin = parsed.pin;
    state.pins = parsed.pins;
    state.packages.pinned = parsed.pinned;
    state.packages.added =
        compute_added_packages(parsed.packages, &parsed.presets, &state.packages.pinned)?;
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
    state.packages.pinned = parsed.pinned;
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
    let requested_branch = if branch.trim().is_empty() {
        "main"
    } else {
        branch.trim()
    };
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

    match fetch_github_commit_sha(&client, &owner, &repo, requested_branch) {
        Ok(rev) => Ok(rev),
        Err(CliError::GitHubApiStatus(status, body))
            if should_retry_default_branch_lookup(status, &body) =>
        {
            let default_branch = fetch_github_default_branch(&client, &owner, &repo)?;
            if default_branch.trim().is_empty() || default_branch == requested_branch {
                return Err(CliError::GitHubApiStatus(status, body));
            }
            fetch_github_commit_sha(&client, &owner, &repo, &default_branch)
        }
        Err(err) => Err(err),
    }
}

fn fetch_github_commit_sha(
    client: &Client,
    owner: &str,
    repo: &str,
    reference: &str,
) -> Result<String, CliError> {
    let ref_encoded = encode_github_ref(reference);
    let api_url = format!(
        "https://api.github.com/repos/{}/{}/commits/{}",
        owner, repo, ref_encoded
    );
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

fn fetch_github_commit_date(url: &str, rev: &str) -> Result<String, CliError> {
    let (owner, repo) = parse_github_repo(url)?;
    let ref_encoded = encode_github_ref(rev);
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
    if let Some(committer) = commit.commit.committer {
        if !committer.date.trim().is_empty() {
            return Ok(committer.date);
        }
    }
    if let Some(author) = commit.commit.author {
        if !author.date.trim().is_empty() {
            return Ok(author.date);
        }
    }

    Err(CliError::GitHubApiMissingDate)
}

fn fetch_github_default_branch(
    client: &Client,
    owner: &str,
    repo: &str,
) -> Result<String, CliError> {
    let api_url = format!("https://api.github.com/repos/{}/{}", owner, repo);
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

    let repo_info: GitHubRepoInfo = response.json()?;
    if repo_info.default_branch.trim().is_empty() {
        return Err(CliError::GitHubApiMissingDefaultBranch);
    }
    Ok(repo_info.default_branch)
}

fn should_retry_default_branch_lookup(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::UNPROCESSABLE_ENTITY && body.contains("No commit found for SHA")
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
                .entry(name.clone())
                .or_insert_with(|| PinnedPackage {
                    version: String::new(),
                    pin: state.pin.clone(),
                });
            update_pin_fields(&mut entry.pin, url, rev, sha256, branch);
            entry.pin.updated = now.date_naive();
            state.packages.added.retain(|pkg| pkg != &name);
            state.packages.removed.retain(|pkg| pkg != &name);
            if let Some(version) = resolve_pinned_version(&name, &entry.pin)? {
                entry.version = version;
            } else if entry.version.trim().is_empty() {
                entry.version = "CHANGEME".to_string();
            }
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
                .entry(name.clone())
                .or_insert_with(|| PinnedPackage {
                    version: String::new(),
                    pin: state.pin.clone(),
                });
            update_pin_fields(&mut entry.pin, url, rev, sha256, branch);
            entry.pin.updated = now.date_naive();
            state.packages.added.retain(|pkg| pkg != &name);
            state.packages.removed.retain(|pkg| pkg != &name);
            if let Some(version) = resolve_pinned_version(&name, &entry.pin)? {
                entry.version = version;
            } else if entry.version.trim().is_empty() {
                entry.version = "CHANGEME".to_string();
            }
        }
    }
    update_profile_modified(state);
    Ok(())
}

fn project_dir_name(paths: &ProjectPaths) -> String {
    paths
        .root_dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "dev-environment".to_string())
}

fn index_display_name_for_url(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return "jpetrucciani/nix".to_string();
    }
    if let Some(pos) = trimmed.find("github.com/") {
        let mut tail = &trimmed[pos + "github.com/".len()..];
        if let Some(idx) = tail.find('#') {
            tail = &tail[..idx];
        }
        if let Some(idx) = tail.find('?') {
            tail = &tail[..idx];
        }
        for marker in ["/archive/", "/tarball/", "/commit/", "/tree/", "/releases/"] {
            if let Some(idx) = tail.find(marker) {
                tail = &tail[..idx];
                break;
            }
        }
        let tail = tail.trim_end_matches(".git");
        let parts: Vec<&str> = tail.split('/').filter(|part| !part.is_empty()).collect();
        if parts.len() >= 2 {
            return format!("{}/{}", parts[0], parts[1]);
        }
    }
    trimmed.to_string()
}

fn pin_branch_label(pin: &Pin) -> String {
    if pin.branch.trim().is_empty() {
        "main".to_string()
    } else {
        pin.branch.clone()
    }
}

fn pin_source_label(pin: &Pin) -> String {
    let repo = index_display_name_for_url(&pin.url);
    let branch = pin_branch_label(pin);
    format!("{}@{}", repo, branch)
}

fn pin_commit_date(output: &Output, pin: &Pin) -> String {
    match fetch_github_commit_date(&pin.url, &pin.rev) {
        Ok(date) => date,
        Err(err) => {
            output.warn(format!(
                "warning: failed to fetch commit date for {}@{}: {}",
                pin.url, pin.rev, err
            ));
            let fallback = pin.updated.and_hms_opt(0, 0, 0).unwrap();
            chrono::DateTime::<Utc>::from_naive_utc_and_offset(fallback, Utc).to_rfc3339()
        }
    }
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
    pinned: &BTreeMap<String, PinnedPackage>,
) -> Result<Vec<String>, CliError> {
    if presets.is_empty() {
        return Ok(packages
            .into_iter()
            .filter(|pkg| !pinned.contains_key(pkg))
            .collect());
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
        .filter(|pkg| !preset_packages.contains(pkg) && !pinned.contains_key(pkg))
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

fn cache_dir() -> Result<PathBuf, CliError> {
    Ok(config_dir()?.join("cache"))
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
    Ok(cache_dir()?.join("index.db"))
}

fn versions_db_path() -> Result<PathBuf, CliError> {
    Ok(cache_dir()?.join("versions.db"))
}

fn home_dir() -> Result<PathBuf, CliError> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| CliError::MissingHome)
}

fn print_project_state(output: &Output, state: &ProjectState) {
    output.info("mode: project");
    output.info(format!("pin: {} @ {}", state.pin.url, state.pin.rev));
    if !state.pins.is_empty() {
        output.info("pins:");
        for (name, pin) in &state.pins {
            output.info(format!("  {} -> {} ({})", name, pin.url, pin.rev));
        }
    }
    output.info(format!("presets: {}", state.presets.active.join(", ")));
    output.info(format!(
        "packages (added): {}",
        state.packages.added.join(", ")
    ));
    output.info(format!(
        "packages (removed): {}",
        state.packages.removed.join(", ")
    ));
    if !state.packages.pinned.is_empty() {
        output.info("packages (pinned):");
        for (name, pinned) in &state.packages.pinned {
            output.info(format!(
                "  {} -> {} ({})",
                name, pinned.version, pinned.pin.rev
            ));
        }
    }
    if !state.env.is_empty() {
        output.info("env:");
        for (key, value) in &state.env {
            let display = env_value_for_editor(value);
            let suffix =
                if env_value_mode_from_stored(value) == tui::app::EnvValueMode::NixExpression {
                    " [expr]"
                } else {
                    ""
                };
            output.info(format!("  {}={}{}", key, display, suffix));
        }
    }
    if let Some(hook) = &state.shell.hook {
        output.info("shellHook:");
        output.info(hook);
    }
}

fn print_profile_state(output: &Output, state: &GlobalProfileState) {
    output.info("mode: global");
    output.info(format!("pin: {} @ {}", state.pin.url, state.pin.rev));
    output.info(format!("presets: {}", state.presets.active.join(", ")));
    output.info(format!(
        "packages (added): {}",
        state.packages.added.join(", ")
    ));
    output.info(format!(
        "packages (removed): {}",
        state.packages.removed.join(", ")
    ));
    if !state.packages.pinned.is_empty() {
        output.info("packages (pinned):");
        for (name, pinned) in &state.packages.pinned {
            output.info(format!(
                "  {} -> {} ({})",
                name, pinned.version, pinned.pin.rev
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        encode_env_editor_value, env_value_for_editor, env_value_mode_from_stored,
        parse_github_repo, resolve_remote_index_urls, should_retry_default_branch_lookup, Cli,
        CliError, Command, IndexCommand,
    };
    use chrono::NaiveDate;
    use clap::Parser;
    use mica_core::state::NIX_EXPR_PREFIX;
    use std::path::PathBuf;

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

    #[test]
    fn cli_accepts_no_subcommand_for_tui_default() {
        let cli = Cli::try_parse_from(["mica"]).expect("parse failed");
        assert!(cli.command.is_none());
    }

    #[test]
    fn cli_parses_presets_subcommand() {
        let cli = Cli::try_parse_from(["mica", "presets"]).expect("parse failed");
        assert!(matches!(cli.command, Some(Command::Presets)));
    }

    #[test]
    fn cli_parses_index_rebuild_local_subcommand() {
        let cli = Cli::try_parse_from([
            "mica",
            "index",
            "rebuild-local",
            "/tmp/nix",
            "--skip-attr",
            "foo,bar",
            "--show-trace",
        ])
        .expect("parse failed");
        match cli.command {
            Some(Command::Index { command }) => match command {
                IndexCommand::RebuildLocal {
                    repo,
                    skip_attr,
                    show_trace,
                    ..
                } => {
                    assert_eq!(repo, PathBuf::from("/tmp/nix"));
                    assert_eq!(skip_attr, vec!["foo".to_string(), "bar".to_string()]);
                    assert!(show_trace);
                }
                _ => panic!("expected rebuild-local"),
            },
            _ => panic!("expected index command"),
        }
    }

    #[test]
    fn resolve_remote_index_urls_prefers_commit_then_fallback() {
        let urls = resolve_remote_index_urls("https://static.g7c.us/mica", Some("abcd1234"));
        assert_eq!(
            urls,
            vec![
                "https://static.g7c.us/mica/abcd1234.db".to_string(),
                "https://static.g7c.us/mica/index.db".to_string()
            ]
        );
    }

    #[test]
    fn resolve_remote_index_urls_keeps_explicit_db_url() {
        let urls = resolve_remote_index_urls("https://static.g7c.us/mica/index.db", Some("abcd"));
        assert_eq!(
            urls,
            vec!["https://static.g7c.us/mica/index.db".to_string()]
        );
    }

    #[test]
    fn index_info_falls_back_to_primary_pin_when_meta_is_unknown() {
        let info = crate::tui::app::IndexInfo {
            url: "unknown".to_string(),
            rev: "unknown".to_string(),
            count: None,
            generated_at: None,
            displayed_count: None,
        };
        let pins = vec![crate::IndexPin {
            name: None,
            pin: mica_core::state::Pin {
                name: None,
                url: "https://github.com/jpetrucciani/nix".to_string(),
                rev: "004391ff727d67a4f2e41590b0e8430a306d6688".to_string(),
                sha256: "sha256-test".to_string(),
                branch: "main".to_string(),
                updated: NaiveDate::from_ymd_opt(2026, 2, 8).expect("valid date"),
            },
        }];

        let merged = crate::index_info_with_pin_fallback(info, &pins);
        assert_eq!(merged.url, "https://github.com/jpetrucciani/nix");
        assert_eq!(merged.rev, "004391ff727d67a4f2e41590b0e8430a306d6688");
    }

    #[test]
    fn retry_default_branch_lookup_when_commit_is_missing_for_sha() {
        let body = r#"{"message":"No commit found for SHA: main"}"#;
        assert!(should_retry_default_branch_lookup(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            body
        ));
    }

    #[test]
    fn does_not_retry_default_branch_lookup_for_other_errors() {
        let body = r#"{"message":"Validation Failed"}"#;
        assert!(!should_retry_default_branch_lookup(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            body
        ));
        assert!(!should_retry_default_branch_lookup(
            reqwest::StatusCode::NOT_FOUND,
            r#"{"message":"Not Found"}"#
        ));
    }

    #[test]
    fn env_expression_values_round_trip_through_editor_helpers() {
        let stored = format!("{}${{pkgs.path}}/meme", NIX_EXPR_PREFIX);
        assert_eq!(
            env_value_mode_from_stored(&stored),
            crate::tui::app::EnvValueMode::NixExpression
        );
        assert_eq!(env_value_for_editor(&stored), "${pkgs.path}/meme");
    }

    #[test]
    fn encode_env_editor_value_marks_nix_expressions() {
        let encoded = encode_env_editor_value(
            "pkgs.path + \"/meme\"",
            crate::tui::app::EnvValueMode::NixExpression,
        )
        .expect("encode should succeed");
        assert_eq!(encoded, format!("{}pkgs.path + \"/meme\"", NIX_EXPR_PREFIX));
    }

    #[test]
    fn encode_env_editor_value_rejects_empty_expression() {
        let result = encode_env_editor_value("   ", crate::tui::app::EnvValueMode::NixExpression);
        assert!(result.is_err());
    }
}
