//! Clap-derived catalog of every leaf CLI command.
//!
//! The command list, summaries, and fields are built from `Cli::command()` so a
//! new CLI command or flag cannot silently drift out of the TUI. The only
//! hand-maintained data is the per-command classification (tier + phase); the
//! completeness test below fails if any discovered leaf lacks one.

use crate::cli::Cli;
use clap::{ArgAction, CommandFactory, Parser};
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    PackageId,
    Version,
    Path,
    SnapshotId,
    RegistryName,
    Text,
    Enum,
    Bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSpec {
    pub id: String,
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
    /// Option/flag (`--key value` or `--flag`); otherwise positional.
    pub option: bool,
    pub long: Option<String>,
    pub possible: Vec<String>,
    pub multi: bool,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub path: Vec<String>,
    pub display: String,
    pub summary: String,
    pub tier: &'static str,
    pub phase: &'static str,
    pub fields: Vec<FieldSpec>,
}

/// path, tier, phase/availability label
const CLASSIFICATIONS: &[(&str, &str, &str)] = &[
    ("tui", "ReadOnly", "This section"),
    ("search", "ReadOnly", "Phase 1 - read-only"),
    ("info", "ReadOnly", "Phase 1 - read-only"),
    (
        "install",
        "RootMutation+Network",
        "Phase 2 (force: Phase 3)",
    ),
    (
        "update",
        "RootMutation+Network",
        "Phase 2 single (all/self: Phase 3-4)",
    ),
    ("remove", "Destructive", "Phase 2 (force: Phase 3)"),
    ("gc", "Destructive", "Phase 3"),
    ("activate", "RootMutation", "Phase 2 single (all: Phase 3)"),
    ("deactivate", "RootMutation", "Phase 2"),
    ("list", "ReadOnly", "Phase 1 - read-only"),
    ("init", "RootMutation", "Phase 3"),
    ("migrate", "RootMutation", "Phase 3"),
    (
        "completions",
        "External",
        "Phase 1 --print (install: Phase 4)",
    ),
    (
        "doctor",
        "ReadOnly/External",
        "Phase 1 scan (repair: Phase 4)",
    ),
    ("try", "RootMutation+Network", "Phase 2"),
    ("use", "RootMutation", "Phase 3 (list: Phase 1)"),
    ("hook enable", "RootMutation", "Phase 3"),
    ("hook disable", "RootMutation", "Phase 3"),
    ("hook list", "ReadOnly", "Phase 1 - read-only"),
    ("registry list", "ReadOnly", "Phase 1 - read-only"),
    ("registry sync", "Network", "Native today (Shift+R)"),
    ("registry add", "RootMutation+Network", "Phase 3"),
    ("registry remove", "Destructive", "Phase 3"),
    ("registry packages", "ReadOnly", "Phase 1 - read-only"),
    ("snapshot list", "ReadOnly", "Phase 1 - read-only"),
    ("snapshot inspect", "ReadOnly", "Phase 1 - read-only"),
    ("snapshot delete", "Destructive", "Phase 3"),
    ("snapshot rollback", "Destructive", "Phase 3"),
    ("setup nu", "External+Network", "Phase 4"),
    ("setup nu remove", "External", "Phase 4"),
    ("setup nu path", "External", "Phase 4"),
    ("setup nu use", "External", "Phase 4"),
    ("setup loader", "External+Network", "Phase 4"),
    ("nupm status", "ReadOnly", "Phase 1 - read-only"),
    ("nupm inspect", "ReadOnly", "Phase 1 - read-only"),
    ("nupm import", "RootMutation", "Phase 3"),
    ("nupm diff", "ReadOnly", "Phase 1 - read-only"),
    (
        "overlay use",
        "RootMutation+Shell",
        "Phase 3 (shell exception)",
    ),
    (
        "overlay hide",
        "RootMutation+Shell",
        "Phase 3 (shell exception)",
    ),
    ("overlay list", "ReadOnly", "Phase 1 - read-only"),
];

/// Summaries for leaves whose Clap variant carries no `about` text.
const SUMMARY_FALLBACKS: &[(&str, &str)] = &[
    ("nupm status", "Show nupm coexistence status"),
    ("nupm inspect", "Inspect nupm package candidates"),
    ("nupm import", "Import a nupm package into Numan"),
    ("nupm diff", "Show drift for a nupm import"),
];

fn classify(display: &str) -> (&'static str, &'static str) {
    CLASSIFICATIONS
        .iter()
        .find(|(path, _, _)| *path == display)
        .map(|(_, tier, phase)| (*tier, *phase))
        .unwrap_or(("Unclassified", "Blocked: no descriptor"))
}

