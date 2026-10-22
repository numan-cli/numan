use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};

use super::catalog::{self, FieldKind, FieldState};
use super::operations::{self, Action, Data, Review};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Installed,
    Discover,
    Updates,
    Commands,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Navigation,
    List,
    Details,
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

pub enum Command {
    Sync,
<<<<<<< HEAD
    /// Run a catalog command through `operations::execute_cli`. The name is
    /// deliberately not "read-only": reviewed mutating commands use it too.
    /// Constructed by the Commands UI in an upper stack layer.
    #[allow(dead_code)]
    ExecuteCli(Vec<String>),
=======
    ExecuteReadOnly(Vec<String>),
>>>>>>> f3629a5 (feat(tui): Commands section with guided forms and visual theme)
    Reload,
    Prepare(Action, String, String, Option<String>),
    Execute(Review),
    Scan,
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
    Navigate(View),
    Native(Action),
    Cli(Vec<String>),
}
#[derive(Clone)]
pub struct LauncherItem {
    pub label: String,
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
    pub error: Option<String>,
    pub notice: Option<String>,
}

pub struct App {
    pub root: PathBuf,
    pub data: Option<Data>,
    pub view: View,
    pub focus: Focus,
    pub selected: usize,
    pub filter: String,
    pub show_all: bool,
    pub pinned: Option<(String, String)>,
    pub overlay: Option<Overlay>,
    pub detail_scroll: u16,
    pub busy: bool,
    pub status: String,
    pub logs: Vec<String>,
    pub exit: bool,
    pub exit_command: Option<String>,
    pub small: bool,
    pub command_selected: usize,
    pub last_package_id: Option<String>,
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

impl App {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.into(),
            data: None,
            view: View::Installed,
            focus: Focus::List,
            selected: 0,
            filter: String::new(),
            show_all: false,
            pinned: None,
            overlay: None,
            detail_scroll: 0,
            busy: false,
            status: "Loading local state; no network on view entry.".into(),
            logs: Vec::new(),
            exit: false,
            exit_command: None,
            small: false,
            command_selected: 0,
            last_package_id: None,
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let Some(data) = &self.data else {
            return Vec::new();
        };
        let default = &data.default_registry;
        let nu = data.nu_valid.then_some(data.nu.as_ref()).flatten();
        let mut rows = Vec::new();
        if self.view == View::Discover {
            for item in &data.catalog {
                let compatible = operations::candidate(item, nu);
                if compatible.is_none() && !self.show_all && data.nu_valid {
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
                let state = if data.installed.packages.contains_key(&id) {
                    "installed"
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
                rows.push(Row { id, version, kind: item.package.package_type.to_string(), state: state.into(), registry: item.registry.clone(),
                    details: format!("{}\n\nRegistry: {}\nSigning key: {}\nIndex SHA256: {}\n\nVersions: {versions}\n\nCompatibility is metadata-based, not runtime proof.\n[v] Choose version; [c] Include incompatible packages.", item.package.description, item.registry, item.signing_key, item.index_digest) });
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
                rows.push(Row { id: id.clone(), version: if self.view == View::Updates { format!("{} > {}", entry.version, next.unwrap_or_default()) } else { entry.version.clone() },
                    kind: entry.package_type.clone(), state, registry,
                    details: format!("{}\n\nOrigin: {}\nPayload: {}\nRevision: {}\nRecorded artifact SHA256: {}\n\nActivation is specific to the cached Nu identity. Future Nu sessions see activation changes.\n\nRecorded verification is not a fresh integrity check.",
                        item.map(|item| item.package.description.as_str()).unwrap_or("Installed package"), entry.origin.as_deref().unwrap_or("unknown"), entry.payload_path(), entry.revision_id.as_deref().unwrap_or("not recorded"), entry.artifact_sha256.as_deref().unwrap_or("not recorded")) });
            }
        }
        let query = self.filter.to_lowercase();
        rows.retain(|row| {
            format!("{} {}", row.id, row.details)
                .to_lowercase()
                .contains(&query)
        });
        rows
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows().into_iter().nth(self.selected)
    }

    pub fn navigate(&mut self, view: View) {
        if let Some(row) = self.selected_row() {
            self.last_package_id = Some(row.id);
        }
        self.command_selected = 0;
        self.view = view;
        self.selected = 0;
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
                    || format!("{} {}", spec.display, spec.summary)
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

    pub fn open_command_form(&mut self, display: &str) {
        let Some(found) = catalog::spec(display) else {
            return;
        };
        let mut fields: Vec<FieldState> = found
            .fields
            .iter()
            .filter(|field| !field.hidden)
            .map(FieldState::from_spec)
            .collect();
        if let Some(id) = self.last_package_id.clone().or_else(|| {
            self.data
                .as_ref()
                .and_then(|data| data.installed.packages.keys().next().cloned())
        }) {
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
            error: None,
            notice: Some(format!(
                "{} | {} | Execute row is at the bottom of this form.",
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
        });
    }

    pub fn launcher_items(&self, query: &str) -> Vec<LauncherItem> {
        let mut items = vec![
            LauncherItem {
                label: "Native: Packages / Installed".into(),
                action: LauncherAction::Navigate(View::Installed),
            },
            LauncherItem {
                label: "Native: Packages / Discover (cached)".into(),
                action: LauncherAction::Navigate(View::Discover),
            },
            LauncherItem {
                label: "Native: Updates (cached)".into(),
                action: LauncherAction::Navigate(View::Updates),
            },
            LauncherItem {
                label: "Native: Commands (all CLI commands)".into(),
                action: LauncherAction::Navigate(View::Commands),
            },
            LauncherItem {
                label: "Native: Local doctor scan (no network, no repairs)".into(),
                action: LauncherAction::Native(Action::Scan),
            },
        ];
        if let Some(row) = self.selected_row() {
            for action in self.available_actions(&row) {
                items.push(LauncherItem {
                    label: format!("Native: {action:?} {}", row.id),
                    action: LauncherAction::Native(action),
                });
            }
            for (label, args) in [
                (
                    "Deactivate selected package",
                    vec!["deactivate".into(), row.id.clone()],
                ),
                ("Try selected package", vec!["try".into(), row.id.clone()]),
                (
                    "Check activation integrity",
                    vec!["activate".into(), row.id.clone(), "--check".into()],
                ),
                (
                    "Inspect nupm import drift",
                    vec!["nupm".into(), "diff".into(), row.id],
                ),
            ] {
                items.push(LauncherItem {
                    label: format!("CLI only: {label}"),
                    action: LauncherAction::Cli(args),
                });
            }
        }
        for (label, args) in [
            ("Initialize root", vec!["init"]),
            ("Refresh Nu identity", vec!["init", "--refresh"]),
            ("Sync registries (network)", vec!["registry", "sync"]),
            ("List registries", vec!["registry", "list"]),
            ("Registry administration", vec!["registry", "--help"]),
            ("List installed Nu versions", vec!["use", "list"]),
            ("Install/manage Nushell", vec!["setup", "nu", "--help"]),
            ("Loader integration", vec!["setup", "loader", "--help"]),
            ("List snapshots", vec!["snapshot", "list"]),
            ("Snapshot rollback/deletion", vec!["snapshot", "--help"]),
            ("Preview garbage collection", vec!["gc", "--dry-run"]),
            ("nupm inspection/import", vec!["nupm", "--help"]),
            ("Shell completions", vec!["completions", "--help"]),
            ("Repair with doctor", vec!["doctor"]),
            (
                "Check Numan self-update",
                vec!["update", "--self", "--check"],
            ),
            ("Update Numan binary", vec!["update", "--self"]),
        ] {
            items.push(LauncherItem {
                label: format!("CLI only: {label}"),
                action: LauncherAction::Cli(args.into_iter().map(str::to_string).collect()),
            });
        }
        items
            .into_iter()
            .filter(|item| item.label.to_lowercase().contains(&query.to_lowercase()))
            .collect()
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
            View::Discover if installed.is_none() && row.state == "compatible" => {
                vec![Action::Install]
            }
            View::Updates => {
                let Some(entry) = installed else {
                    return Vec::new();
                };
                let active = entry.activation.is_some() || entry.module_activation.is_some();
                let allowed = !active
                    || std::env::var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION").as_deref() == Ok("1");
                if allowed && (!active || entry.package_type == "plugin") {
                    vec![Action::Update]
                } else {
                    Vec::new()
                }
            }
            _ => match installed {
                Some(entry) if entry.activation.is_none() && entry.module_activation.is_none() => {
                    let mut actions = vec![Action::Remove];
                    if matches!(row.kind.as_str(), "plugin" | "module") {
                        actions.insert(0, Action::Activate);
                    }
                    actions
                }
                Some(_)
                    if row.state != "active"
                        && matches!(row.kind.as_str(), "plugin" | "module") =>
                {
                    vec![Action::Activate]
                }
                _ => Vec::new(),
            },
        }
    }

    fn request(&self, action: Action) -> Option<Command> {
        if self.busy {
            return None;
        }
        if action == Action::Scan {
            return Some(Command::Scan);
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
        [Action::Install, Action::Activate, Action::Update, Action::Remove].into_iter().map(|action| {
            let reason = if self.busy { "Wait for the current operation to finish." }
                else if row.is_none() { "Select a package first." }
                else if allowed.contains(&action) { "" }
                else { match action {
                    Action::Install => "Already installed. Select Updates to check for newer compatible versions.",
                    Action::Activate => "Already active, or activation is unsupported for this package type. Inspect package state and Nu context.",
                    Action::Update => "Select the Updates section and a newer compatible cached version first.",
                    Action::Remove => "Active packages must be deactivated via CLI before removal. Bundled plugins are managed with Nushell.",
                    Action::Scan => "Use the launcher for doctor scan.",
                }};
            (format!("Review {action:?}"), action, reason.to_string())
        }).collect()
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
                "Self-update replaces the running numan binary. The TUI will advise a restart on success."
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
                        KeyCode::Backspace => {
                            query.pop();
                            selected = 0;
                        }
                        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            query.push(c);
                            selected = 0;
                        }
                        KeyCode::Enter => {
                            if let Some(item) = items.get(selected) {
                                match &item.action {
                                    LauncherAction::Navigate(view) => {
                                        self.navigate(*view);
                                        self.focus = Focus::List;
                                    }
                                    LauncherAction::Native(action) => return self.request(*action),
                                    LauncherAction::Cli(args) => {
                                        self.overlay = Some(Overlay::Handoff {
                                            args: args.clone(),
                                            shell: Shell::Nushell,
                                        })
                                    }
                                }
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
                } => {
                    match key.code {
                        KeyCode::Esc => return None,
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
                    });
                }
                Overlay::CommandForm(mut form) => {
                    if let Some(target) = form.editing {
                        match key.code {
                            KeyCode::Esc => {
                                form.editing = None;
                                form.error = None;
                            }
                            KeyCode::Backspace => {
                                form.edit_buffer.pop();
                            }
                            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                                form.edit_buffer.push(c);
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
                            if let Some(field) = form.fields.get_mut(form.selected) {
                                match field.spec.kind {
                                    FieldKind::Bool => field.enabled = !field.enabled,
                                    FieldKind::Enum if !field.spec.possible.is_empty() => {
                                        let current = field
                                            .spec
                                            .possible
                                            .iter()
                                            .position(|value| value == &field.value)
                                            .unwrap_or(usize::MAX);
                                        let next = if key.code == KeyCode::Right {
                                            if current == usize::MAX {
                                                0
                                            } else {
                                                (current + 1) % field.spec.possible.len()
                                            }
                                        } else if current == usize::MAX {
                                            field.spec.possible.len() - 1
                                        } else {
                                            (current + field.spec.possible.len() - 1)
                                                % field.spec.possible.len()
                                        };
                                        field.value = field.spec.possible[next].clone();
                                    }
                                    _ => {}
                                }
                            }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            if form.selected == form.fields.len() + 1 {
                                match catalog::execution_mode(&form.display, &form.fields) {
                                    catalog::ExecutionMode::Blocked(reason) => {
                                        form.error = Some(reason.to_string());
                                    }
                                    mode => {
                                        if let Some(missing) = form.fields.iter().find(|field| {
                                            field.spec.required
                                                && field.spec.kind != FieldKind::Bool
                                                && field.value.trim().is_empty()
                                        }) {
                                            form.error = Some(format!(
                                                "Required field '{}' is empty.",
                                                missing.spec.id
                                            ));
                                        } else if let Some(found) = catalog::spec(&form.display) {
                                            let argv = catalog::build_argv(found, &form.fields);
                                            if mode == catalog::ExecutionMode::ReadOnly {
                                                return Some(Command::ExecuteReadOnly(argv));
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
                                form.editing = Some(EditTarget::Preview);
                            } else if let Some(field) = form.fields.get_mut(form.selected) {
                                match field.spec.kind {
                                    FieldKind::Bool => field.enabled = !field.enabled,
                                    FieldKind::Enum if !field.spec.possible.is_empty() => {
                                        let current = field
                                            .spec
                                            .possible
                                            .iter()
                                            .position(|value| value == &field.value)
                                            .unwrap_or(usize::MAX);
                                        let next = if current == usize::MAX {
                                            0
                                        } else {
                                            (current + 1) % field.spec.possible.len()
                                        };
                                        field.value = field.spec.possible[next].clone();
                                    }
                                    _ => {
                                        form.edit_buffer = field.value.clone();
                                        form.editing = Some(EditTarget::Field(form.selected));
                                    }
                                }
                            }
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
                            return Some(Command::ExecuteReadOnly(argv));
                        }
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
            KeyCode::Char('q') => { if self.busy { self.status = "Wait for the current operation before quitting.".into(); } else { self.exit = true; } }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match (self.focus, key.code == KeyCode::BackTab) {
                    (Focus::Navigation, false) | (Focus::Details, true) => Focus::List,
                    (Focus::List, false) | (Focus::Navigation, true) => Focus::Details,
                    _ => Focus::Navigation,
                };
            }
            KeyCode::Char('?') => self.text("Help", "Left/Right or Tab/Shift+Tab: focus panes\nArrows/j/k: select or scroll\nEnter: details (never executes from a package row)\n/: filter list    Ctrl+P: launcher\nCommands section: every CLI command with a guided form, editable preview, and in-TUI execution\ni/a/x: review install/activate/remove in Packages\nu: review selected update in Updates\nr: reload local files (no network)    Shift+R: review registry refresh\nv: cycle selected Discover version    c: include incompatible\nl: operation transcript    n: Nu/root context\nEsc: close overlay/back    q: quit when idle\n\nLauncher CLI-only actions print a command on exit; Commands-section actions execute in the TUI after review.\nInstall and activation require separate reviews.\nCancellation is unavailable after submission.".into()),
            KeyCode::Char('l') => self.text("Operation transcript", self.logs.join("\n")),
            KeyCode::Char('n') => {
                let context = self.data.as_ref().and_then(|data| data.nu.as_ref()).map(|nu| format!("Nu: {}\nExecutable: {}\nSHA256: {}\nPlugin registry: {}\nAutoload: {:?}", nu.nu_version, nu.nu_executable, nu.nu_executable_hash, nu.plugin_registry_path, nu.vendor_autoload_dir)).unwrap_or_else(|| "Nu context missing; initialize outside TUI.".into());
                self.text("Context", format!("Root: {}\n{context}", self.root.display()));
            }
            KeyCode::Char('r') if self.focus != Focus::Navigation && !self.busy => return Some(Command::Reload),
            KeyCode::Char('/') if self.focus == Focus::List => self.overlay = Some(Overlay::Filter(self.filter.clone())),
            KeyCode::Char('i') | KeyCode::Char('a') | KeyCode::Char('x') if self.focus != Focus::Navigation && self.view != View::Updates && self.view != View::Commands => {
                let action = match key.code { KeyCode::Char('i') => Action::Install, KeyCode::Char('a') => Action::Activate, _ => Action::Remove };
                return self.request(action);
            }
            KeyCode::Char('u') if self.focus != Focus::Navigation && self.view == View::Updates => return self.request(Action::Update),
            KeyCode::Char('c') if self.focus == Focus::List && self.view == View::Discover => { self.show_all = !self.show_all; self.selected = 0; }
            KeyCode::Char('v') if self.focus != Focus::Navigation && self.view == View::Discover => {
                if let (Some(row), Some(data)) = (self.selected_row(), self.data.as_ref()) {
                    if let Some(item) = data.catalog.iter().find(|item| item.registry == row.registry && item.package.id.to_string() == row.id) {
                        let versions = &item.package.versions;
                        if !versions.is_empty() {
                            let index = versions.iter().position(|v| v.version.to_string() == row.version).unwrap_or(0);
                            self.pinned = Some((row.id, versions[(index + 1) % versions.len()].version.to_string()));
                        }
                    }
                }
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Up | KeyCode::Char('k') => {
                let down = matches!(key.code, KeyCode::Down | KeyCode::Char('j'));
                match self.focus {
                    Focus::Navigation => {
                        let views = [View::Installed, View::Discover, View::Updates, View::Commands];
                        let index = views.iter().position(|view| *view == self.view).unwrap_or(0);
                        self.navigate(views[if down { (index + 1) % 4 } else { (index + 3) % 4 }]);
                    }
                    Focus::List => {
                        if self.view == View::Commands {
                            let count = self.command_specs().len();
                            self.command_selected = if down { (self.command_selected + 1).min(count.saturating_sub(1)) } else { self.command_selected.saturating_sub(1) };
                        } else {
                            self.selected = if down { (self.selected + 1).min(self.rows().len().saturating_sub(1)) } else { self.selected.saturating_sub(1) };
                        }
                        self.detail_scroll = 0;
                    }
                    Focus::Details => { self.detail_scroll = if down { self.detail_scroll.saturating_add(1) } else { self.detail_scroll.saturating_sub(1) }; }
                }
            }
            KeyCode::Left => self.focus = match self.focus { Focus::Details => Focus::List, _ => Focus::Navigation },
            KeyCode::Right => self.focus = match self.focus { Focus::Navigation => Focus::List, _ => Focus::Details },
            KeyCode::Char('R') if !self.busy => self.overlay = Some(Overlay::Sync),
            KeyCode::Enter => { if self.focus == Focus::Navigation { self.focus = Focus::List; } else if self.view == View::Commands && self.focus == Focus::List { if let Some(found) = self.selected_command() { let display = found.display.clone(); self.open_command_form(&display); } } else { self.overlay = Some(Overlay::Actions { selected: 0 }); } }
            KeyCode::Esc => self.focus = Focus::List,
            _ => {}
        }
        None
    }
}
