use mica_core::config::SearchMode;
use mica_core::state::{Pin, PinnedPackage};
use ratatui::widgets::{ListState, TableState};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct PackageEntry {
    pub attr_path: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub platforms: Option<String>,
    pub main_program: Option<String>,
    pub position: Option<String>,
    pub broken: bool,
    pub insecure: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PackageFilters {
    pub show_broken: bool,
    pub show_insecure: bool,
    pub license: String,
    pub platform: String,
    pub show_installed_only: bool,
}

impl PackageFilters {
    pub fn matches(&self, pkg: &PackageEntry) -> bool {
        if !self.show_broken && pkg.broken {
            return false;
        }
        if !self.show_insecure && pkg.insecure {
            return false;
        }
        if !self.license.is_empty() {
            let haystack = pkg.license.as_deref().unwrap_or("");
            if !contains_case_insensitive(haystack, &self.license) {
                return false;
            }
        }
        if !self.platform.is_empty() {
            let haystack = pkg.platforms.as_deref().unwrap_or("");
            if !contains_case_insensitive(haystack, &self.platform) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct PresetEntry {
    pub name: String,
    pub description: String,
    pub order: i32,
    pub packages_required: Vec<String>,
    pub packages_optional: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct IndexInfo {
    pub url: String,
    pub rev: String,
    pub count: Option<usize>,
    pub generated_at: Option<String>,
    pub displayed_count: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Packages,
    Presets,
    Changes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Version,
    Description,
    License,
    Platforms,
    MainProgram,
}

#[derive(Debug, Clone, Copy)]
pub struct ColumnOption {
    pub kind: ColumnKind,
    pub label: &'static str,
}

pub const COLUMN_OPTIONS: [ColumnOption; 5] = [
    ColumnOption {
        kind: ColumnKind::Version,
        label: "Version",
    },
    ColumnOption {
        kind: ColumnKind::Description,
        label: "Description",
    },
    ColumnOption {
        kind: ColumnKind::License,
        label: "License",
    },
    ColumnOption {
        kind: ColumnKind::Platforms,
        label: "Platforms",
    },
    ColumnOption {
        kind: ColumnKind::MainProgram,
        label: "Main program",
    },
];

#[derive(Debug, Clone, Copy)]
pub struct ColumnSettings {
    pub show_version: bool,
    pub show_description: bool,
    pub show_license: bool,
    pub show_platforms: bool,
    pub show_main_program: bool,
}

impl Default for ColumnSettings {
    fn default() -> Self {
        Self {
            show_version: true,
            show_description: true,
            show_license: false,
            show_platforms: false,
            show_main_program: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Project,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    License,
    Platform,
}

#[derive(Debug, Clone)]
pub struct FilterEditorState {
    pub kind: FilterKind,
    pub input: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvEditMode {
    List,
    Edit {
        original_key: Option<String>,
        value_mode: EnvValueMode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvValueMode {
    String,
    NixExpression,
}

impl EnvValueMode {
    pub fn toggle(self) -> Self {
        match self {
            EnvValueMode::String => EnvValueMode::NixExpression,
            EnvValueMode::NixExpression => EnvValueMode::String,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct EnvEditorState {
    pub entries: Vec<EnvEntry>,
    pub cursor: usize,
    pub input: String,
    pub input_cursor: usize,
    pub mode: EnvEditMode,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ShellEditorState {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub original: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DiffViewerState {
    pub full_lines: Vec<String>,
    pub change_lines: Vec<String>,
    pub show_full: bool,
    pub scroll: usize,
}

#[derive(Debug, Clone)]
pub struct PackageInfoState {
    pub lines: Vec<String>,
    pub scroll: usize,
}

#[derive(Debug, Clone)]
pub struct VersionPickerEntry {
    pub source: String,
    pub version: String,
    pub commit: String,
    pub commit_date: String,
    pub branch: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct VersionPickerState {
    pub entries: Vec<VersionPickerEntry>,
    pub cursor: usize,
    pub package: String,
}

#[derive(Debug, Clone)]
pub struct ColumnsEditorState {
    pub cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinField {
    Name,
    Url,
    Branch,
    Rev,
    Sha256,
    TarballName,
}

pub const PIN_FIELDS: [PinField; 6] = [
    PinField::Name,
    PinField::Url,
    PinField::Branch,
    PinField::Rev,
    PinField::Sha256,
    PinField::TarballName,
];

#[derive(Debug, Clone)]
pub struct PinEditorState {
    pub name: String,
    pub name_cursor: usize,
    pub url: String,
    pub url_cursor: usize,
    pub branch: String,
    pub branch_cursor: usize,
    pub rev: String,
    pub rev_cursor: usize,
    pub sha256: String,
    pub sha256_cursor: usize,
    pub tarball_name: String,
    pub tarball_name_cursor: usize,
    pub active: PinField,
    pub use_latest: bool,
    pub error: Option<String>,
}

impl PinEditorState {
    pub fn new(default_url: String, default_branch: String) -> Self {
        let branch = if default_branch.trim().is_empty() {
            "main".to_string()
        } else {
            default_branch
        };
        let url_cursor = default_url.len();
        let branch_cursor = branch.len();
        Self {
            name: String::new(),
            name_cursor: 0,
            url: default_url,
            url_cursor,
            branch,
            branch_cursor,
            rev: String::new(),
            rev_cursor: 0,
            sha256: String::new(),
            sha256_cursor: 0,
            tarball_name: String::new(),
            tarball_name_cursor: 0,
            active: PinField::Name,
            use_latest: true,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ToastLevel {
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub level: ToastLevel,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
pub enum Overlay {
    Help,
    PackageInfo(PackageInfoState),
    VersionPicker(VersionPickerState),
    PinEditor(PinEditorState),
    Columns(ColumnsEditorState),
    Env(EnvEditorState),
    Shell(ShellEditorState),
    Filter(FilterEditorState),
    Diff(DiffViewerState),
}

#[derive(Debug)]
pub struct App {
    pub mode: AppMode,
    pub project_dir: Option<String>,
    pub query: String,
    pub preset_query: String,
    pub cursor: usize,
    pub packages: Vec<PackageEntry>,
    pub preset_cursor: usize,
    pub focus: Focus,
    pub presets: Vec<PresetEntry>,
    pub preset_filtered: Vec<usize>,
    pub presets_collapsed: bool,
    pub changes_collapsed: bool,
    pub columns: ColumnSettings,
    pub show_details: bool,
    pub pinned: BTreeMap<String, PinnedPackage>,
    pub base_pinned: BTreeMap<String, PinnedPackage>,
    pub pin_map: BTreeMap<String, Pin>,
    pub added: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    pub active_presets: BTreeSet<String>,
    pub preset_packages: BTreeSet<String>,
    pub env: BTreeMap<String, String>,
    pub shell_hook: Option<String>,
    pub base_added: BTreeSet<String>,
    pub base_removed: BTreeSet<String>,
    pub base_presets: BTreeSet<String>,
    pub base_env: BTreeMap<String, String>,
    pub base_shell_hook: Option<String>,
    pub filters: PackageFilters,
    pub search_mode: SearchMode,
    pub packages_state: TableState,
    pub presets_state: ListState,
    pub overlay: Option<Overlay>,
    pub index_info: IndexInfo,
    pub toast: Option<Toast>,
    pub dirty: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(packages: Vec<PackageEntry>, presets: Vec<PresetEntry>) -> App {
        let mut app = App {
            mode: AppMode::Project,
            project_dir: None,
            query: String::new(),
            preset_query: String::new(),
            cursor: 0,
            packages,
            preset_cursor: 0,
            focus: Focus::Packages,
            presets,
            preset_filtered: Vec::new(),
            presets_collapsed: true,
            changes_collapsed: false,
            columns: ColumnSettings::default(),
            show_details: true,
            pinned: BTreeMap::new(),
            base_pinned: BTreeMap::new(),
            pin_map: BTreeMap::new(),
            added: BTreeSet::new(),
            removed: BTreeSet::new(),
            active_presets: BTreeSet::new(),
            preset_packages: BTreeSet::new(),
            env: BTreeMap::new(),
            shell_hook: None,
            base_added: BTreeSet::new(),
            base_removed: BTreeSet::new(),
            base_presets: BTreeSet::new(),
            base_env: BTreeMap::new(),
            base_shell_hook: None,
            filters: PackageFilters::default(),
            search_mode: SearchMode::All,
            packages_state: TableState::new(),
            presets_state: ListState::default(),
            overlay: None,
            index_info: IndexInfo::default(),
            toast: None,
            dirty: false,
            should_quit: false,
        };
        if !app.packages.is_empty() {
            app.packages_state.select(Some(0));
        }
        app.refresh_preset_filter();
        app
    }

    pub fn effective_package_count(&self) -> usize {
        let mut packages = self.preset_packages.clone();
        for pkg in &self.added {
            packages.insert(pkg.clone());
        }
        for pkg in &self.removed {
            packages.remove(pkg);
        }
        for pkg in self.pinned.keys() {
            packages.insert(pkg.clone());
        }
        packages.len()
    }

    pub fn cycle_search_mode(&mut self) {
        self.search_mode = match self.search_mode {
            SearchMode::All => SearchMode::Name,
            SearchMode::Name => SearchMode::Description,
            SearchMode::Description => SearchMode::Binary,
            SearchMode::Binary => SearchMode::All,
        };
    }

    pub fn search_mode_label(&self) -> &'static str {
        match self.search_mode {
            SearchMode::All => "all",
            SearchMode::Name => "name",
            SearchMode::Description => "desc",
            SearchMode::Binary => "bin",
        }
    }

    pub fn is_installed(&self, name: &str) -> bool {
        let base = self.base_attr_for(name);
        let installed = self.preset_packages.contains(&base)
            || self.added.contains(&base)
            || self.pinned.contains_key(&base);
        installed && !self.removed.contains(&base)
    }

    pub fn base_attr_for(&self, attr_path: &str) -> String {
        for prefix in self.pin_map.keys() {
            let needle = format!("{}.", prefix);
            if attr_path.starts_with(&needle) {
                return attr_path[needle.len()..].to_string();
            }
        }
        attr_path.to_string()
    }

    pub fn pin_for_attr(&self, attr_path: &str) -> Option<(String, Pin)> {
        for (prefix, pin) in &self.pin_map {
            let needle = format!("{}.", prefix);
            if attr_path.starts_with(&needle) {
                let base = attr_path[needle.len()..].to_string();
                return Some((base, pin.clone()));
            }
        }
        None
    }

    pub fn current_package(&self) -> Option<&PackageEntry> {
        self.packages.get(self.cursor)
    }

    pub fn toggle_column(&mut self, column: ColumnKind) {
        match column {
            ColumnKind::Version => self.columns.show_version = !self.columns.show_version,
            ColumnKind::Description => {
                self.columns.show_description = !self.columns.show_description
            }
            ColumnKind::License => self.columns.show_license = !self.columns.show_license,
            ColumnKind::Platforms => self.columns.show_platforms = !self.columns.show_platforms,
            ColumnKind::MainProgram => {
                self.columns.show_main_program = !self.columns.show_main_program
            }
        }
    }

    pub fn next(&mut self) {
        match self.focus {
            Focus::Packages => {
                if self.packages.is_empty() {
                    self.cursor = 0;
                    self.packages_state.select(None);
                    return;
                }
                self.cursor = (self.cursor + 1).min(self.packages.len() - 1);
                self.packages_state.select(Some(self.cursor));
            }
            Focus::Presets => {
                if self.preset_filtered.is_empty() {
                    self.preset_cursor = 0;
                    self.presets_state.select(None);
                    return;
                }
                self.preset_cursor = (self.preset_cursor + 1).min(self.preset_filtered.len() - 1);
                self.presets_state.select(Some(self.preset_cursor));
            }
            Focus::Changes => {}
        }
    }

    pub fn prev(&mut self) {
        match self.focus {
            Focus::Packages => {
                if self.packages.is_empty() {
                    self.cursor = 0;
                    self.packages_state.select(None);
                    return;
                }
                if self.cursor == 0 {
                    return;
                }
                self.cursor -= 1;
                self.packages_state.select(Some(self.cursor));
            }
            Focus::Presets => {
                if self.preset_filtered.is_empty() {
                    self.preset_cursor = 0;
                    self.presets_state.select(None);
                    return;
                }
                if self.preset_cursor == 0 {
                    return;
                }
                self.preset_cursor -= 1;
                self.presets_state.select(Some(self.preset_cursor));
            }
            Focus::Changes => {}
        }
    }

    pub fn toggle_current(&mut self) {
        match self.focus {
            Focus::Packages => self.toggle_current_package(),
            Focus::Presets => self.toggle_current_preset(),
            Focus::Changes => {}
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Packages => Focus::Presets,
            Focus::Presets => Focus::Changes,
            Focus::Changes => Focus::Packages,
        };
    }

    pub fn rebuild_preset_packages(&mut self) {
        self.preset_packages.clear();
        for preset in &self.presets {
            if self.active_presets.contains(&preset.name) {
                for pkg in &preset.packages_required {
                    self.preset_packages.insert(pkg.clone());
                }
            }
        }
    }

    fn toggle_current_package(&mut self) {
        if let Some(entry) = self.packages.get(self.cursor) {
            if let Some((base, pin)) = self.pin_for_attr(&entry.attr_path) {
                if self.pinned.remove(&base).is_none() {
                    let version = entry
                        .version
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    self.pinned
                        .insert(base.clone(), PinnedPackage { version, pin });
                    self.added.remove(&base);
                    self.removed.remove(&base);
                }
                self.update_dirty();
                return;
            }

            let base = self.base_attr_for(&entry.attr_path);
            if self.pinned.remove(&base).is_some() {
                if self.preset_packages.contains(&base) {
                    self.removed.insert(base.clone());
                }
                self.update_dirty();
                return;
            }

            if self.preset_packages.contains(&base) {
                if self.removed.contains(&base) {
                    self.removed.remove(&base);
                } else {
                    self.removed.insert(base.clone());
                    self.added.remove(&base);
                }
            } else if self.added.contains(&base) {
                self.added.remove(&base);
            } else {
                self.added.insert(base);
            }
            self.update_dirty();
        }
    }

    fn toggle_current_preset(&mut self) {
        if let Some(index) = self.preset_filtered.get(self.preset_cursor).copied() {
            if let Some(entry) = self.presets.get(index) {
                if self.active_presets.contains(&entry.name) {
                    self.active_presets.remove(&entry.name);
                } else {
                    self.active_presets.insert(entry.name.clone());
                }
                self.rebuild_preset_packages();
                self.update_dirty();
            }
        }
    }

    pub fn commit_baseline(&mut self) {
        self.base_added = self.added.clone();
        self.base_removed = self.removed.clone();
        self.base_presets = self.active_presets.clone();
        self.base_env = self.env.clone();
        self.base_shell_hook = self.shell_hook.clone();
        self.base_pinned = self.pinned.clone();
        self.dirty = false;
    }

    pub fn update_dirty(&mut self) {
        self.dirty = self.added != self.base_added
            || self.removed != self.base_removed
            || self.active_presets != self.base_presets
            || self.env != self.base_env
            || self.shell_hook != self.base_shell_hook
            || self.pinned != self.base_pinned;
    }

    pub fn push_toast(&mut self, level: ToastLevel, message: impl Into<String>) {
        self.toast = Some(Toast {
            message: message.into(),
            level,
            expires_at: Instant::now() + Duration::from_secs(3),
        });
    }

    pub fn clear_expired_toast(&mut self) {
        let expired = match &self.toast {
            Some(toast) => Instant::now() >= toast.expires_at,
            None => false,
        };
        if expired {
            self.toast = None;
        }
    }

    pub fn refresh_preset_filter(&mut self) {
        let needle = self.preset_query.trim().to_lowercase();
        self.preset_filtered = self
            .presets
            .iter()
            .enumerate()
            .filter(|(_, preset)| {
                if needle.is_empty() {
                    return true;
                }
                preset.name.to_lowercase().contains(&needle)
                    || preset.description.to_lowercase().contains(&needle)
            })
            .map(|(idx, _)| idx)
            .collect();
        if self.preset_cursor >= self.preset_filtered.len() {
            self.preset_cursor = 0;
        }
        if self.preset_filtered.is_empty() {
            self.presets_state.select(None);
        } else {
            self.presets_state.select(Some(self.preset_cursor));
        }
    }

    pub fn current_preset(&self) -> Option<&PresetEntry> {
        self.preset_filtered
            .get(self.preset_cursor)
            .and_then(|idx| self.presets.get(*idx))
    }
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}