fn field_kind(path: &str, id: &str, possible: &[String]) -> FieldKind {
    if !possible.is_empty() {
        return FieldKind::Enum;
    }
    if id.contains("package") || (id == "id" && path == "info") {
        FieldKind::PackageId
    } else if path.starts_with("snapshot") && id == "id" {
        FieldKind::SnapshotId
    } else if path.starts_with("registry") && id == "name" {
        FieldKind::RegistryName
    } else if id.contains("path") || id.contains("home") || id.contains("root") || id == "manifest"
    {
        FieldKind::Path
    } else if id.contains("version") || (id == "package" && path == "install") {
        FieldKind::Version
    } else {
        FieldKind::Text
    }
}

fn build_fields(path: &str, cmd: &clap::Command) -> Vec<FieldSpec> {
    let mut fields = Vec::new();
    for arg in cmd.get_arguments() {
        let id = arg.get_id().to_string();
        if matches!(id.as_str(), "help" | "version" | "root") {
            continue;
        }
        let possible: Vec<String> = arg
            .get_possible_values()
            .iter()
            .map(|value| value.get_name().to_string())
            .collect();
        let is_bool = matches!(arg.get_action(), ArgAction::SetTrue | ArgAction::Count);
        let kind = if is_bool {
            FieldKind::Bool
        } else {
            field_kind(path, &id, &possible)
        };
        let label = arg
            .get_help()
            .map(|help| help.to_string())
            .filter(|help| !help.is_empty())
            .unwrap_or_else(|| id.replace('_', " "));
        fields.push(FieldSpec {
            label,
            kind,
            required: arg.is_required_set(),
            option: arg.get_long().is_some() || arg.get_short().is_some(),
            long: arg.get_long().map(str::to_string),
            possible,
            multi: matches!(arg.get_action(), ArgAction::Append)
                || arg
                    .get_num_args()
                    .is_some_and(|range| range.max_values() > 1),
            hidden: arg.is_hide_set(),
            id,
        });
    }
    fields
}

fn walk(cmd: &clap::Command, prefix: &[String], out: &mut Vec<CommandSpec>) {
    let children: Vec<&clap::Command> = cmd
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
        .collect();
    let has_own_args = cmd
        .get_arguments()
        .any(|arg| !matches!(arg.get_id().as_str(), "help" | "version" | "root"));
    let is_leaf = children.is_empty() || (has_own_args && !cmd.is_subcommand_required_set());
    if is_leaf && !prefix.is_empty() {
        let display = prefix.join(" ");
        let (tier, phase) = classify(&display);
        out.push(CommandSpec {
            path: prefix.to_vec(),
            summary: cmd
                .get_about()
                .map(|about| about.to_string())
                .filter(|about| !about.is_empty())
                .or_else(|| {
                    SUMMARY_FALLBACKS
                        .iter()
                        .find(|(path, _)| *path == display)
                        .map(|(_, summary)| summary.to_string())
                })
                .unwrap_or_default(),
            fields: build_fields(&display, cmd),
            display,
            tier,
            phase,
        });
    }
    for child in children {
        let mut next = prefix.to_vec();
        next.push(child.get_name().to_string());
        walk(child, &next, out);
    }
}

pub fn catalog() -> &'static [CommandSpec] {
    static CATALOG: OnceLock<Vec<CommandSpec>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let cmd = Cli::command();
        let mut specs = Vec::new();
        for child in cmd.get_subcommands() {
            if child.get_name() != "help" {
                walk(child, &[child.get_name().to_string()], &mut specs);
            }
        }
        specs.sort_by(|a, b| a.display.cmp(&b.display));
        specs
    })
}

pub fn spec(display: &str) -> Option<&'static CommandSpec> {
    catalog().iter().find(|spec| spec.display == display)
}

/// Editable state for one form field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldState {
    pub spec: FieldSpec,
    pub value: String,
    pub enabled: bool,
}

impl FieldState {
    pub fn from_spec(spec: &FieldSpec) -> Self {
        Self {
            spec: spec.clone(),
            value: String::new(),
            enabled: false,
        }
    }
}

/// Build argv (command path first, no `numan`/`--root`) from field states.
/// Unset optional fields are omitted so CLI defaults apply unchanged.
pub fn build_argv(spec: &CommandSpec, fields: &[FieldState]) -> Vec<String> {
    let mut argv = spec.path.clone();
    for field in fields {
        if field.spec.hidden {
            continue;
        }
        if field.spec.kind == FieldKind::Bool {
            if field.enabled {
                argv.push(format!(
                    "--{}",
                    field.spec.long.as_deref().unwrap_or(&field.spec.id)
                ));
            }
            continue;
        }
        let value = field.value.trim();
        if value.is_empty() {
            continue;
        }
        if field.spec.option {
            argv.push(format!(
                "--{}",
                field.spec.long.as_deref().unwrap_or(&field.spec.id)
            ));
        }
        if field.spec.multi {
            argv.extend(value.split_whitespace().map(str::to_string));
        } else {
            argv.push(value.to_string());
        }
    }
    argv
}

