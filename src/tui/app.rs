use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::path::{Path, PathBuf};

use super::catalog::{self, FieldKind, FieldState};
use super::operations::{self, Action, Data, Review};
use super::theme::{self, Tone};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Installed,
    Discover,
    Updates,
    Commands,
    System,
}

impl View {
    pub const ALL: [Self; 5] = [
        Self::Installed,
        Self::Discover,
        Self::Updates,
        Self::Commands,
        Self::System,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Installed => "Installed",
            Self::Discover => "Discover",
            Self::Updates => "Updates",
            Self::Commands => "Commands",
            Self::System => "System",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Installed => 0,
            Self::Discover => 1,
            Self::Updates => 2,
            Self::Commands => 3,
            Self::System => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeFilter {
    All,
    Plugin,
    Module,
    Script,
    Completion,
}

impl TypeFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Plugin,
            Self::Plugin => Self::Module,
            Self::Module => Self::Script,
            Self::Script => Self::Completion,
            Self::Completion => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Plugin => "plugin",
            Self::Module => "module",
            Self::Script => "script",
            Self::Completion => "completion",
        }
    }

    pub fn matches(self, kind: &str) -> bool {
        self == Self::All || self.label() == kind
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Nushell,
    Pwsh,
    Posix,
}

impl Shell {
    pub fn next(self) -> Self {
        match self {
            Self::Nushell => Self::Pwsh,
            Self::Pwsh => Self::Posix,
            Self::Posix => Self::Nushell,
        }
    }
    pub fn quote(self, value: &str) -> String {
        match self {
            Self::Nushell => format!(
                "\"{}\"",
                value
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
                    .replace('\t', "\\t")
                    .replace('$', "\\$")
            ),
            Self::Pwsh => format!("'{}'", value.replace('\'', "''")),
            Self::Posix => format!("'{}'", value.replace('\'', "'\"'\"'")),
        }
    }
}

#[derive(Clone)]
pub enum Command {
    Sync,
    /// Run a catalog command through `operations::execute_cli`. The name is
    /// deliberately not "read-only": reviewed mutating commands use it too.
    ExecuteCli(Vec<String>),
    Reload,
    Prepare(Action, String, String, Option<String>),
    Execute(Review),
    Scan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentRun {
    pub argv: Vec<String>,
    pub display: String,
    pub note: String,
    pub read_only: bool,
}

#[derive(Clone)]
pub struct FollowUp {
    pub key: char,
    pub label: String,
    pub command: Box<Command>,
}

pub struct Row {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub state: String,
    pub registry: String,
    pub details: String,
}

#[derive(Clone)]
pub enum LauncherAction {
    Navigate {
        view: View,
        select_id: Option<String>,
        include_incompatible: bool,
    },
    Native(Action, String, String, Option<String>),
    OpenForm {
        display: String,
        prefill_pkg: Option<String>,
    },
    ExecuteCli(Vec<String>),
    OpenSync,
    Cli(Vec<String>),
}

#[derive(Clone)]
pub struct LauncherItem {
    pub label: String,
    pub sub: String,
    pub tag: String,
    pub tone: Tone,
    pub score: i64,
    pub action: LauncherAction,
}

pub enum Overlay {
    Actions {
        selected: usize,
    },
    Sync,
    Filter(String),
    Launcher {
        query: String,
        selected: usize,
    },
    Review {
        review: Review,
        confirm: bool,
        scroll: u16,
    },
    Text {
        title: String,
        content: String,
        scroll: u16,
        follow_up: Option<FollowUp>,
    },
    Handoff {
        args: Vec<String>,
        shell: Shell,
    },
    CommandForm(Box<CommandForm>),
    GenericReview {
        argv: Vec<String>,
        display: String,
        effects: Vec<String>,
        confirm: bool,
        scroll: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    Field(usize),
    Preview,
}

pub struct CommandForm {
    pub display: String,
    pub fields: Vec<FieldState>,
    pub selected: usize,
    pub editing: Option<EditTarget>,
    pub edit_buffer: String,
    pub edit_cursor: usize,
    pub error: Option<String>,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailClick {
    Prepare(Action),
    Scan,
    Sync,
    RollbackSnapshot(String),
    CycleVersion,
    BatchUpdate,
}

#[derive(Clone)]
pub enum ModalClick {
    Cancel,
    Confirm,
    FollowUp(Command),
    Close,
    CycleShell,
    SwitchConfirm(bool),
    FieldRow(usize),
}

#[derive(Default, Clone)]
pub struct LayoutRegions {
    pub tabs: [Rect; 5],
    pub filter_chip: Option<Rect>,
    pub clear_filter_chip: Option<Rect>,
    pub pane_toggle_chip: Option<Rect>,
    pub type_chip: Option<Rect>,
    pub compat_chip: Option<Rect>,
    pub batch_update_chip: Option<Rect>,
    pub list_pane: Option<Rect>,
    pub list_rows_area: Option<Rect>,
    pub list_offset: usize,
    pub details_pane: Option<Rect>,
    pub detail_buttons: Vec<(Rect, DetailClick)>,
    pub modal_rect: Option<Rect>,
    pub modal_close_rect: Option<Rect>,
    pub modal_list_area: Option<(Rect, usize)>,
    pub modal_buttons: Vec<(Rect, ModalClick)>,
}

fn contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

pub struct App {
    pub root: PathBuf,
    pub data: Option<Data>,
    pub view: View,
    pub focus: Focus,
    pub selected: usize,
    pub list_offset: usize,
    pub filter: String,
    pub type_filter: TypeFilter,
    pub show_all: bool,
    pub pinned: Option<(String, String)>,
    pub overlay: Option<Overlay>,
    pub detail_scroll: u16,
    pub busy: bool,
    pub spinner_tick: usize,
    pub download_progress: Option<(u64, Option<u64>)>,
    pub status: String,
    pub logs: Vec<String>,
    pub doctor_report: Option<String>,
    pub recent_runs: Vec<RecentRun>,
    pub active_op: Option<Command>,
    pub exit: bool,
    pub exit_command: Option<String>,
    pub small: bool,
    pub wide: bool,
    pub command_selected: usize,
    pub last_package_id: Option<String>,
    pub regions: LayoutRegions,
}

pub fn clean(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

pub fn fuzzy_score(hay: &str, needle: &str) -> Option<i64> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Some(1);
    }
    let h = hay.to_lowercase();
    let n = needle.to_lowercase();
    if let Some(idx) = h.find(&n) {
        let idx_chars = h[..idx].chars().count() as i64;
        let len_diff = (h.chars().count().saturating_sub(n.chars().count())) as i64;
        return Some(2000 - idx_chars * 4 - len_diff);
    }
    let mut hi = h.chars();
    let mut score = 0i64;
    let mut run = 0i64;
    for ch in n.chars() {
        let mut found = false;
        for hc in hi.by_ref() {
            if hc == ch {
                run += 1;
                score += 10 + run * 4;
                found = true;
                break;
            }
            run = 0;
        }
        if !found {
            return None;
        }
    }
    Some(score - (h.chars().count() as i64) / 4)
}

fn char_to_byte(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(text.len())
}

fn insert_char_at(text: &mut String, cursor: &mut usize, c: char) {
    let byte_idx = char_to_byte(text, *cursor);
    text.insert(byte_idx, c);
    *cursor += 1;
}

fn remove_before_cursor(text: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = char_to_byte(text, *cursor - 1);
    let end = char_to_byte(text, *cursor);
    text.replace_range(start..end, "");
    *cursor -= 1;
}

fn remove_at_cursor(text: &mut String, cursor: usize) {
    let len = text.chars().count();
    if cursor >= len {
        return;
    }
    let start = char_to_byte(text, cursor);
    let end = char_to_byte(text, cursor + 1);
    text.replace_range(start..end, "");
}

impl App {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.into(),
            data: None,
            view: View::Installed,
            focus: Focus::List,
            selected: 0,
            list_offset: 0,
            filter: String::new(),
            type_filter: TypeFilter::All,
            show_all: false,
            pinned: None,
            overlay: None,
            detail_scroll: 0,
            busy: false,
            spinner_tick: 0,
            download_progress: None,
            status: "Loading local state; no network on view entry.".into(),
            logs: Vec::new(),
            doctor_report: None,
            recent_runs: Vec::new(),
            active_op: None,
            exit: false,
            exit_command: None,
            small: false,
            wide: true,
            command_selected: 0,
            last_package_id: None,
            regions: LayoutRegions::default(),
        }
    }

    pub fn record_recent(&mut self, argv: Vec<String>, note: String, read_only: bool) {
        if argv.is_empty() {
            return;
        }
        let display = argv.join(" ");
        self.recent_runs.retain(|r| r.display != display);
        self.recent_runs.insert(
            0,
            RecentRun {
                argv,
                display,
                note: clean(&note),
                read_only,
            },
        );
        if self.recent_runs.len() > 6 {
            self.recent_runs.truncate(6);
        }
    }

