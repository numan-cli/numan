//! Scheduled background auto-update management and OS scheduler integration.

use anyhow::{bail, Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};

use crate::cmd::update::{self, UpdateArgs};
use crate::core::integrity::compute_sha256;
use crate::util::atomic::write_bytes_atomic;
use crate::util::fs_safety::setup_subcommand_lock;
use crate::util::output::{eprintln, println};

/// Options for `numan setup autoupdate` scheduled background extension updates.
#[derive(Debug, Args, Clone, Default)]
pub struct AutoupdateArgs {
    /// Enable scheduled background auto-updates
    #[arg(long, conflicts_with_all = ["disable", "status", "run_now"])]
    pub enable: bool,

    /// Disable and unregister scheduled background auto-updates
    #[arg(long, conflicts_with_all = ["enable", "status", "run_now"])]
    pub disable: bool,

    /// Display current auto-update configuration and recent log tail
    #[arg(long, conflicts_with_all = ["enable", "disable", "run_now"])]
    pub status: bool,

    /// Run an immediate unattended update of all packages (`numan update --all --yes`)
    #[arg(long, conflicts_with_all = ["enable", "disable", "status"])]
    pub run_now: bool,

    /// Schedule interval (`daily` or `hourly`)
    #[arg(long, default_value = "daily", value_name = "INTERVAL")]
    pub interval: String,

    /// Opt into background active-plugin mutation (`NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1`)
    #[arg(long)]
    pub allow_active_plugin_mutation: bool,