/// Whether this command may execute read-only in the TUI (Phase 1).
/// Conditional commands depend on current field values.
pub fn phase1_executable(display: &str, fields: &[FieldState]) -> bool {
    match display {
        "search" | "info" | "list" | "registry list" | "registry packages" | "snapshot list"
        | "snapshot inspect" | "nupm status" | "nupm inspect" | "nupm diff" | "hook list"
        | "overlay list" => true,
        "completions" => fields
            .iter()
            .any(|field| field.spec.id == "print" && field.enabled),
        "doctor" => fields
            .iter()
            .any(|field| (field.spec.id == "scan" || field.spec.id == "json") && field.enabled),
        "activate" => fields
            .iter()
            .any(|field| (field.spec.id == "list" || field.spec.id == "check") && field.enabled),
        "update" => {
            let enabled = |id: &str| {
                fields
                    .iter()
                    .any(|field| field.spec.id == id && field.enabled)
            };
            enabled("check") && !enabled("self")
        }
        "use" => fields
            .iter()
            .any(|field| field.spec.id == "version" && field.value.trim() == "list"),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Runs immediately, no review.
    ReadOnly,
    /// Opens the generic review; confirmation executes.
    Reviewed,
    /// Cannot run inside the TUI.
    Blocked(&'static str),
}

/// How the Commands form may run this command with its current field values.
pub fn execution_mode(display: &str, fields: &[FieldState]) -> ExecutionMode {
    if display == "tui" {
        return ExecutionMode::Blocked("The TUI cannot open inside itself.");
    }
    if phase1_executable(display, fields) {
        ExecutionMode::ReadOnly
    } else {
        ExecutionMode::Reviewed
    }
}

/// Load field states from argv tokens that follow the command path.
pub fn fields_from_argv(spec: &CommandSpec, rest: &[String]) -> Vec<FieldState> {
    let mut fields: Vec<FieldState> = spec.fields.iter().map(FieldState::from_spec).collect();
    let mut positionals = 0usize;
    let mut index = 0usize;
    while index < rest.len() {
        let token = &rest[index];
        if let Some(long) = token.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            if let Some(field) = fields
                .iter_mut()
                .find(|f| f.spec.long.as_deref() == Some(name))
            {
                if field.spec.kind == FieldKind::Bool {
                    field.enabled = true;
                } else if let Some(value) = inline {
                    field.value = value;
                    index += 1;
                    continue;
                } else if index + 1 < rest.len() {
                    field.value = rest[index + 1].clone();
                    index += 2;
                    continue;
                }
            }
            index += 1;
            continue;
        }
        let positional_indexes: Vec<usize> = fields
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.spec.option && f.spec.kind != FieldKind::Bool)
            .map(|(i, _)| i)
            .collect();
        if let Some(&field_index) = positional_indexes.get(positionals) {
            let field = &mut fields[field_index];
            if field.spec.multi {
                if !field.value.is_empty() {
                    field.value.push(' ');
                }
                field.value.push_str(token);
            } else {
                field.value = token.clone();
                positionals += 1;
            }
        }
        index += 1;
    }
    fields
}

/// Non-shell tokenizer for the editable preview. Shell metacharacters are
/// rejected outright; the result is only ever re-parsed by Clap, never a shell.
pub fn tokenize(input: &str) -> Result<Vec<String>, String> {
    if input
        .chars()
        .any(|c| matches!(c, ';' | '|' | '&' | '$' | '<' | '>' | '`' | '\n' | '\r'))
    {
        return Err("Shell characters (; | & $ < > ` and newlines) are not allowed.".into());
    }
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(active) if c == active => quote = None,
            Some('"') if c == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started || !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => current.push(c),
        }
    }
    if quote.is_some() {
        return Err("Unterminated quote in command preview.".into());
    }
    if started || !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