    pub fn count_for_view(&self, view: View) -> Option<usize> {
        let data = self.data.as_ref()?;
        match view {
            View::Installed => Some(data.installed.packages.len()),
            View::Discover => Some(data.catalog.len()),
            View::Updates => {
                let nu = data.nu_valid.then_some(data.nu.as_ref()).flatten();
                let default = &data.default_registry;
                let count = data
                    .installed
                    .packages
                    .iter()
                    .filter(|(id, entry)| {
                        let registry = operations::registry_for(entry, default);
                        let item = data.catalog.iter().find(|item| {
                            item.registry == registry && item.package.id.to_string() == **id
                        });
                        item.and_then(|item| operations::candidate(item, nu))
                            .is_some_and(|v| {
                                match (
                                    v.parse::<semver::Version>(),
                                    entry.version.parse::<semver::Version>(),
                                ) {
                                    (Ok(next), Ok(now)) => next > now,
                                    _ => false,
                                }
                            })
                    })
                    .count();
                Some(count)
            }
            View::Commands => Some(catalog::catalog().len()),
            View::System => None,
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let Some(data) = &self.data else {
            return Vec::new();
        };
        if self.view == View::System {
            return self.system_rows(data);
        }
        let default = &data.default_registry;
        let nu = data.nu_valid.then_some(data.nu.as_ref()).flatten();
        let mut rows = Vec::new();
        if self.view == View::Discover {
            for item in &data.catalog {
                let compatible = operations::candidate(item, nu);
                if compatible.is_none() && !self.show_all && data.nu_valid {
                    continue;
                }
                let kind = item.package.package_type.to_string();
                if !self.type_filter.matches(&kind) {
                    continue;
                }
                let id = item.package.id.to_string();
                let version = self
                    .pinned
                    .as_ref()
                    .filter(|(key, _)| key == &id)
                    .map(|(_, v)| v.clone())
                    .or(compatible.clone())
                    .or_else(|| item.package.versions.first().map(|v| v.version.to_string()))
                    .unwrap_or_default();
                let selected_entry = item
                    .package
                    .versions
                    .iter()
                    .find(|v| v.version.to_string() == version)
                    .or_else(|| item.package.versions.first());
                let state = if data.installed.packages.contains_key(&id) {
                    "installed"
                } else if selected_entry.is_some_and(|v| v.is_provisional()) && compatible.is_some()
                {
                    "provisional"
                } else if compatible.is_some() {
                    "compatible"
                } else if !data.nu_valid {
                    "not checked"
                } else {
                    "incompatible"
                };
                let versions = item
                    .package
                    .versions
                    .iter()
                    .map(|v| v.version.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let nu_range = selected_entry
                    .map(|v| v.nu_version.clone())
                    .unwrap_or_else(|| "unknown".into());
                let verified_with = selected_entry
                    .map(|v| {
                        if v.verified_with.is_empty() {
                            v.deferral_reason_display()
                        } else {
                            format!("Nu {}", v.verified_with.join(", "))
                        }
                    })
                    .unwrap_or_else(|| "unverified".into());
                let tags = if item.package.tags.is_empty() {
                    "none".into()
                } else {
                    item.package.tags.join(", ")
                };
                let install_only = operations::install_only_note_for_package(&item.package)
                    .map(|note| format!("\n\n{note}"))
                    .unwrap_or_default();
                rows.push(Row {
                    id,
                    version,
                    kind,
                    state: state.into(),
                    registry: item.registry.clone(),
                    details: format!(
                        "{}\n\nNu range: {}\nProof: {}\nTags: {}\nRepo: {}\nRegistry: {}\nSigning key: {}\nIndex SHA256: {}\n\nVersions: {versions}{install_only}\n\nCompatibility is metadata-based, not runtime proof.\n[v] Choose version; [t] Filter type; [c] Include incompatible.",
                        item.package.description,
                        nu_range,
                        verified_with,
                        tags,
                        item.package.repo,
                        item.registry,
                        item.signing_key,
                        item.index_digest
                    ),
                });
            }
        } else {
            for (id, entry) in &data.installed.packages {
                let registry = operations::registry_for(entry, default);
                let item = data
                    .catalog
                    .iter()
                    .find(|item| item.registry == registry && item.package.id.to_string() == *id);
                let next = item.and_then(|item| operations::candidate(item, nu));
                let newer = next.as_ref().is_some_and(|v| {
                    match (
                        v.parse::<semver::Version>(),
                        entry.version.parse::<semver::Version>(),
                    ) {
                        (Ok(next), Ok(now)) => next > now,
                        _ => false,
                    }
                });
                if self.view == View::Updates && !newer {
                    continue;
                }
                let state = operations::activation(
                    entry,
                    if data.nu_valid {
                        data.nu.as_ref()
                    } else {
                        None
                    },
                )
                .to_string();
                let tags = item
                    .map(|i| i.package.tags.join(", "))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "none".into());
                let nu_range = item
                    .and_then(|i| {
                        i.package
                            .versions
                            .iter()
                            .find(|v| v.version.to_string() == entry.version)
                    })
                    .map(|v| v.nu_version.as_str())
                    .unwrap_or("not in cache");
                rows.push(Row {
                    id: id.clone(),
                    version: if self.view == View::Updates {
                        format!("{} > {}", entry.version, next.unwrap_or_default())
                    } else {
                        entry.version.clone()
                    },
                    kind: entry.package_type.clone(),
                    state,
                    registry,
                    details: format!(
                        "{}\n\nOrigin: {}\nPayload: {}\nNu range: {}\nTags: {}\nRevision: {}\nRecorded artifact SHA256: {}\n\nActivation is specific to the cached Nu identity. Future Nu sessions see activation changes.\n\nRecorded verification is not a fresh integrity check.",
                        item.map(|item| item.package.description.as_str())
                            .unwrap_or("Installed package"),
                        entry.origin.as_deref().unwrap_or("unknown"),
                        entry.payload_path(),
                        nu_range,
                        tags,
                        entry.revision_id.as_deref().unwrap_or("not recorded"),
                        entry.artifact_sha256.as_deref().unwrap_or("not recorded")
                    ),
                });
            }
        }
        let query = self.filter.to_lowercase();
        if !query.is_empty() {
            rows.retain(|row| {
                format!("{} {} {} {}", row.id, row.kind, row.state, row.details)
                    .to_lowercase()
                    .contains(&query)
            });
        }
        rows
    }

    fn system_rows(&self, data: &Data) -> Vec<Row> {
        let mut out = Vec::new();
        let identity_state = if data.nu_valid {
            "verified"
        } else if data.nu.is_some() {
            "stale"
        } else {
            "missing"
        };
        let nu_desc = data
            .nu
            .as_ref()
            .map(|nu| {
                format!(
                    "Root: {}\nNu: {}\nExecutable: {}\nSHA256: {}\nPlugin registry: {}\nVendor autoload dir: {}\nManaged autoload file: {}",
                    self.root.display(),
                    nu.nu_version,
                    nu.nu_executable,
                    nu.nu_executable_hash,
                    nu.plugin_registry_path,
                    nu.vendor_autoload_dir.as_deref().unwrap_or("none"),
                    data.managed_autoload_file.as_deref().unwrap_or("none")
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "Root: {}\nNu context unavailable; run `numan init` or `doctor --fix`.",
                    self.root.display()
                )
            });
        out.push(Row {
            id: "Identity".into(),
            version: data
                .nu
                .as_ref()
                .map(|nu| nu.nu_version.clone())
                .unwrap_or_else(|| "none".into()),
            kind: "info".into(),
            state: identity_state.into(),
            registry: data.default_registry.clone(),
            details: nu_desc,
        });

        let doctor_state = if let Some(report) = &self.doctor_report {
            if report.contains("0 errors, 0 warnings") {
                "clean"
            } else if report.contains("0 errors") {
                "warn"
            } else {
                "error"
            }
        } else {
            "idle"
        };
        let doctor_details = self.doctor_report.clone().unwrap_or_else(|| {
            "Inert on open (no writes, no repairs, no network probes).\n\nPress [s] to run a read-only local doctor scan."
                .into()
        });
        out.push(Row {
            id: "Doctor".into(),
            version: "--scan".into(),
            kind: "scan".into(),
            state: doctor_state.into(),
            registry: data.default_registry.clone(),
            details: doctor_details,
        });

        let snap_count = data.snapshots.len();
        let snap_state = if snap_count > 0 { "ready" } else { "empty" };
        let snap_details = if data.snapshots.is_empty() {
            "No snapshots stored yet.".into()
        } else {
            data.snapshots
                .iter()
                .map(|s| {
                    format!(
                        "{}  {:?}  {:?}  {} pkg(s)  {}",
                        s.id,
                        s.reason,
                        s.trigger,
                        s.payload_revisions.len(),
                        s.created_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        out.push(Row {
            id: "Snapshots".into(),
            version: format!("{snap_count}"),
            kind: "state".into(),
            state: snap_state.into(),
            registry: data.default_registry.clone(),
            details: snap_details,
        });

        let reg_count = data.registries.len();
        let reg_error = data.registries.iter().any(|r| r.error.is_some());
        let reg_state = if reg_error {
            "error"
        } else if reg_count == 0 {
            "none"
        } else {
            "Ed25519 OK"
        };
        let reg_details = if data.registries.is_empty() {
            "No registries configured.".into()
        } else {
            data.registries
                .iter()
                .map(|r| {
                    let status = if !r.enabled {
                        "disabled".into()
                    } else if let Some(err) = &r.error {
                        format!("error: {err}")
                    } else {
                        format!(
                            "verified ({} pkg(s), updated {}, sha256 {})",
                            r.cached_packages,
                            r.updated_at.as_deref().unwrap_or("unknown"),
                            r.index_digest
                                .as_deref()
                                .map(|d| &d[..8.min(d.len())])
                                .unwrap_or("none")
                        )
                    };
                    format!(
                        "{} [{}]\n  URL: {}\n  Key: {}",
                        r.name,
                        status,
                        if r.url.is_empty() { "(local)" } else { &r.url },
                        r.signing_key.as_deref().unwrap_or("none")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        out.push(Row {
            id: "Registries".into(),
            version: format!("{reg_count}"),
            kind: "trust".into(),
            state: reg_state.into(),
            registry: data.default_registry.clone(),
            details: reg_details,
        });

        let transcript_details = if self.logs.is_empty() {
            "No operation logs recorded yet.".into()
        } else {
            self.logs
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        };
        out.push(Row {
            id: "Transcript".into(),
            version: format!("{}", self.logs.len()),
            kind: "logs".into(),
            state: "live".into(),
            registry: data.default_registry.clone(),
            details: transcript_details,
        });

        let query = self.filter.to_lowercase();
        if !query.is_empty() {
            out.retain(|row| {
                format!("{} {} {}", row.id, row.kind, row.details)
                    .to_lowercase()
                    .contains(&query)
            });
        }
        out
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows().into_iter().nth(self.selected)
    }

    pub fn navigate(&mut self, view: View) {
        if let Some(row) = self.selected_row() {
            if matches!(self.view, View::Installed | View::Discover | View::Updates) {
                self.last_package_id = Some(row.id);
            }
        }
        self.command_selected = 0;
        self.view = view;
        self.selected = 0;
        self.list_offset = 0;
        self.filter.clear();
        self.pinned = None;
        self.detail_scroll = 0;
    }

    pub fn command_specs(&self) -> Vec<&'static catalog::CommandSpec> {
        let query = self.filter.to_lowercase();
        catalog::catalog()
            .iter()
            .filter(|spec| {
                query.is_empty()
                    || format!("{} {} {}", spec.display, spec.summary, spec.tier)
                        .to_lowercase()
                        .contains(&query)
            })
            .collect()
    }

    pub fn selected_command(&self) -> Option<&'static catalog::CommandSpec> {
        self.command_specs().into_iter().nth(self.command_selected)
    }

    pub fn command_preview(&self, form: &CommandForm) -> String {
        let Some(found) = catalog::spec(&form.display) else {
            return String::new();
        };
        self.handoff(&catalog::build_argv(found, &form.fields), Shell::Nushell)
    }

    pub fn picker_options(&self, form: &CommandForm, field_index: usize) -> Vec<String> {
        let Some(field) = form.fields.get(field_index) else {
            return Vec::new();
        };
        match field.spec.kind {
            FieldKind::Enum => field.spec.possible.clone(),
            FieldKind::PackageId => {
                let mut seen = Vec::new();
                if let Some(data) = &self.data {
                    for id in data.installed.packages.keys() {
                        if !seen.contains(id) {
                            seen.push(id.clone());
                        }
                    }
                    for item in &data.catalog {
                        let id = item.package.id.to_string();
                        if !seen.contains(&id) {
                            seen.push(id);
                        }
                    }
                }
                seen
            }
            FieldKind::SnapshotId => self
                .data
                .as_ref()
                .map(|d| d.snapshots.iter().map(|s| s.id.clone()).collect())
                .unwrap_or_default(),
            FieldKind::RegistryName => self
                .data
                .as_ref()
                .map(|d| d.registries.iter().map(|r| r.name.clone()).collect())
                .unwrap_or_default(),
            FieldKind::Version => {
                if form.display == "use" {
                    return vec!["list".into(), "latest".into()];
                }
                let pkg_id = form
                    .fields
                    .iter()
                    .find(|f| f.spec.kind == FieldKind::PackageId && !f.value.trim().is_empty())
                    .map(|f| f.value.trim().to_string());
                if let (Some(pkg_id), Some(data)) = (pkg_id, self.data.as_ref()) {
                    if let Some(item) = data
                        .catalog
                        .iter()
                        .find(|item| item.package.id.to_string() == pkg_id)
                    {
                        return item
                            .package
                            .versions
                            .iter()
                            .map(|v| v.version.to_string())
                            .collect();
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    pub fn open_command_form(&mut self, display: &str) {
        self.open_command_form_with_pkg(display, None);
    }

    pub fn open_command_form_with_pkg(&mut self, display: &str, prefill_pkg: Option<&str>) {
        let Some(found) = catalog::spec(display) else {
            return;
        };
        let mut fields: Vec<FieldState> = found
            .fields
            .iter()
            .filter(|field| !field.hidden)
            .map(FieldState::from_spec)
            .collect();
        let pkg = prefill_pkg
            .map(str::to_string)
            .or_else(|| self.last_package_id.clone())
            .or_else(|| {
                self.data
                    .as_ref()
                    .and_then(|data| data.installed.packages.keys().next().cloned())
            });
        if let Some(id) = pkg {
            for field in &mut fields {
                if field.spec.kind == FieldKind::PackageId && field.value.is_empty() {
                    field.value = id.clone();
                }
            }
        }
        self.overlay = Some(Overlay::CommandForm(Box::new(CommandForm {
            display: found.display.clone(),
            fields,
            selected: 0,
            editing: None,
            edit_buffer: String::new(),
            edit_cursor: 0,
            error: None,
            notice: Some(format!(
                "{} | {} | Opened in-place; Esc returns without losing tab cursor.",
                found.tier, found.phase
            )),
        })));
    }

    pub fn record(&mut self, text: String) {
        let text = clean(&text);
        self.logs.extend(text.lines().map(str::to_string));
        if self.logs.len() > 500 {
            self.logs.drain(..self.logs.len() - 500);
        }
    }

    pub fn text(&mut self, title: &str, content: String) {
        self.overlay = Some(Overlay::Text {
            title: title.into(),
            content: clean(&content),
            scroll: 0,
            follow_up: None,
        });
    }

    pub fn text_with_follow_up(&mut self, title: &str, content: String, follow_up: FollowUp) {
        self.overlay = Some(Overlay::Text {
            title: title.into(),
            content: clean(&content),
            scroll: 0,
            follow_up: Some(follow_up),
        });
    }

    pub fn launcher_items(&self, query: &str) -> Vec<LauncherItem> {
        let cur_pkg = self.selected_row().and_then(|row| {
            matches!(self.view, View::Installed | View::Discover | View::Updates).then_some(row)
        });
        let q = query.trim();
        let mut items: Vec<LauncherItem> = Vec::new();

        if q.is_empty() {
            for (idx, run) in self.recent_runs.iter().enumerate() {
                let action = if run.read_only {
                    LauncherAction::ExecuteCli(run.argv.clone())
                } else {
                    LauncherAction::OpenForm {
                        display: self
                            .catalog_display_for(&run.argv)
                            .unwrap_or_else(|| run.display.clone()),
                        prefill_pkg: None,
                    }
                };
                items.push(LauncherItem {
                    label: format!("numan {}", run.display),
                    sub: run.note.clone(),
                    tag: "Recent".into(),
                    tone: Tone::Info,
                    score: 1000 - idx as i64,
                    action,
                });
            }

            if let Some(row) = &cur_pkg {
                let installed = self
                    .data
                    .as_ref()
                    .and_then(|d| d.installed.packages.get(&row.id));
                if let Some(entry) = installed {
                    items.push(LauncherItem {
                        label: format!("activate {} --check", row.id),
                        sub: "Selected package: verify plugin/autoload integrity inline".into(),
                        tag: "ReadOnly".into(),
                        tone: Tone::Ok,
                        score: 880,
                        action: LauncherAction::ExecuteCli(vec![
                            "activate".into(),
                            row.id.clone(),
                            "--check".into(),
                        ]),
                    });
                    if self.available_actions(row).contains(&Action::Deactivate) {
                        items.push(LauncherItem {
                            label: format!("Deactivate {}", row.id),
                            sub: "Selected package: review deactivation".into(),
                            tag: "Reviewed".into(),
                            tone: Tone::Warn,
                            score: 875,
                            action: LauncherAction::Native(
                                Action::Deactivate,
                                row.id.clone(),
                                row.registry.clone(),
                                None,
                            ),
                        });
                    } else if operations::is_entry_activatable(entry) && row.state != "active" {
                        items.push(LauncherItem {
                            label: format!("Activate {}", row.id),
                            sub: "Selected package: review activation".into(),
                            tag: "Reviewed".into(),
                            tone: Tone::Ok,
                            score: 875,
                            action: LauncherAction::Native(
                                Action::Activate,
                                row.id.clone(),
                                row.registry.clone(),
                                None,
                            ),
                        });
                    }
                    items.push(LauncherItem {
                        label: format!("nupm diff {}", row.id),
                        sub: "Selected package: inspect nupm import drift".into(),
                        tag: "ReadOnly".into(),
                        tone: Tone::Ok,
                        score: 870,
                        action: LauncherAction::ExecuteCli(vec![
                            "nupm".into(),
                            "diff".into(),
                            row.id.clone(),
                        ]),
                    });
                } else {
                    if row.state == "compatible" || row.state == "provisional" {
                        items.push(LauncherItem {
                            label: format!("Install {}@{}", row.id, row.version),
                            sub: "Selected package: review SHA256-verified inert install".into(),
                            tag: "Reviewed".into(),
                            tone: Tone::Info,
                            score: 880,
                            action: LauncherAction::Native(
                                Action::Install,
                                row.id.clone(),
                                row.registry.clone(),
                                Some(row.version.clone()),
                            ),
                        });
                    }
                    items.push(LauncherItem {
                        label: format!("numan try {}", row.id),
                        sub: "Selected package: open guided `try` form in-place".into(),
                        tag: "Form".into(),
                        tone: Tone::Info,
                        score: 875,
                        action: LauncherAction::OpenForm {
                            display: "try".into(),
                            prefill_pkg: Some(row.id.clone()),
                        },
                    });
                }
            }

            let presets: Vec<(&str, &str, &str, Tone, LauncherAction)> = vec![
                (
                    "Local doctor scan",
                    "Read-only local check (no repairs or network probes)",
                    "ReadOnly",
                    Tone::Ok,
                    LauncherAction::Native(Action::Scan, String::new(), String::new(), None),
                ),
                (
                    "gc --dry-run",
                    "Preview reclaimable cached downloads & orphaned payloads",
                    "Preset",
                    Tone::Ok,
                    LauncherAction::ExecuteCli(vec!["gc".into(), "--dry-run".into()]),
                ),
                (
                    "use list",
                    "List managed & detected Nushell runtimes",
                    "Preset",
                    Tone::Ok,
                    LauncherAction::ExecuteCli(vec!["use".into(), "list".into()]),
                ),
                (
                    "update --self --check",
                    "Check whether a newer numan CLI binary exists",
                    "Preset",
                    Tone::Ok,
                    LauncherAction::ExecuteCli(vec![
                        "update".into(),
                        "--self".into(),
                        "--check".into(),
                    ]),
                ),
                (
                    "Sync registries (network)",
                    "Review Ed25519-verified remote registry sync",
                    "Reviewed",
                    Tone::Warn,
                    LauncherAction::OpenSync,
                ),
                (
                    "numan snapshot rollback",
                    "Open guided form in-place with snapshot picker",
                    "Form",
                    Tone::Info,
                    LauncherAction::OpenForm {
                        display: "snapshot rollback".into(),
                        prefill_pkg: None,
                    },
                ),
                (
                    "numan setup loader",
                    "Open guided form in-place (starship/zoxide/carapace)",
                    "Form",
                    Tone::Info,
                    LauncherAction::OpenForm {
                        display: "setup loader".into(),
                        prefill_pkg: None,
                    },
                ),
                (
                    "Update numan binary (CLI handoff)",
                    "Prints `numan update --self` on exit; never self-replaces live",
                    "Handoff",
                    Tone::Muted,
                    LauncherAction::Cli(vec!["update".into(), "--self".into()]),
                ),
            ];
            for (idx, (label, sub, tag, tone, action)) in presets.into_iter().enumerate() {
                items.push(LauncherItem {
                    label: label.into(),
                    sub: sub.into(),
                    tag: tag.into(),
                    tone,
                    score: 800 - idx as i64,
                    action,
                });
            }
            items.sort_by_key(|a| std::cmp::Reverse(a.score));
            return items;
        }

        // Non-empty query: universal fuzzy search across recent runs, installed
        // packages, cached catalog packages, all 42 CLI commands, views & snapshots.
        for run in &self.recent_runs {
            if let Some(sc) = fuzzy_score(&format!("{} {}", run.display, run.note), q) {
                let action = if run.read_only {
                    LauncherAction::ExecuteCli(run.argv.clone())
                } else {
                    LauncherAction::OpenForm {
                        display: self
                            .catalog_display_for(&run.argv)
                            .unwrap_or_else(|| run.display.clone()),
                        prefill_pkg: None,
                    }
                };
                items.push(LauncherItem {
                    label: format!("numan {}", run.display),
                    sub: run.note.clone(),
                    tag: "Recent".into(),
                    tone: Tone::Info,
                    score: sc + 60,
                    action,
                });
            }
        }

        if let Some(data) = &self.data {
            let nu = data.nu_valid.then_some(data.nu.as_ref()).flatten();
            for (id, entry) in &data.installed.packages {
                let state = operations::activation(entry, nu);
                let hay = format!("{id} {} {state}", entry.package_type);
                if let Some(sc) = fuzzy_score(&hay, q) {
                    let reg = operations::registry_for(entry, &data.default_registry);
                    items.push(LauncherItem {
                        label: id.clone(),
                        sub: format!(
                            "{} • {} • {state} (jump to Installed)",
                            entry.version, entry.package_type
                        ),
                        tag: "Installed".into(),
                        tone: theme::status_tone(state),
                        score: sc + 40,
                        action: LauncherAction::Navigate {
                            view: View::Installed,
                            select_id: Some(id.clone()),
                            include_incompatible: false,
                        },
                    });
                    let row_stub = Row {
                        id: id.clone(),
                        version: entry.version.clone(),
                        kind: entry.package_type.clone(),
                        state: state.into(),
                        registry: reg.clone(),
                        details: String::new(),
                    };
                    let acts = self.installed_actions(&row_stub, entry);
                    if acts.contains(&Action::Deactivate) {
                        items.push(LauncherItem {
                            label: format!("Deactivate {id}"),
                            sub: format!("Review numan deactivate {id}"),
                            tag: "Deactivate".into(),
                            tone: Tone::Warn,
                            score: sc + 15,
                            action: LauncherAction::Native(
                                Action::Deactivate,
                                id.clone(),
                                reg,
                                None,
                            ),
                        });
                    } else if acts.contains(&Action::Activate) {
                        items.push(LauncherItem {
                            label: format!("Activate {id}"),
                            sub: format!("Review numan activate {id}"),
                            tag: "Activate".into(),
                            tone: Tone::Ok,
                            score: sc + 15,
                            action: LauncherAction::Native(Action::Activate, id.clone(), reg, None),
                        });
                    }
                }
            }

            for item in &data.catalog {
                let id = item.package.id.to_string();
                let compat = operations::candidate(item, nu);
                let version = compat
                    .clone()
                    .or_else(|| item.package.versions.first().map(|v| v.version.to_string()))
                    .unwrap_or_default();
                let hay = format!(
                    "{id} {} {} {}",
                    item.package.package_type,
                    item.package.description,
                    item.package.tags.join(" ")
                );
                if let Some(sc) = fuzzy_score(&hay, q) {
                    let is_inst = data.installed.packages.contains_key(&id);
                    let incomp = compat.is_none() && data.nu_valid;
                    items.push(LauncherItem {
                        label: format!("{id} ({version})"),
                        sub: format!(
                            "{} • {} (jump to Discover)",
                            item.package.package_type, item.package.description
                        ),
                        tag: "Discover".into(),
                        tone: if is_inst {
                            Tone::Info
                        } else if incomp {
                            Tone::Err
                        } else {
                            Tone::Ok
                        },
                        score: sc + 30,
                        action: LauncherAction::Navigate {
                            view: View::Discover,
                            select_id: Some(id.clone()),
                            include_incompatible: incomp,
                        },
                    });
                    if !is_inst && compat.is_some() {
                        items.push(LauncherItem {
                            label: format!("Install {id}@{version}"),
                            sub: "Review SHA256-verified artifact & inert install".into(),
                            tag: "Install".into(),
                            tone: Tone::Info,
                            score: sc + 18,
                            action: LauncherAction::Native(
                                Action::Install,
                                id,
                                item.registry.clone(),
                                Some(version),
                            ),
                        });
                    }
                }
            }

            for snap in &data.snapshots {
                let hay = format!("{} snapshot {:?}", snap.id, snap.trigger);
                if let Some(sc) = fuzzy_score(&hay, q) {
                    items.push(LauncherItem {
                        label: snap.id.clone(),
                        sub: format!(
                            "{:?} ({}) — inspect in System",
                            snap.trigger, snap.created_at
                        ),
                        tag: "Snapshot".into(),
                        tone: Tone::Muted,
                        score: sc + 5,
                        action: LauncherAction::Navigate {
                            view: View::System,
                            select_id: Some("Snapshots".into()),
                            include_incompatible: false,
                        },
                    });
                }
            }
        }

        let cur_pkg_id = cur_pkg.map(|r| r.id);
        for spec in catalog::catalog() {
            let hay = format!("{} {} {}", spec.display, spec.summary, spec.tier);
            if let Some(sc) = fuzzy_score(&hay, q) {
                items.push(LauncherItem {
                    label: format!("numan {}", spec.display),
                    sub: format!("Open form in-place • {}", spec.summary),
                    tag: "Form".into(),
                    tone: theme::tier_tone(spec.tier),
                    score: sc + 25,
                    action: LauncherAction::OpenForm {
                        display: spec.display.clone(),
                        prefill_pkg: cur_pkg_id.clone(),
                    },
                });
            }
        }

        for (label, sub, view) in [
            ("Tab: Installed", "View installed packages", View::Installed),
            (
                "Tab: Discover",
                "Browse verified cached registry",
                View::Discover,
            ),
            ("Tab: Updates", "Check cached updates", View::Updates),
            (
                "Tab: Commands",
                "Browse all 42 CLI command forms",
                View::Commands,
            ),
            (
                "Tab: System",
                "Identity, Doctor, Snapshots, Registries, Transcript",
                View::System,
            ),
        ] {
            if let Some(sc) = fuzzy_score(&format!("{label} {sub}"), q) {
                items.push(LauncherItem {
                    label: label.into(),
                    sub: sub.into(),
                    tag: "Tab".into(),
                    tone: Tone::Muted,
                    score: sc,
                    action: LauncherAction::Navigate {
                        view,
                        select_id: None,
                        include_incompatible: false,
                    },
                });
            }
        }

        if let Some(sc) = fuzzy_score("local doctor scan health check", q) {
            items.push(LauncherItem {
                label: "Local doctor scan".into(),
                sub: "Read-only local scan (no repairs, no network)".into(),
                tag: "ReadOnly".into(),
                tone: Tone::Ok,
                score: sc + 35,
                action: LauncherAction::Native(Action::Scan, String::new(), String::new(), None),
            });
        }

        if let Some(sc) = fuzzy_score("sync registries refresh remote network", q) {
            items.push(LauncherItem {
                label: "Sync registries (Shift+R)".into(),
                sub: "Review remote Ed25519-verified registry sync".into(),
                tag: "Reviewed".into(),
                tone: Tone::Warn,
                score: sc + 20,
                action: LauncherAction::OpenSync,
            });
        }

        if let Some(sc) = fuzzy_score("update --self handoff binary upgrade", q) {
            items.push(LauncherItem {
                label: "Update numan binary (CLI handoff)".into(),
                sub: "Prints `numan update --self` on exit".into(),
                tag: "Handoff".into(),
                tone: Tone::Muted,
                score: sc + 10,
                action: LauncherAction::Cli(vec!["update".into(), "--self".into()]),
            });
        }

        items.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.label.cmp(&b.label)));
        items.truncate(25);
        items
    }

    fn catalog_display_for(&self, argv: &[String]) -> Option<String> {
        catalog::catalog()
            .iter()
            .filter(|spec| argv.starts_with(&spec.path))
            .max_by_key(|spec| spec.path.len())
            .map(|spec| spec.display.clone())
    }

    fn installed_actions(
        &self,
        row: &Row,
        entry: &crate::state::lockfile::LockfileEntry,
    ) -> Vec<Action> {
        let has_act = operations::has_any_activation(entry);
        let mut actions = Vec::new();
        if row.state != "active" && operations::is_entry_activatable(entry) {
            actions.push(Action::Activate);
        }
        let deactivatable = if entry.package_type == "plugin" {
            row.state == "active"
        } else {
            matches!(
                entry.package_type.as_str(),
                "module" | "script" | "completion"
            ) && has_act
        };
        if deactivatable {
            actions.push(Action::Deactivate);
        }
        if !has_act && entry.origin.as_deref() != Some(crate::state::lockfile::BUNDLED_NU_ORIGIN) {
            actions.push(Action::Remove);
        }
        actions
    }

    pub fn available_actions(&self, row: &Row) -> Vec<Action> {
        if self.busy {
            return Vec::new();
        }
        let installed = self
            .data
            .as_ref()
            .and_then(|d| d.installed.packages.get(&row.id));
        match self.view {
            View::Discover => {
                if installed.is_none() && (row.state == "compatible" || row.state == "provisional")
                {
                    vec![Action::Install]
                } else {
                    Vec::new()
                }
            }
            View::Updates => {
                let Some(entry) = installed else {
                    return Vec::new();
                };
                let mut actions = Vec::new();
                let active = operations::has_any_activation(entry);
                let opt_in =
                    std::env::var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION").as_deref() == Ok("1");
                if (!active || (entry.package_type == "plugin" && opt_in))
                    && entry.origin.as_deref() != Some(crate::state::lockfile::BUNDLED_NU_ORIGIN)
                {
                    actions.push(Action::Update);
                }
                actions.extend(self.installed_actions(row, entry));
                actions
            }
            View::Installed => match installed {
                Some(entry) => self.installed_actions(row, entry),
                None => Vec::new(),
            },
            View::System => {
                if row.id == "Doctor" {
                    vec![Action::Scan]
                } else {
                    Vec::new()
                }
            }
            View::Commands => Vec::new(),
        }
    }

    fn request(&self, action: Action) -> Option<Command> {
        if self.busy {
            return None;
        }
        match action {
            Action::Scan => return Some(Command::Scan),
            Action::BatchUpdate => {
                return Some(Command::Prepare(
                    Action::BatchUpdate,
                    String::new(),
                    String::new(),
                    None,
                ))
            }
            _ => {}
        }
        let row = self.selected_row()?;
        if !self.available_actions(&row).contains(&action) {
            return None;
        }
        let pinned = (action == Action::Install).then_some(row.version);
        Some(Command::Prepare(action, row.id, row.registry, pinned))
    }

    pub fn package_actions(&self) -> Vec<(String, Action, String)> {
        let row = self.selected_row();
        let allowed = row
            .as_ref()
            .map(|row| self.available_actions(row))
            .unwrap_or_default();
        let mut actions = vec![
            Action::Install,
            Action::Activate,
            Action::Deactivate,
            Action::Update,
        ];
        if self.view == View::Updates {
            actions.push(Action::BatchUpdate);
        }
        actions.push(Action::Remove);
        actions
            .into_iter()
            .map(|action| {
                let reason = if self.busy {
                    "Wait for the current operation to finish."
                } else if action != Action::BatchUpdate && row.is_none() {
                    "Select a package first."
                } else if allowed.contains(&action)
                    || (action == Action::BatchUpdate && self.view == View::Updates)
                {
                    ""
                } else {
                    match action {
                        Action::Install => {
                            "Already installed or incompatible. Use Discover for compatible packages."
                        }
                        Action::Activate => {
                            "Already active, or activation is unsupported for this package type."
                        }
                        Action::Deactivate => {
                            "Package is not active for the current Nu context."
                        }
                        Action::Update => {
                            "Select Updates and ensure the package is inactive (or plugin mutation opt-in is enabled)."
                        }
                        Action::BatchUpdate => {
                            "Select the Updates section to run batch updates."
                        }
                        Action::Remove => {
                            "Active packages must be deactivated before removal ([d]). Bundled plugins are managed with Nushell."
                        }
                        Action::Scan => "Use System > Doctor or Ctrl+P for doctor scan.",
                    }
                };
                let label = match action {
                    Action::BatchUpdate => "Review Batch Update (all eligible)".into(),
                    other => format!("Review {other:?}"),
                };
                (label, action, reason.to_string())
            })
            .collect()
    }

    pub fn generic_effects(&self, display: &str, argv: &[String]) -> Vec<String> {
        let mut effects = vec![
            format!("Root: {}", self.root.display()),
            format!("Command: {}", self.handoff(argv, Shell::Nushell)),
        ];
        if let Some(found) = catalog::spec(display) {
            effects.push(format!("Tier: {} | {}", found.tier, found.phase));
            if found.tier.contains("Destructive") {
                effects.push(
                    "Destructive: this permanently deletes state or payloads. Snapshots do not guarantee restoration."
                        .into(),
                );
            }
            if found.tier.contains("External") {
                effects.push(
                    "External: this writes outside the Numan root (shell config, PATH, managed Nu, tool binaries)."
                        .into(),
                );
            }
            if found.tier.contains("Network") {
                effects.push("Network: this contacts remote registries or release hosts.".into());
            }
            if found.tier.contains("Shell") {
                effects.push(
                    "Shell: the Nushell statement this emits must still be run in your current shell; the TUI cannot apply it for you."
                        .into(),
                );
            }
        }
        if argv.iter().any(|arg| arg == "--self") && !argv.iter().any(|arg| arg == "--check") {
            effects.push(
                "Self-update replaces the running numan binary and is not executed in the TUI; run `numan update --self` in a terminal."
                    .into(),
            );
        }
        effects.push(
            "This review is the confirmation. Where the CLI requires --yes, it is added at execution time only and never stored."
                .into(),
        );
        effects.push(
            "Existing lock, snapshot, journal, and trust rules apply. Cancellation is unavailable after submission."
                .into(),
        );
        effects
    }

    pub fn handoff(&self, args: &[String], shell: Shell) -> String {
        let mut words = vec![
            "numan".to_string(),
            "--root".into(),
            shell.quote(&self.root.to_string_lossy()),
        ];
        words.extend(args.iter().map(|arg| shell.quote(arg)));
        words.join(" ")
    }

    fn cycle_discover_version(&mut self) {
        if self.view != View::Discover {
            return;
        }
        if let (Some(row), Some(data)) = (self.selected_row(), self.data.as_ref()) {
            if let Some(item) = data
                .catalog
                .iter()
                .find(|item| item.registry == row.registry && item.package.id.to_string() == row.id)
            {
                let versions = &item.package.versions;
                if !versions.is_empty() {
                    let index = versions
                        .iter()
                        .position(|v| v.version.to_string() == row.version)
                        .unwrap_or(0);
                    self.pinned = Some((
                        row.id,
                        versions[(index + 1) % versions.len()].version.to_string(),
                    ));
                }
            }
        }
    }

    fn execute_form_row(&mut self, mut form: Box<CommandForm>) -> Option<Command> {
        if form.selected == form.fields.len() + 1 {
            match catalog::execution_mode(&form.display, &form.fields) {
                catalog::ExecutionMode::Blocked(reason) => {
                    form.error = Some(reason.to_string());
                }
                mode => {
                    if self.busy {
                        form.error = Some("Wait for the current operation to finish.".into());
                    } else if let Some(missing) = form.fields.iter().find(|field| {
                        field.spec.required
                            && field.spec.kind != FieldKind::Bool
                            && field.value.trim().is_empty()
                    }) {
                        form.error =
                            Some(format!("Required field '{}' is empty.", missing.spec.id));
                    } else {
                        let Some(found) = catalog::spec(&form.display) else {
                            form.error = Some(format!("No catalog entry for {}.", form.display));
                            self.overlay = Some(Overlay::CommandForm(form));
                            return None;
                        };
                        // `activate --list/--check` conflicts with a positional;
                        // a context-prefilled package must not leak into those read-only forms.
                        let mut effective = form.fields.clone();
                        if form.display == "activate"
                            && effective.iter().any(|field| {
                                (field.spec.id == "list" || field.spec.id == "check")
                                    && field.enabled
                            })
                        {
                            for field in &mut effective {
                                if field.spec.id == "packages" {
                                    field.value.clear();
                                }
                            }
                        }
                        let argv = catalog::build_argv(found, &effective);
                        if mode == catalog::ExecutionMode::ReadOnly {
                            return Some(Command::ExecuteCli(argv));
                        }
                        let display = form.display.clone();
                        let effects = self.generic_effects(&display, &argv);
                        self.overlay = Some(Overlay::GenericReview {
                            argv,
                            display,
                            effects,
                            confirm: false,
                            scroll: 0,
                        });
                        return None;
                    }
                }
            }
        } else if form.selected == form.fields.len() {
            form.edit_buffer = self.command_preview(&form);
            form.edit_cursor = form.edit_buffer.chars().count();
            form.editing = Some(EditTarget::Preview);
        } else if let Some(field_kind) = form.fields.get(form.selected).map(|f| f.spec.kind) {
            match field_kind {
                FieldKind::Bool => {
                    if let Some(field) = form.fields.get_mut(form.selected) {
                        field.enabled = !field.enabled;
                    }
                }
                FieldKind::Enum => {
                    let opts = form.fields[form.selected].spec.possible.clone();
                    if !opts.is_empty() {
                        let field = &mut form.fields[form.selected];
                        let current = opts
                            .iter()
                            .position(|v| v == &field.value)
                            .unwrap_or(usize::MAX);
                        let next = if current == usize::MAX {
                            0
                        } else {
                            (current + 1) % opts.len()
                        };
                        field.value = opts[next].clone();
                    } else {
                        form.edit_buffer = form.fields[form.selected].value.clone();
                        form.edit_cursor = form.edit_buffer.chars().count();
                        form.editing = Some(EditTarget::Field(form.selected));
                    }
                }
                _ => {
                    form.edit_buffer = form.fields[form.selected].value.clone();
                    form.edit_cursor = form.edit_buffer.chars().count();
                    form.editing = Some(EditTarget::Field(form.selected));
                }
            }
        }
        self.overlay = Some(Overlay::CommandForm(form));
        None
    }

    fn execute_launcher_item(&mut self, item: LauncherItem) -> Option<Command> {
        match item.action {
            LauncherAction::Navigate {
                view,
                select_id,
                include_incompatible,
            } => {
                self.navigate(view);
                if include_incompatible && view == View::Discover {
                    self.show_all = true;
                }
                if let Some(target_id) = select_id {
                    if let Some(idx) = self.rows().iter().position(|r| r.id == target_id) {
                        self.selected = idx;
                        self.focus = Focus::Details;
                    }
                }
                None
            }
            LauncherAction::Native(action, id, registry, version) => {
                if action == Action::Scan {
                    return (!self.busy).then_some(Command::Scan);
                }
                if self.busy {
                    return None;
                }
                Some(Command::Prepare(action, id, registry, version))
            }
            LauncherAction::OpenForm {
                display,
                prefill_pkg,
            } => {
                self.open_command_form_with_pkg(&display, prefill_pkg.as_deref());
                None
            }
            LauncherAction::ExecuteCli(argv) => {
                if self.busy {
                    None
                } else {
                    Some(Command::ExecuteCli(argv))
                }
            }
            LauncherAction::OpenSync => {
                if !self.busy {
                    self.overlay = Some(Overlay::Sync);
                }
                None
            }
            LauncherAction::Cli(args) => {
                self.overlay = Some(Overlay::Handoff {
                    args,
                    shell: Shell::Nushell,
                });
                None
            }
        }
    }

    fn handle_remove_request(&mut self) -> Option<Command> {
        if self.busy {
            return None;
        }
        if let Some(cmd) = self.request(Action::Remove) {
            return Some(cmd);
        }
        let row = self.selected_row()?;
        if self.available_actions(&row).contains(&Action::Deactivate) {
            let id = row.id.clone();
            let reg = row.registry.clone();
            let state = row.state.clone();
            self.text_with_follow_up(
                "Deactivate required",
                format!(
                    "{id} is currently active ({state}). Deactivate it before removal.\n\nPress [d] to open a deactivation review."
                ),
                FollowUp {
                    key: 'd',
                    label: "Review Deactivate [d]".into(),
                    command: Box::new(Command::Prepare(Action::Deactivate, id, reg, None)),
                },
            );
        } else {
            self.text(
                "Remove unavailable",
                format!(
                    "{} cannot be removed safely in its current state ({}). Inspect state or deactivate under its active Nu.",
                    row.id, row.state
                ),
            );
        }
        None
    }

    fn handle_update_request(&mut self) -> Option<Command> {
        if self.busy {
            return None;
        }
        if let Some(cmd) = self.request(Action::Update) {
            return Some(cmd);
        }
        let row = self.selected_row()?;
        if self.available_actions(&row).contains(&Action::Deactivate) {
            let id = row.id.clone();
            let reg = row.registry.clone();
            self.text_with_follow_up(
                "Active package update gated",
                format!(
                    "{id} is currently active. Deactivate first, or set NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1 for active plugins.\n\nPress [d] to review deactivation."
                ),
                FollowUp {
                    key: 'd',
                    label: "Review Deactivate [d]".into(),
                    command: Box::new(Command::Prepare(Action::Deactivate, id, reg, None)),
                },
            );
        }
        None
    }

    /// One focus owns each event. Returning a command never executes it here.
    pub fn key(&mut self, key: KeyEvent) -> Option<Command> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.busy {
                self.status =
                    "Operation running; cancellation unavailable. Wait for completion.".into();
            } else {
                self.exit = true;
            }
            return None;
        }
        if self.small {
            if !self.busy && key.code == KeyCode::Char('q') {
                self.exit = true;
            }
            return None;
        }
        if let Some(overlay) = self.overlay.take() {
            match overlay {
                Overlay::Actions { mut selected } => {
                    let entries = self.package_actions();
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Down => {
                            selected = (selected + 1).min(entries.len().saturating_sub(1))
                        }
                        KeyCode::Up => selected = selected.saturating_sub(1),
                        KeyCode::Enter => {
                            if let Some((_, action, reason)) = entries.get(selected) {
                                if reason.is_empty() {
                                    return self.request(*action);
                                }
                                self.text("Action unavailable", reason.clone());
                            }
                            return None;
                        }
                        _ => {}
                    }
                    self.overlay = Some(Overlay::Actions { selected });
                }

                Overlay::Sync => match key.code {
                    KeyCode::Enter if !self.busy => return Some(Command::Sync),
                    KeyCode::Esc => return None,
                    _ => self.overlay = Some(Overlay::Sync),
                },

                Overlay::Filter(mut query) => {
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Enter => {
                            self.filter = query;
                            self.selected = 0;
                            self.list_offset = 0;
                            self.command_selected = 0;
                            return None;
                        }
                        KeyCode::Backspace => {
                            query.pop();
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            query.push(c)
                        }
                        _ => {}
                    }
                    self.overlay = Some(Overlay::Filter(query));
                }

                Overlay::Launcher {
                    mut query,
                    mut selected,
                } => {
                    let items = self.launcher_items(&query);
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Down => {
                            selected = (selected + 1).min(items.len().saturating_sub(1))
                        }
                        KeyCode::Up => selected = selected.saturating_sub(1),
                        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            selected = (selected + 1).min(items.len().saturating_sub(1))
                        }
                        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            selected = selected.saturating_sub(1)
                        }
                        KeyCode::Backspace => {
                            query.pop();
                            selected = 0;
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            query.push(c);
                            selected = 0;
                        }
                        KeyCode::Enter => {
                            if let Some(item) = items.get(selected).cloned() {
                                return self.execute_launcher_item(item);
                            }
                            return None;
                        }
                        _ => {}
                    }
                    self.overlay = Some(Overlay::Launcher { query, selected });
                }

                Overlay::Review {
                    review,
                    mut confirm,
                    mut scroll,
                } => {
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                            confirm = !confirm
                        }
                        KeyCode::Down | KeyCode::PageDown => scroll = scroll.saturating_add(1),
                        KeyCode::Up | KeyCode::PageUp => scroll = scroll.saturating_sub(1),
                        KeyCode::Enter if confirm && !self.busy => {
                            return Some(Command::Execute(review))
                        }
                        KeyCode::Enter if confirm => {
                            self.status = "Operation running; review discarded. Wait for completion, then review again.".into();
                            return None;
                        }
                        KeyCode::Enter if !confirm => return None,
                        _ => {}
                    }
                    self.overlay = Some(Overlay::Review {
                        review,
                        confirm,
                        scroll,
                    });
                }

                Overlay::Text {
                    title,
                    content,
                    mut scroll,
                    follow_up,
                } => {
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            if let Some(ref fu) = follow_up {
                                if c == fu.key && !self.busy {
                                    return Some((*fu.command).clone());
                                }
                            }
                        }
                        KeyCode::Down => {
                            scroll = scroll
                                .saturating_add(1)
                                .min(content.lines().count().saturating_sub(1) as u16)
                        }
                        KeyCode::PageDown => {
                            scroll = scroll
                                .saturating_add(10)
                                .min(content.lines().count().saturating_sub(1) as u16)
                        }
                        KeyCode::Up => scroll = scroll.saturating_sub(1),
                        KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                        _ => {}
                    }
                    self.overlay = Some(Overlay::Text {
                        title,
                        content,
                        scroll,
                        follow_up,
                    });
                }

                Overlay::CommandForm(mut form) => {
                    if let Some(target) = form.editing {
                        let len = form.edit_buffer.chars().count();
                        form.edit_cursor = form.edit_cursor.min(len);
                        match key.code {
                            KeyCode::Esc => {
                                form.editing = None;
                                form.error = None;
                            }
                            KeyCode::Left => form.edit_cursor = form.edit_cursor.saturating_sub(1),
                            KeyCode::Right => form.edit_cursor = (form.edit_cursor + 1).min(len),
                            KeyCode::Home => form.edit_cursor = 0,
                            KeyCode::End => form.edit_cursor = len,
                            KeyCode::Backspace => {
                                remove_before_cursor(&mut form.edit_buffer, &mut form.edit_cursor);
                            }
                            KeyCode::Delete => {
                                remove_at_cursor(&mut form.edit_buffer, form.edit_cursor);
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                insert_char_at(&mut form.edit_buffer, &mut form.edit_cursor, c);
                            }
                            KeyCode::Enter => match target {
                                EditTarget::Field(index) => {
                                    if let Some(field) = form.fields.get_mut(index) {
                                        field.value = form.edit_buffer.clone();
                                    }
                                    form.editing = None;
                                    form.error = None;
                                }
                                EditTarget::Preview => {
                                    match catalog::validate_preview(
                                        &self.root.clone(),
                                        &form.edit_buffer.clone(),
                                    ) {
                                        Ok((found, rest)) => {
                                            let display = found.display.clone();
                                            let fields = catalog::fields_from_argv(&found, &rest)
                                                .into_iter()
                                                .filter(|field| !field.spec.hidden)
                                                .collect();
                                            form = Box::new(CommandForm {
                                                display,
                                                fields,
                                                selected: 0,
                                                editing: None,
                                                edit_buffer: String::new(),
                                                edit_cursor: 0,
                                                error: None,
                                                notice: Some(
                                                    "Preview valid. Rebuilt form from the edited command; review invalidated."
                                                        .into(),
                                                ),
                                            });
                                        }
                                        Err(error) => {
                                            form.error = Some(error);
                                        }
                                    }
                                }
                            },
                            _ => {}
                        }
                        self.overlay = Some(Overlay::CommandForm(form));
                        return None;
                    }
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Down => {
                            let max = form.fields.len() + 1;
                            form.selected = (form.selected + 1).min(max);
                        }
                        KeyCode::Up => form.selected = form.selected.saturating_sub(1),
                        KeyCode::Left | KeyCode::Right => {
                            if let Some(field) = form.fields.get(form.selected) {
                                if field.spec.kind == FieldKind::Bool {
                                    if let Some(field) = form.fields.get_mut(form.selected) {
                                        field.enabled = !field.enabled;
                                    }
                                } else {
                                    let opts = self.picker_options(&form, form.selected);
                                    if !opts.is_empty() {
                                        let field = &mut form.fields[form.selected];
                                        let current = opts
                                            .iter()
                                            .position(|value| value == &field.value)
                                            .unwrap_or(usize::MAX);
                                        let next = if key.code == KeyCode::Right {
                                            if current == usize::MAX {
                                                0
                                            } else {
                                                (current + 1) % opts.len()
                                            }
                                        } else if current == usize::MAX {
                                            opts.len() - 1
                                        } else {
                                            (current + opts.len() - 1) % opts.len()
                                        };
                                        field.value = opts[next].clone();
                                    }
                                }
                            }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            return self.execute_form_row(form);
                        }
                        _ => {}
                    }
                    self.overlay = Some(Overlay::CommandForm(form));
                }

                Overlay::GenericReview {
                    argv,
                    display,
                    effects,
                    mut confirm,
                    mut scroll,
                } => {
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                            confirm = !confirm
                        }
                        KeyCode::Down | KeyCode::PageDown => scroll = scroll.saturating_add(1),
                        KeyCode::Up | KeyCode::PageUp => scroll = scroll.saturating_sub(1),
                        KeyCode::Enter if confirm && !self.busy => {
                            return Some(Command::ExecuteCli(argv));
                        }
                        KeyCode::Enter if !confirm => return None,
                        _ => {}
                    }
                    self.overlay = Some(Overlay::GenericReview {
                        argv,
                        display,
                        effects,
                        confirm,
                        scroll,
                    });
                }

                Overlay::Handoff { args, mut shell } => {
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Tab => shell = shell.next(),
                        KeyCode::Enter if !self.busy => {
                            self.exit_command = Some(self.handoff(&args, shell));
                            self.exit = true;
                            return None;
                        }
                        _ => {}
                    }
                    self.overlay = Some(Overlay::Handoff { args, shell });
                }
            }
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('p') && !self.busy {
                self.overlay = Some(Overlay::Launcher {
                    query: String::new(),
                    selected: 0,
                });
            }
            return None;
        }
        match key.code {
            KeyCode::Char('q') => {
                if self.busy {
                    self.status = "Wait for the current operation before quitting.".into();
                } else {
                    self.exit = true;
                }
            }
            KeyCode::Char(c @ '1'..='5') => {
                let idx = (c as usize) - ('1' as usize);
                self.navigate(View::ALL[idx]);
                self.focus = Focus::List;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::List => Focus::Details,
                    Focus::Details => Focus::List,
                };
            }
            KeyCode::Char('?') => self.text(
                "Help",
                "1-5: switch view (Installed / Discover / Updates / Commands / System)\nTab / Left / Right: focus panes (List / Details)\nArrows / j / k: select row or scroll details    PageUp / PageDown: jump scroll\nEnter: actions menu (or open Command Form in Commands)\n/: filter active tab    Ctrl+P: fuzzy palette across packages, commands & presets\ni / a / d / u / U / x: review install / activate / deactivate / update / update-all / remove\nt / c / v: cycle type filter / compatibility / Discover version\ns: run local doctor scan in System    r: reload local files    Shift+R: review registry sync\nl: operation transcript    n: Nu/root context    Esc: back/clear    q: quit\n\nInstall and activation require separate reviews.\nCancellation is unavailable after submission."
                    .into(),
            ),
            KeyCode::Char('l') => self.text("Operation transcript", self.logs.join("\n")),
            KeyCode::Char('n') => {
                let context = self
                    .data
                    .as_ref()
                    .and_then(|data| data.nu.as_ref())
                    .map(|nu| {
                        format!(
                            "Nu: {}\nExecutable: {}\nSHA256: {}\nPlugin registry: {}\nAutoload: {:?}",
                            nu.nu_version,
                            nu.nu_executable,
                            nu.nu_executable_hash,
                            nu.plugin_registry_path,
                            nu.vendor_autoload_dir
                        )
                    })
                    .unwrap_or_else(|| "Nu context missing; initialize outside TUI.".into());
                self.text("Context", format!("Root: {}\n{context}", self.root.display()));
            }
            KeyCode::Char('r') if !self.busy => return Some(Command::Reload),
            KeyCode::Char('/') => self.overlay = Some(Overlay::Filter(self.filter.clone())),
            KeyCode::Char('i') if self.view == View::Discover => {
                return self.request(Action::Install);
            }
            KeyCode::Char('a') if matches!(self.view, View::Installed | View::Updates) => {
                return self.request(Action::Activate);
            }
            KeyCode::Char('d') if matches!(self.view, View::Installed | View::Updates) => {
                return self.request(Action::Deactivate);
            }
            KeyCode::Char('x') if matches!(self.view, View::Installed | View::Updates) => {
                return self.handle_remove_request();
            }
            KeyCode::Char('u') if self.view == View::Updates => {
                return self.handle_update_request();
            }
            KeyCode::Char('U') if self.view == View::Updates => {
                return self.request(Action::BatchUpdate);
            }
            KeyCode::Char('s') if self.view == View::System => {
                return self.request(Action::Scan);
            }
            KeyCode::Char('t') if self.view == View::Discover => {
                self.type_filter = self.type_filter.next();
                self.selected = 0;
                self.list_offset = 0;
                self.detail_scroll = 0;
            }
            KeyCode::Char('c') if self.view == View::Discover => {
                self.show_all = !self.show_all;
                self.selected = 0;
                self.list_offset = 0;
                self.detail_scroll = 0;
            }
            KeyCode::Char('v') if self.view == View::Discover => {
                self.cycle_discover_version();
            }
            KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::PageDown
            | KeyCode::PageUp => {
                let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown);
                let step = if matches!(key.code, KeyCode::PageDown | KeyCode::PageUp) {
                    10usize
                } else {
                    1usize
                };
                match self.focus {
                    Focus::List => {
                        if self.view == View::Commands {
                            let count = self.command_specs().len();
                            self.command_selected = if down {
                                (self.command_selected + step).min(count.saturating_sub(1))
                            } else {
                                self.command_selected.saturating_sub(step)
                            };
                        } else {
                            let count = self.rows().len();
                            self.selected = if down {
                                (self.selected + step).min(count.saturating_sub(1))
                            } else {
                                self.selected.saturating_sub(step)
                            };
                        }
                        self.detail_scroll = 0;
                    }
                    Focus::Details => {
                        let s = step as u16;
                        self.detail_scroll = if down {
                            self.detail_scroll.saturating_add(s)
                        } else {
                            self.detail_scroll.saturating_sub(s)
                        };
                    }
                }
            }
            KeyCode::Left => self.focus = Focus::List,
            KeyCode::Right => self.focus = Focus::Details,
            KeyCode::Char('R') if !self.busy => self.overlay = Some(Overlay::Sync),
            KeyCode::Enter => {
                if self.view == View::Commands && self.focus == Focus::List {
                    if let Some(found) = self.selected_command() {
                        let display = found.display.clone();
                        self.open_command_form(&display);
                    }
                } else if self.view == View::System {
                    if let Some(row) = self.selected_row() {
                        if row.id == "Doctor" {
                            return self.request(Action::Scan);
                        } else if row.id == "Registries" && !self.busy {
                            self.overlay = Some(Overlay::Sync);
                        } else {
                            self.focus = Focus::Details;
                        }
                    }
                } else {
                    self.overlay = Some(Overlay::Actions { selected: 0 });
                }
            }
            KeyCode::Esc => {
                if self.focus == Focus::Details {
                    self.focus = Focus::List;
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                    self.selected = 0;
                    self.list_offset = 0;
                    self.command_selected = 0;
                }
            }
            _ => {}
        }
        None
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Option<Command> {
        if self.small {
            return None;
        }
        let col = mouse.column;
        let row = mouse.row;
        let regions = self.regions.clone();

        match mouse.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = mouse.kind == MouseEventKind::ScrollDown;
                if let Some(overlay) = self.overlay.take() {
                    match overlay {
                        Overlay::Actions { mut selected } => {
                            let max = self.package_actions().len().saturating_sub(1);
                            selected = if down {
                                (selected + 1).min(max)
                            } else {
                                selected.saturating_sub(1)
                            };
                            self.overlay = Some(Overlay::Actions { selected });
                        }
                        Overlay::Launcher {
                            query,
                            mut selected,
                        } => {
                            let max = self.launcher_items(&query).len().saturating_sub(1);
                            selected = if down {
                                (selected + 1).min(max)
                            } else {
                                selected.saturating_sub(1)
                            };
                            self.overlay = Some(Overlay::Launcher { query, selected });
                        }
                        Overlay::Review {
                            review,
                            confirm,
                            mut scroll,
                        } => {
                            scroll = if down {
                                scroll.saturating_add(1)
                            } else {
                                scroll.saturating_sub(1)
                            };
                            self.overlay = Some(Overlay::Review {
                                review,
                                confirm,
                                scroll,
                            });
                        }
                        Overlay::GenericReview {
                            argv,
                            display,
                            effects,
                            confirm,
                            mut scroll,
                        } => {
                            scroll = if down {
                                scroll.saturating_add(1)
                            } else {
                                scroll.saturating_sub(1)
                            };
                            self.overlay = Some(Overlay::GenericReview {
                                argv,
                                display,
                                effects,
                                confirm,
                                scroll,
                            });
                        }
                        Overlay::Text {
                            title,
                            content,
                            mut scroll,
                            follow_up,
                        } => {
                            let max = content.lines().count().saturating_sub(1) as u16;
                            scroll = if down {
                                scroll.saturating_add(1).min(max)
                            } else {
                                scroll.saturating_sub(1)
                            };
                            self.overlay = Some(Overlay::Text {
                                title,
                                content,
                                scroll,
                                follow_up,
                            });
                        }
                        Overlay::CommandForm(mut form) => {
                            if form.editing.is_none() {
                                let max = form.fields.len() + 1;
                                form.selected = if down {
                                    (form.selected + 1).min(max)
                                } else {
                                    form.selected.saturating_sub(1)
                                };
                            }
                            self.overlay = Some(Overlay::CommandForm(form));
                        }
                        other => self.overlay = Some(other),
                    }
                    return None;
                }
                if regions.details_pane.is_some_and(|r| contains(r, col, row)) {
                    self.detail_scroll = if down {
                        self.detail_scroll.saturating_add(1)
                    } else {
                        self.detail_scroll.saturating_sub(1)
                    };
                } else if regions.list_pane.is_some_and(|r| contains(r, col, row)) {
                    if self.view == View::Commands {
                        let max = self.command_specs().len().saturating_sub(1);
                        self.command_selected = if down {
                            (self.command_selected + 1).min(max)
                        } else {
                            self.command_selected.saturating_sub(1)
                        };
                    } else {
                        let max = self.rows().len().saturating_sub(1);
                        self.selected = if down {
                            (self.selected + 1).min(max)
                        } else {
                            self.selected.saturating_sub(1)
                        };
                    }
                    self.detail_scroll = 0;
                }
                None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(overlay) = self.overlay.take() {
                    if regions
                        .modal_close_rect
                        .is_some_and(|r| contains(r, col, row))
                    {
                        return None;
                    }
                    for (rect, action) in &regions.modal_buttons {
                        if contains(*rect, col, row) {
                            match action {
                                ModalClick::Cancel | ModalClick::Close => return None,
                                ModalClick::Confirm => match overlay {
                                    Overlay::Sync if !self.busy => return Some(Command::Sync),
                                    Overlay::Review { review, .. } if !self.busy => {
                                        return Some(Command::Execute(review))
                                    }
                                    Overlay::GenericReview { argv, .. } if !self.busy => {
                                        return Some(Command::ExecuteCli(argv))
                                    }
                                    Overlay::Handoff { args, shell } if !self.busy => {
                                        self.exit_command = Some(self.handoff(&args, shell));
                                        self.exit = true;
                                        return None;
                                    }
                                    other => {
                                        self.overlay = Some(other);
                                        return None;
                                    }
                                },
                                ModalClick::FollowUp(cmd) if !self.busy => {
                                    return Some(cmd.clone());
                                }
                                ModalClick::CycleShell => {
                                    if let Overlay::Handoff { args, shell } = overlay {
                                        self.overlay = Some(Overlay::Handoff {
                                            args,
                                            shell: shell.next(),
                                        });
                                    } else {
                                        self.overlay = Some(overlay);
                                    }
                                    return None;
                                }
                                ModalClick::SwitchConfirm(choice) => {
                                    match overlay {
                                        Overlay::Review { review, scroll, .. } => {
                                            self.overlay = Some(Overlay::Review {
                                                review,
                                                confirm: *choice,
                                                scroll,
                                            });
                                        }
                                        Overlay::GenericReview {
                                            argv,
                                            display,
                                            effects,
                                            scroll,
                                            ..
                                        } => {
                                            self.overlay = Some(Overlay::GenericReview {
                                                argv,
                                                display,
                                                effects,
                                                confirm: *choice,
                                                scroll,
                                            });
                                        }
                                        other => self.overlay = Some(other),
                                    }
                                    return None;
                                }
                                ModalClick::FieldRow(idx) => {
                                    if let Overlay::CommandForm(mut form) = overlay {
                                        form.editing = None;
                                        form.selected = *idx;
                                        return self.execute_form_row(form);
                                    }
                                    self.overlay = Some(overlay);
                                    return None;
                                }
                                _ => {
                                    self.overlay = Some(overlay);
                                    return None;
                                }
                            }
                        }
                    }
                    if let Some((list_rect, offset)) = regions.modal_list_area {
                        if contains(list_rect, col, row) {
                            let idx = offset + usize::from(row - list_rect.y);
                            match overlay {
                                Overlay::Actions { .. } => {
                                    let entries = self.package_actions();
                                    if let Some((_, action, reason)) = entries.get(idx) {
                                        if reason.is_empty() {
                                            return self.request(*action);
                                        }
                                        self.text("Action unavailable", reason.clone());
                                        return None;
                                    }
                                    self.overlay = Some(Overlay::Actions { selected: 0 });
                                    return None;
                                }
                                Overlay::Launcher { query, .. } => {
                                    let items = self.launcher_items(&query);
                                    if let Some(item) = items.get(idx).cloned() {
                                        return self.execute_launcher_item(item);
                                    }
                                    self.overlay = Some(Overlay::Launcher { query, selected: 0 });
                                    return None;
                                }
                                other => {
                                    self.overlay = Some(other);
                                    return None;
                                }
                            }
                        }
                    }
                    // Click outside modal dismisses non-editing overlays cleanly.
                    if regions.modal_rect.is_some_and(|r| !contains(r, col, row)) {
                        match &overlay {
                            Overlay::CommandForm(f) if f.editing.is_some() => {
                                self.overlay = Some(overlay);
                            }
                            _ => return None,
                        }
                    } else {
                        self.overlay = Some(overlay);
                    }
                    return None;
                }

                // No overlay open: check tabs first
                for (idx, rect) in regions.tabs.iter().enumerate() {
                    if contains(*rect, col, row) {
                        self.navigate(View::ALL[idx]);
                        self.focus = Focus::List;
                        return None;
                    }
                }
                if regions
                    .clear_filter_chip
                    .is_some_and(|r| contains(r, col, row))
                {
                    self.filter.clear();
                    self.selected = 0;
                    self.list_offset = 0;
                    self.command_selected = 0;
                    return None;
                }
                if regions.filter_chip.is_some_and(|r| contains(r, col, row)) {
                    self.overlay = Some(Overlay::Filter(self.filter.clone()));
                    return None;
                }
                if regions
                    .pane_toggle_chip
                    .is_some_and(|r| contains(r, col, row))
                {
                    self.focus = match self.focus {
                        Focus::List => Focus::Details,
                        Focus::Details => Focus::List,
                    };
                    return None;
                }
                if regions.type_chip.is_some_and(|r| contains(r, col, row))
                    && self.view == View::Discover
                {
                    self.type_filter = self.type_filter.next();
                    self.selected = 0;
                    self.list_offset = 0;
                    return None;
                }
                if regions.compat_chip.is_some_and(|r| contains(r, col, row))
                    && self.view == View::Discover
                {
                    self.show_all = !self.show_all;
                    self.selected = 0;
                    self.list_offset = 0;
                    return None;
                }
                if regions
                    .batch_update_chip
                    .is_some_and(|r| contains(r, col, row))
                    && self.view == View::Updates
                {
                    return self.request(Action::BatchUpdate);
                }
                for (rect, action) in &regions.detail_buttons {
                    if contains(*rect, col, row) {
                        match action {
                            DetailClick::Prepare(Action::Remove) => {
                                return self.handle_remove_request();
                            }
                            DetailClick::Prepare(Action::Update) => {
                                return self.handle_update_request();
                            }
                            DetailClick::Prepare(act) => return self.request(*act),
                            DetailClick::Scan => return (!self.busy).then_some(Command::Scan),
                            DetailClick::Sync => {
                                if !self.busy {
                                    self.overlay = Some(Overlay::Sync);
                                }
                                return None;
                            }
                            DetailClick::RollbackSnapshot(snap_id) => {
                                if self.busy {
                                    return None;
                                }
                                let argv =
                                    vec!["snapshot".into(), "rollback".into(), snap_id.clone()];
                                let effects = self.generic_effects("snapshot rollback", &argv);
                                self.overlay = Some(Overlay::GenericReview {
                                    argv,
                                    display: "snapshot rollback".into(),
                                    effects,
                                    confirm: false,
                                    scroll: 0,
                                });
                                return None;
                            }
                            DetailClick::CycleVersion => {
                                self.cycle_discover_version();
                                return None;
                            }
                            DetailClick::BatchUpdate => {
                                return self.request(Action::BatchUpdate);
                            }
                        }
                    }
                }
                if let Some(area) = regions.list_rows_area {
                    if contains(area, col, row) {
                        let idx = regions.list_offset + usize::from(row - area.y);
                        if self.view == View::Commands {
                            let specs = self.command_specs();
                            if let Some(found) = specs.get(idx) {
                                self.command_selected = idx;
                                self.focus = Focus::List;
                                let display = found.display.clone();
                                self.open_command_form(&display);
                            }
                        } else {
                            let len = self.rows().len();
                            if idx < len {
                                self.selected = idx;
                                self.focus = Focus::List;
                                self.detail_scroll = 0;
                            }
                        }
                        return None;
                    }
                }
                if regions.details_pane.is_some_and(|r| contains(r, col, row)) {
                    self.focus = Focus::Details;
                    return None;
                }
                if regions.list_pane.is_some_and(|r| contains(r, col, row)) {
                    self.focus = Focus::List;
                    return None;
                }
                None
            }
            _ => None,
        }
    }
}