    /// Skip confirmation prompts
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutoupdateState {
    pub schema_version: u32,
    pub enabled: bool,
    pub interval: String,
    pub scheduler: String,
    pub wrapper_path: String,
    pub log_path: String,
    pub updated_at: String,
    #[serde(default)]
    pub allow_active_plugin_mutation: bool,
}

pub(crate) trait AutoupdateScheduler {
    fn scheduler_name(&self) -> &'static str;
    fn enable(&self, root: &Path, wrapper_path: &Path, interval: &str) -> Result<()>;
    fn disable(&self, root: &Path) -> Result<()>;
}

struct OsAutoupdateScheduler;

fn autoupdate_root_id(root: &Path) -> String {
    let digest = compute_sha256(root.to_string_lossy().as_bytes());
    digest[..12].to_string()
}

#[cfg(target_os = "macos")]
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_escape_arg(input: &str) -> String {
    input
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

pub fn format_schtasks_tr(wrapper_path: &Path) -> String {
    format!("\"{}\"", wrapper_path.display())
}

#[cfg(target_os = "windows")]
fn enable_windows_schtasks(root: &Path, wrapper_path: &Path, interval: &str) -> Result<()> {
    let sc = match interval {
        "hourly" => "HOURLY",
        _ => "DAILY",
    };
    let tr = format_schtasks_tr(wrapper_path);
    let mut cmd = std::process::Command::new("schtasks");
    let root_id = autoupdate_root_id(root);
    let task_name = format!("NumanAutoUpdate-{root_id}");
    cmd.args(["/Create", "/F", "/TN", &task_name, "/SC", sc, "/TR", &tr]);
    let output = cmd
        .output()
        .context("Failed to execute 'schtasks' command to register auto-update task")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to register scheduled task via schtasks: {}",
            stderr.trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn disable_windows_schtasks(root: &Path) -> Result<()> {
    let root_id = autoupdate_root_id(root);
    let task_name = format!("NumanAutoUpdate-{root_id}");
    let mut cmd = std::process::Command::new("schtasks");
    cmd.args(["/Delete", "/F", "/TN", &task_name]);
    let output = cmd
        .output()
        .context("Failed to execute 'schtasks' command to unregister auto-update task")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Exit code 1 with "The specified task name ... does not exist" is benign
        if !stderr.contains("does not exist") && !stderr.contains("cannot find") {
            bail!(
                "Failed to unregister scheduled task via schtasks: {}",
                stderr.trim()
            );
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn enable_macos_launchd(root: &Path, wrapper_path: &Path, interval: &str) -> Result<()> {
    let root_id = autoupdate_root_id(root);
    let label = format!("dev.numan.autoupdate.{root_id}");
    let interval_secs = match interval {
        "hourly" => 3600,
        _ => 86400,
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable is not set")?;
    let agents_dir = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents_dir).with_context(|| {
        format!(
            "Failed to create LaunchAgents directory at '{}'",
            agents_dir.display()
        )
    })?;
    let plist_path = agents_dir.join(format!("{label}.plist"));
    let wrapper_str = wrapper_path.to_string_lossy();
    let escaped_wrapper = xml_escape(&wrapper_str);
    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{escaped_wrapper}</string>
    </array>
    <key>StartInterval</key>
    <integer>{interval_secs}</integer>
    <key>RunAtLoad</key>
    <false/>
</dict>
</plist>
"#
    );
    write_bytes_atomic(&plist_path, plist_content.as_bytes()).with_context(|| {
        format!(
            "Failed to write launchd plist to '{}'",
            plist_path.display()
        )
    })?;
    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .output();
    let load = std::process::Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .output()
        .with_context(|| format!("Failed to load launchd agent at '{}'", plist_path.display()))?;
    if !load.status.success() {
        let stderr = String::from_utf8_lossy(&load.stderr);
        bail!("Failed to load launchd agent: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn disable_macos_launchd(root: &Path) -> Result<()> {
    let root_id = autoupdate_root_id(root);
    let label = format!("dev.numan.autoupdate.{root_id}");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable is not set")?;
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist"));
    if plist_path.is_file() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .output();
        let _ = std::fs::remove_file(&plist_path);
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn write_systemd_units(
    units_dir: &Path,
    unit_prefix: &str,
    escaped_wrapper: &str,
    timer_on_calendar: &str,
) -> Result<()> {
    let service_path = units_dir.join(format!("{unit_prefix}.service"));
    let timer_path = units_dir.join(format!("{unit_prefix}.timer"));
    let service_content = format!(
        r#"[Unit]
Description=Numan unattended package auto-update

[Service]
Type=oneshot
ExecStart="{escaped_wrapper}"
"#
    );
    let timer_content = format!(
        r#"[Unit]
Description=Numan unattended package auto-update timer

[Timer]
OnCalendar={timer_on_calendar}
Persistent=true

[Install]
WantedBy=timers.target
"#
    );
    write_bytes_atomic(&service_path, service_content.as_bytes()).with_context(|| {
        format!(
            "Failed to write systemd service unit to '{}'",
            service_path.display()
        )
    })?;
    write_bytes_atomic(&timer_path, timer_content.as_bytes()).with_context(|| {
        format!(
            "Failed to write systemd timer unit to '{}'",
            timer_path.display()
        )
    })?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn enable_linux_systemd(root: &Path, wrapper_path: &Path, interval: &str) -> Result<()> {
    let root_id = autoupdate_root_id(root);
    let unit_prefix = format!("numan-autoupdate-{root_id}");
    let timer_on_calendar = match interval {
        "hourly" => "hourly",
        _ => "daily",
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable is not set")?;
    let units_dir = home.join(".config").join("systemd").join("user");
    std::fs::create_dir_all(&units_dir).with_context(|| {
        format!(
            "Failed to create systemd user units directory at '{}'",
            units_dir.display()
        )
    })?;
    let escaped_wrapper = systemd_escape_arg(&wrapper_path.to_string_lossy());
    write_systemd_units(
        &units_dir,
        &unit_prefix,
        &escaped_wrapper,
        timer_on_calendar,
    )?;

    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .context("Failed to execute 'systemctl --user daemon-reload'")?;
    if !reload.status.success() {
        let stderr = String::from_utf8_lossy(&reload.stderr);
        bail!("Failed to reload systemd user daemon: {}", stderr.trim());
    }
    let timer_name = format!("{unit_prefix}.timer");
    let enable = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", &timer_name])
        .output()
        .with_context(|| {
            format!("Failed to execute 'systemctl --user enable --now {timer_name}'")
        })?;
    if !enable.status.success() {
        let stderr = String::from_utf8_lossy(&enable.stderr);
        bail!("Failed to enable {timer_name}: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn stop_systemd_timer_if_present(timer_name: &str, timer_path: &Path) -> Result<()> {
    if !timer_path.exists() {
        return Ok(());
    }
    let disable = std::process::Command::new("systemctl")
        .args(["--user", "disable", "--now", timer_name])
        .output()
        .with_context(|| {
            format!("Failed to execute 'systemctl --user disable --now {timer_name}'")
        })?;
    if !disable.status.success() {
        let stderr = String::from_utf8_lossy(&disable.stderr);
        bail!("Failed to disable {timer_name}: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn remove_systemd_units_and_reload(paths: &[&Path]) -> Result<()> {
    for path in paths {
        if path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .context("Failed to execute 'systemctl --user daemon-reload'")?;
    if !reload.status.success() {
        let stderr = String::from_utf8_lossy(&reload.stderr);
        bail!("Failed to reload systemd user daemon: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn disable_linux_systemd(root: &Path) -> Result<()> {
    let root_id = autoupdate_root_id(root);
    let unit_prefix = format!("numan-autoupdate-{root_id}");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable is not set")?;
    let units_dir = home.join(".config").join("systemd").join("user");
    let service_path = units_dir.join(format!("{unit_prefix}.service"));
    let timer_path = units_dir.join(format!("{unit_prefix}.timer"));
    if !service_path.exists() && !timer_path.exists() {
        return Ok(());
    }
    let timer_name = format!("{unit_prefix}.timer");
    stop_systemd_timer_if_present(&timer_name, &timer_path)?;
    remove_systemd_units_and_reload(&[&timer_path, &service_path])
}

impl AutoupdateScheduler for OsAutoupdateScheduler {
    fn scheduler_name(&self) -> &'static str {
        #[cfg(target_os = "windows")]
        {
            "schtasks"
        }
        #[cfg(target_os = "macos")]
        {
            "launchd"
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            "systemd-user"
        }
        #[cfg(not(any(target_os = "windows", unix)))]
        {
            "unsupported"
        }
    }

    fn enable(&self, root: &Path, wrapper_path: &Path, interval: &str) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            enable_windows_schtasks(root, wrapper_path, interval)
        }
        #[cfg(target_os = "macos")]
        {
            enable_macos_launchd(root, wrapper_path, interval)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            enable_linux_systemd(root, wrapper_path, interval)
        }
        #[cfg(not(any(target_os = "windows", unix)))]
        {
            let _ = (root, wrapper_path, interval);
            bail!("Scheduled auto-update is not supported on this operating system.")
        }
    }

    fn disable(&self, root: &Path) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            disable_windows_schtasks(root)
        }
        #[cfg(target_os = "macos")]
        {
            disable_macos_launchd(root)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            disable_linux_systemd(root)
        }
        #[cfg(not(any(target_os = "windows", unix)))]
        {
            let _ = root;
            bail!("Scheduled auto-update is not supported on this operating system.")
        }
    }
}

pub fn autoupdate_state_path(root: &Path) -> PathBuf {
    root.join("autoupdate-state.json")
}

pub fn autoupdate_log_path(root: &Path) -> PathBuf {
    root.join("autoupdate.log")
}

pub fn autoupdate_wrapper_path(root: &Path) -> PathBuf {
    let bin_dir = root.join("bin");
    #[cfg(target_os = "windows")]
    {
        bin_dir.join("numan-autoupdate.cmd")
    }
    #[cfg(not(target_os = "windows"))]
    {
        bin_dir.join("numan-autoupdate.sh")
    }
}

fn validate_autoupdate_interval(interval: &str) -> Result<String> {
    match interval.to_ascii_lowercase().as_str() {
        "daily" => Ok("daily".to_string()),
        "hourly" => Ok("hourly".to_string()),
        other => bail!(
            "Unsupported auto-update interval '{other}'. Supported intervals: 'daily', 'hourly'."
        ),
    }
}

pub fn read_autoupdate_state(root: &Path) -> Result<Option<AutoupdateState>> {
    let path = autoupdate_state_path(root);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("Failed to read auto-update state from '{}'", path.display()))?;
    let state = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "Failed to parse auto-update state from '{}'",
            path.display()
        )
    })?;
    Ok(Some(state))
}

struct AutoupdateWrapperConfig<'a> {
    wrapper_path: &'a Path,
    numan_exe: &'a Path,
    root: &'a Path,
    log_path: &'a Path,
    allow_active_plugin_mutation: bool,
}

fn write_autoupdate_wrapper(cfg: &AutoupdateWrapperConfig<'_>) -> Result<()> {
    if let Some(parent) = cfg.wrapper_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create wrapper directory at '{}'",
                parent.display()
            )
        })?;
    }
    let exe_str = cfg.numan_exe.to_string_lossy();
    let root_str = cfg.root.to_string_lossy();
    let log_str = cfg.log_path.to_string_lossy();

    #[cfg(target_os = "windows")]
    {
        let plugin_env_line = if cfg.allow_active_plugin_mutation {
            "set NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1\r\n"
        } else {
            ""
        };
        let content = format!(
            "@echo off\r\n\
             rem Numan automated background update wrapper\r\n\
             set NUMAN_ROOT={root_str}\r\n\
             {plugin_env_line}\
             echo [%%date%% %%time%%] Running numan background auto-update >> \"{log_str}\"\r\n\
             \"{exe_str}\" --root \"{root_str}\" update --all --yes >> \"{log_str}\" 2>&1\r\n"
        );
        write_bytes_atomic(cfg.wrapper_path, content.as_bytes()).with_context(|| {
            format!(
                "Failed to write wrapper script to '{}'",
                cfg.wrapper_path.display()
            )
        })?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        let plugin_env_line = if cfg.allow_active_plugin_mutation {
            "export NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1\n"
        } else {
            ""
        };
        let content = format!(
            "#!/usr/bin/env sh\n\
             # Numan automated background update wrapper\n\
             set -eu\n\
             export NUMAN_ROOT=\"{root_str}\"\n\
             {plugin_env_line}\
             echo \"[$(date)] Running numan background auto-update\" >> \"{log_str}\"\n\
             exec \"{exe_str}\" --root \"{root_str}\" update --all --yes >> \"{log_str}\" 2>&1\n"
        );
        write_bytes_atomic(cfg.wrapper_path, content.as_bytes()).with_context(|| {
            format!(
                "Failed to write wrapper script to '{}'",
                cfg.wrapper_path.display()
            )
        })?;
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        let _ = std::fs::set_permissions(cfg.wrapper_path, perms);
    }
    Ok(())
}

pub fn execute_autoupdate(args: &AutoupdateArgs, root: &Path) -> Result<()> {
    let numan_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("numan"));
    execute_autoupdate_with_scheduler(args, root, &numan_exe, &OsAutoupdateScheduler)
}

fn validate_autoupdate_action_flags(args: &AutoupdateArgs) -> Result<()> {
    let selected = [args.enable, args.disable, args.status, args.run_now]
        .into_iter()
        .filter(|&flag| flag)
        .count();
    if selected > 1 {
        bail!("--enable, --disable, --status, and --run-now are mutually exclusive.");
    }
    Ok(())
}

fn write_autoupdate_state(root: &Path, state: &AutoupdateState) -> Result<()> {
    let state_bytes = serde_json::to_vec_pretty(state)
        .context("Failed to serialize auto-update state to JSON")?;
    let path = autoupdate_state_path(root);
    write_bytes_atomic(&path, &state_bytes)
        .with_context(|| format!("Failed to write auto-update state to '{}'", path.display()))?;
    Ok(())
}

fn enable_autoupdate_locked(
    args: &AutoupdateArgs,
    root: &Path,
    numan_exe: &Path,
    scheduler: &dyn AutoupdateScheduler,
) -> Result<()> {
    let interval = validate_autoupdate_interval(&args.interval)?;
    let wrapper_path = autoupdate_wrapper_path(root);
    let log_path = autoupdate_log_path(root);

    let wrapper_cfg = AutoupdateWrapperConfig {
        wrapper_path: &wrapper_path,
        numan_exe,
        root,
        log_path: &log_path,
        allow_active_plugin_mutation: args.allow_active_plugin_mutation,
    };
    write_autoupdate_wrapper(&wrapper_cfg)?;

    let abs_wrapper = std::path::absolute(&wrapper_path).unwrap_or_else(|_| wrapper_path.clone());
    scheduler
        .enable(root, &abs_wrapper, &interval)
        .with_context(|| {
            format!(
                "Failed to register task with scheduler '{}'",
                scheduler.scheduler_name()
            )
        })?;

    let state = AutoupdateState {
        schema_version: 1,
        enabled: true,
        interval,
        scheduler: scheduler.scheduler_name().to_string(),
        wrapper_path: abs_wrapper.to_string_lossy().to_string(),
        log_path: log_path.to_string_lossy().to_string(),
        updated_at: crate::util::format_timestamp(),
        allow_active_plugin_mutation: args.allow_active_plugin_mutation,
    };
    write_autoupdate_state(root, &state)?;

    println!(
        "Auto-update enabled successfully with scheduler '{}'.",
        scheduler.scheduler_name()
    );
    println!("  Interval:     {}", state.interval);
    println!("  Wrapper:      {}", wrapper_path.display());
    println!("  Log file:     {}", log_path.display());
    if args.allow_active_plugin_mutation {
        println!("  Active plugins: enabled for unattended auto-mutation");
    }
    Ok(())
}

fn disable_autoupdate_locked(root: &Path, scheduler: &dyn AutoupdateScheduler) -> Result<()> {
    scheduler.disable(root).with_context(|| {
        format!(
            "Failed to unregister task from scheduler '{}'",
            scheduler.scheduler_name()
        )
    })?;

    let wrapper_path = autoupdate_wrapper_path(root);
    if wrapper_path.is_file() {
        let _ = std::fs::remove_file(&wrapper_path);
    }

    let prior_state = read_autoupdate_state(root)?;
    let state = AutoupdateState {
        schema_version: 1,
        enabled: false,
        interval: prior_state
            .as_ref()
            .map(|s| s.interval.clone())
            .unwrap_or_else(|| "daily".to_string()),
        scheduler: scheduler.scheduler_name().to_string(),
        wrapper_path: wrapper_path.to_string_lossy().to_string(),
        log_path: autoupdate_log_path(root).to_string_lossy().to_string(),
        updated_at: crate::util::format_timestamp(),
        allow_active_plugin_mutation: prior_state
            .as_ref()
            .map(|s| s.allow_active_plugin_mutation)
            .unwrap_or(false),
    };
    write_autoupdate_state(root, &state)?;

    println!(
        "Auto-update disabled successfully with scheduler '{}'.",
        scheduler.scheduler_name()
    );
    Ok(())
}

fn run_autoupdate_now(args: &AutoupdateArgs, root: &Path) -> Result<()> {
    let update_args = UpdateArgs::unattended_all();
    let state = read_autoupdate_state(root)?;
    let allow_plugin = args.allow_active_plugin_mutation
        || state
            .as_ref()
            .map(|s| s.allow_active_plugin_mutation)
            .unwrap_or(false);
    let original_var = std::env::var_os("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION");
    if allow_plugin {
        std::env::set_var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION", "1");
    }
    let res = update::execute(&update_args, root);
    match original_var {
        Some(v) => std::env::set_var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION", v),
        None => std::env::remove_var("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION"),
    }
    res
}

pub(crate) fn execute_autoupdate_with_scheduler(
    args: &AutoupdateArgs,
    root: &Path,
    numan_exe: &Path,
    scheduler: &dyn AutoupdateScheduler,
) -> Result<()> {
    validate_autoupdate_action_flags(args)?;

    if args.run_now {
        return run_autoupdate_now(args, root);
    }

    if args.status {
        return execute_autoupdate_status(root, scheduler.scheduler_name());
    }

    if args.enable {
        setup_subcommand_lock(root, "setup autoupdate --enable", || {
            enable_autoupdate_locked(args, root, numan_exe, scheduler)
        })
    } else if args.disable {
        setup_subcommand_lock(root, "setup autoupdate --disable", || {
            disable_autoupdate_locked(root, scheduler)
        })
    } else {
        execute_autoupdate_status(root, scheduler.scheduler_name())
    }
}

fn print_autoupdate_state_summary(state: &AutoupdateState) {
    let status_str = if state.enabled { "enabled" } else { "disabled" };
    println!("  Status:     {status_str}");
    println!("  Interval:   {}", state.interval);
    println!("  Scheduler:  {}", state.scheduler);
    println!("  Wrapper:    {}", state.wrapper_path);
    println!("  Log file:   {}", state.log_path);
    println!("  Updated at: {}", state.updated_at);
    println!(
        "  Active plugins: {}",
        if state.allow_active_plugin_mutation {
            "allowed"
        } else {
            "disallowed"
        }
    );
}

fn print_recent_autoupdate_log(log_path: &Path) {
    if !log_path.is_file() {
        println!("\nNo auto-update log records found.");
        return;
    }
    println!("\nRecent auto-update log output (tail):");
    match std::fs::read_to_string(log_path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(10);
            for line in &lines[start..] {
                println!("  {line}");
            }
        }
        Err(err) => {
            eprintln!("  Failed to read log file: {err}");
        }
    }
}