/// Validate an edited preview against the fixed root and the real Clap parser.
/// Returns the argv after the command path on success.
pub fn validate_preview(root: &Path, input: &str) -> Result<(CommandSpec, Vec<String>), String> {
    let tokens = tokenize(input)?;
    let root_text = root.to_string_lossy().to_string();
    if tokens.first().map(String::as_str) != Some("numan") {
        return Err("Preview must start with `numan`.".into());
    }
    if tokens.get(1).map(String::as_str) != Some("--root") || tokens.get(2) != Some(&root_text) {
        return Err("Preview root is fixed to the TUI root; --root cannot be edited.".into());
    }
    let rest = &tokens[3..];
    let found = catalog()
        .iter()
        .filter(|spec| rest.starts_with(&spec.path))
        .max_by_key(|spec| spec.path.len())
        .ok_or_else(|| "Preview does not name a known numan command.".to_string())?;
    let parse_argv: Vec<String> = std::iter::once("numan".to_string())
        .chain(tokens[1..].iter().cloned())
        .collect();
    Cli::try_parse_from(&parse_argv).map_err(|error| error.to_string())?;
    Ok((found.clone(), rest[found.path.len()..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn discovered_leaves() -> Vec<String> {
        fn walk(cmd: &clap::Command, prefix: &[String], out: &mut Vec<String>) {
            let children: Vec<&clap::Command> = cmd
                .get_subcommands()
                .filter(|sub| sub.get_name() != "help")
                .collect();
            let has_own_args = cmd
                .get_arguments()
                .any(|arg| !matches!(arg.get_id().as_str(), "help" | "version" | "root"));
            if (children.is_empty() || (has_own_args && !cmd.is_subcommand_required_set()))
                && !prefix.is_empty()
            {
                out.push(prefix.join(" "));
            }
            for child in children {
                let mut next = prefix.to_vec();
                next.push(child.get_name().to_string());
                walk(child, &next, out);
            }
        }
        let cmd = Cli::command();
        let mut leaves = Vec::new();
        for child in cmd.get_subcommands() {
            if child.get_name() != "help" {
                walk(child, &[child.get_name().to_string()], &mut leaves);
            }
        }
        leaves.sort();
        leaves
    }

    #[test]
    fn catalog_covers_every_clap_leaf_with_classification_and_fields() {
        let leaves = discovered_leaves();
        assert_eq!(leaves.len(), 40, "leaf count changed: {leaves:?}");
        assert_eq!(catalog().len(), leaves.len());
        for leaf in &leaves {
            let found = spec(leaf).unwrap_or_else(|| panic!("no catalog spec for {leaf}"));
            assert_ne!(found.tier, "Unclassified", "{leaf} lacks classification");
            assert!(!found.summary.is_empty(), "{leaf} lacks a Clap summary");
            for field in &found.fields {
                assert!(!field.id.is_empty());
            }
        }
        assert_eq!(CLASSIFICATIONS.len(), leaves.len());
    }

    #[test]
    fn form_argv_round_trips_through_clap() {
        let root = Path::new("/tmp/numan-root");
        for display in ["snapshot rollback", "registry add", "install", "activate"] {
            let found = spec(display).unwrap();
            let mut fields: Vec<FieldState> =
                found.fields.iter().map(FieldState::from_spec).collect();
            for field in &mut fields {
                match field.spec.kind {
                    FieldKind::Bool => field.enabled = false,
                    FieldKind::Enum => {
                        field.value = field.spec.possible.first().cloned().unwrap_or_default()
                    }
                    FieldKind::PackageId => field.value = "owner/pkg".into(),
                    FieldKind::SnapshotId => {
                        field.value = "018f0000-0000-7000-8000-000000000000".into()
                    }
                    FieldKind::RegistryName => field.value = "custom".into(),
                    FieldKind::Path => field.value = "/tmp/x".into(),
                    FieldKind::Version => field.value = "owner/pkg@1.0.0".into(),
                    FieldKind::Text => {
                        field.value = if field.spec.id == "url" {
                            "https://example.invalid/index.json".into()
                        } else if field.spec.id == "key" {
                            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into()
                        } else {
                            "value".into()
                        }
                    }
                }
            }
            let argv = build_argv(found, &fields);
            let preview = format!("numan --root {} {}", root.display(), argv.join(" "));
            let (parsed, rest) = validate_preview(root, &preview)
                .unwrap_or_else(|error| panic!("{display}: {error}"));
            assert_eq!(parsed.display, *display);
            let rebuilt = fields_from_argv(&parsed, &rest);
            assert_eq!(build_argv(&parsed, &rebuilt), argv, "{display}");
        }
    }

    #[test]
    fn preview_rejects_shell_syntax_root_change_and_unknown_commands() {
        let root = Path::new("/tmp/numan-root");
        for bad in [
            "numan --root /tmp/numan-root list; rm -rf /",
            "numan --root /tmp/numan-root list | cat",
            "numan --root /tmp/numan-root list $(whoami)",
            "numan --root /other list",
            "numan --root /tmp/numan-root frobnicate",
            "numan --root /tmp/numan-root snapshot rollback",
            "numan --root /tmp/numan-root 'list",
        ] {
            assert!(validate_preview(root, bad).is_err(), "{bad}");
        }
        assert!(validate_preview(root, "numan --root /tmp/numan-root list").is_ok());
    }
}