fn execute_autoupdate_status(root: &Path, default_scheduler: &str) -> Result<()> {
    println!("Numan Auto-Update Status:");
    match read_autoupdate_state(root)? {
        Some(state) => print_autoupdate_state_summary(&state),
        None => {
            println!("  Status:     not configured (run 'numan setup autoupdate --enable')");
            println!("  Scheduler:  {default_scheduler}");
            println!("  Log file:   {}", autoupdate_log_path(root).display());
        }
    }
    print_recent_autoupdate_log(&autoupdate_log_path(root));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct FakeAutoupdateScheduler {
        enabled_calls: std::sync::Mutex<Vec<(PathBuf, String)>>,
        disabled_calls: std::sync::Mutex<usize>,
    }

    impl FakeAutoupdateScheduler {
        fn new() -> Self {
            Self {
                enabled_calls: std::sync::Mutex::new(Vec::new()),
                disabled_calls: std::sync::Mutex::new(0),
            }
        }
    }

    impl AutoupdateScheduler for FakeAutoupdateScheduler {
        fn scheduler_name(&self) -> &'static str {
            "fake-scheduler"
        }

        fn enable(&self, _root: &Path, wrapper_path: &Path, interval: &str) -> Result<()> {
            self.enabled_calls
                .lock()
                .unwrap()
                .push((wrapper_path.to_path_buf(), interval.to_string()));
            Ok(())
        }

        fn disable(&self, _root: &Path) -> Result<()> {
            *self.disabled_calls.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[test]
    fn setup_autoupdate_enable_writes_wrapper_and_state() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let fake_exe = dir.path().join("numan-bin");
        let scheduler = FakeAutoupdateScheduler::new();

        let args = AutoupdateArgs {
            enable: true,
            disable: false,
            status: false,
            run_now: false,
            interval: "hourly".to_string(),
            allow_active_plugin_mutation: false,
            yes: true,
        };

        execute_autoupdate_with_scheduler(&args, &root, &fake_exe, &scheduler).unwrap();

        let wrapper = autoupdate_wrapper_path(&root);
        assert!(wrapper.is_file(), "wrapper script must be written");
        let wrapper_content = std::fs::read_to_string(&wrapper).unwrap();
        assert!(
            !wrapper_content.contains("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1"),
            "active plugin mutation must require explicit --allow-active-plugin-mutation"
        );
        assert!(wrapper_content.contains("update --all --yes"));

        let calls = scheduler.enabled_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, std::path::absolute(&wrapper).unwrap());
        assert_eq!(calls[0].1, "hourly");

        let state = read_autoupdate_state(&root).unwrap().expect("state exists");
        assert!(state.enabled);
        assert_eq!(state.interval, "hourly");
        assert_eq!(state.scheduler, "fake-scheduler");
        assert!(!state.allow_active_plugin_mutation);
    }

    #[test]
    fn setup_autoupdate_enable_with_plugin_mutation_sets_env_in_wrapper() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let fake_exe = dir.path().join("numan-bin");
        let scheduler = FakeAutoupdateScheduler::new();

        let args = AutoupdateArgs {
            enable: true,
            disable: false,
            status: false,
            run_now: false,
            interval: "daily".to_string(),
            allow_active_plugin_mutation: true,
            yes: true,
        };

        execute_autoupdate_with_scheduler(&args, &root, &fake_exe, &scheduler).unwrap();

        let wrapper = autoupdate_wrapper_path(&root);
        let wrapper_content = std::fs::read_to_string(&wrapper).unwrap();
        assert!(
            wrapper_content.contains("NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1"),
            "wrapper must export NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1 when flag is set"
        );

        let state = read_autoupdate_state(&root).unwrap().expect("state exists");
        assert!(state.allow_active_plugin_mutation);
    }

    #[test]
    fn setup_autoupdate_rejects_conflicting_action_flags() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let fake_exe = dir.path().join("numan-bin");
        let scheduler = FakeAutoupdateScheduler::new();

        let args = AutoupdateArgs {
            enable: true,
            disable: true,
            status: false,
            run_now: false,
            interval: "daily".to_string(),
            allow_active_plugin_mutation: false,
            yes: true,
        };

        let err = execute_autoupdate_with_scheduler(&args, &root, &fake_exe, &scheduler)
            .unwrap_err()
            .to_string();
        assert!(err.contains("mutually exclusive"));
    }

    #[test]
    fn setup_autoupdate_disable_removes_wrapper_and_updates_state() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("numan-root");
        let fake_exe = dir.path().join("numan-bin");
        let scheduler = FakeAutoupdateScheduler::new();

        let enable_args = AutoupdateArgs {
            enable: true,
            disable: false,
            status: false,
            run_now: false,
            interval: "daily".to_string(),
            allow_active_plugin_mutation: true,
            yes: true,
        };
        execute_autoupdate_with_scheduler(&enable_args, &root, &fake_exe, &scheduler).unwrap();

        let disable_args = AutoupdateArgs {
            enable: false,
            disable: true,
            status: false,
            run_now: false,
            interval: "daily".to_string(),
            allow_active_plugin_mutation: false,
            yes: true,
        };
        execute_autoupdate_with_scheduler(&disable_args, &root, &fake_exe, &scheduler).unwrap();

        let wrapper = autoupdate_wrapper_path(&root);
        assert!(
            !wrapper.exists(),
            "wrapper script must be removed on disable"
        );

        let disabled_count = *scheduler.disabled_calls.lock().unwrap();
        assert_eq!(disabled_count, 1);

        let state = read_autoupdate_state(&root).unwrap().expect("state exists");
        assert!(!state.enabled);
        assert!(state.allow_active_plugin_mutation);
    }

    #[test]
    fn setup_autoupdate_windows_schtasks_quotes_tr() {
        let path_with_spaces = Path::new(r#"C:\Users\Jane Doe\.numan\bin\numan-autoupdate.cmd"#);
        let tr = format_schtasks_tr(path_with_spaces);
        assert_eq!(
            tr, r#""C:\Users\Jane Doe\.numan\bin\numan-autoupdate.cmd""#,
            "TR value must be wrapped in literal double quotes"
        );
    }
}
