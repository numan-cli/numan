//! Optional, explicit terminal workbench. No network or mutation on view entry.
pub mod app;
pub mod catalog;
pub mod operations;
#[cfg(test)]
mod tests;
pub mod theme;
pub mod ui;

use crate::util::output::{self, OutputEvent};
use anyhow::{Context, Result};
use app::{App, Command, FollowUp, Overlay};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

enum Completion {
    Loaded(Result<Box<operations::Data>>),
    Prepared(Result<operations::Review>),
    Finished(Result<String>),
}

struct Worker {
    handle: Option<JoinHandle<Completion>>,
    events: Receiver<OutputEvent>,
}

impl Worker {
    fn start(root: PathBuf, command: Command) -> Self {
        let (send, events) = mpsc::channel();
        let handle = thread::spawn(move || {
            output::with_sink(
                move |event| {
                    let _ = send.send(event);
                },
                || match command {
                    Command::Sync => Completion::Finished(operations::sync(&root)),
                    Command::ExecuteCli(argv) => {
                        Completion::Finished(operations::execute_cli(&root, &argv))
                    }
                    Command::Reload => Completion::Loaded(operations::load(&root).map(Box::new)),
                    Command::Prepare(action, id, registry, pinned) => Completion::Prepared(
                        operations::prepare(&root, action, &id, &registry, pinned.as_deref()),
                    ),
                    Command::Execute(review) => {
                        Completion::Finished(operations::execute(&root, &review))
                    }
                    Command::Scan => Completion::Finished(operations::scan(&root)),
                },
            )
        });
        Self {
            handle: Some(handle),
            events,
        }
    }

    fn ready(&self) -> bool {
        self.handle.as_ref().is_some_and(JoinHandle::is_finished)
    }

    fn drain(&self, app: &mut App) {
        for event in self.events.try_iter() {
            match event {
                OutputEvent::Message { text, error } => {
                    app.record(format!("{}{}", if error { "Warning: " } else { "" }, text))
                }
                OutputEvent::Download { received, total } => {
                    app.download_progress = Some((received, total));
                    app.status = match total {
                        Some(total) => format!("Downloading: {received}/{total} bytes"),
                        None => format!("Downloading: {received} bytes (total unknown)"),
                    };
                }
            }
        }
    }
}

// An I/O failure cannot detach a worker still mutating a root.
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct TerminalGuard {
    mouse_enabled: bool,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.mouse_enabled {
            let _ = execute!(std::io::stdout(), DisableMouseCapture);
        }
        ratatui::restore();
    }
}

fn spawn_command(app: &mut App, root: &Path, command: Command) -> Worker {
    app.active_op = Some(command.clone());
    app.busy = true;
    app.download_progress = None;
    app.status = "Working; cancellation unavailable. [l] Transcript".into();
    Worker::start(root.to_path_buf(), command)
}

fn handle_finished(
    app: &mut App,
    root: &Path,
    finished_op: Option<Command>,
    result: Result<String>,
) {
    match result {
        Ok(summary) => {
            app.record(summary.clone());
            let first_line = summary.lines().next().unwrap_or("Completed").to_string();
            match finished_op {
                Some(Command::Scan) => {
                    app.doctor_report = Some(summary.clone());
                    app.record_recent(vec!["doctor".into(), "--scan".into()], first_line, true);
                    app.text("Operation result", summary);
                }
                Some(Command::ExecuteCli(argv)) => {
                    if argv.first().map(String::as_str) == Some("doctor")
                        && argv.iter().any(|a| a == "--scan")
                    {
                        app.doctor_report = Some(summary.clone());
                    }
                    let is_read_only = catalog::catalog()
                        .iter()
                        .filter(|spec| argv.starts_with(&spec.path))
                        .max_by_key(|spec| spec.path.len())
                        .is_some_and(|spec| {
                            let rest = argv.get(spec.path.len()..).unwrap_or(&[]);
                            let fields = catalog::fields_from_argv(spec, rest);
                            catalog::execution_mode(&spec.display, &fields)
                                == catalog::ExecutionMode::ReadOnly
                        });
                    app.record_recent(argv, first_line, is_read_only);
                    app.text("Operation result", summary);
                }
                Some(Command::Execute(review)) if review.action == operations::Action::Install => {
                    let follow_up = operations::load(root).ok().and_then(|fresh| {
                        let entry = fresh.installed.packages.get(&review.id)?;
                        if !operations::is_entry_activatable(entry) {
                            return None;
                        }
                        let nu = fresh.nu_valid.then_some(fresh.nu.as_ref()).flatten();
                        if operations::activation(entry, nu) == "active" {
                            return None;
                        }
                        Some(FollowUp {
                            key: 'a',
                            label: "Review Activate [a]".into(),
                            command: Box::new(Command::Prepare(
                                operations::Action::Activate,
                                review.id.clone(),
                                review.registry.clone(),
                                None,
                            )),
                        })
                    });
                    if let Some(fu) = follow_up {
                        app.text_with_follow_up(
                            "Operation result",
                            format!(
                                "{summary}\n\nNext step: review activation for {} (install is inert).",
                                review.id
                            ),
                            fu,
                        );
                    } else {
                        app.text("Operation result", summary);
                    }
                }
                _ => app.text("Operation result", summary),
            }
        }
        Err(error) => {
            let summary = format!(
                "Operation failed: {error:#}\nInspect state and retained journals before retrying."
            );
            app.record(summary.clone());
            app.text("Operation result", summary);
        }
    }
}

pub fn run(root: &Path) -> Result<Option<String>> {
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let mut guard = TerminalGuard {
        mouse_enabled: false,
    };
    let mut terminal = ratatui::try_init()?;
    if execute!(std::io::stdout(), EnableMouseCapture).is_ok() {
        guard.mouse_enabled = true;
    }
    let mut app = App::new(&root);
    let mut worker = Some(Worker::start(root.clone(), Command::Reload));
    app.active_op = Some(Command::Reload);
    app.busy = true;
    let mut last_spin = Instant::now();
    loop {
        if let Some(active) = &worker {
            active.drain(&mut app);
        }
        if app.busy && last_spin.elapsed() >= Duration::from_millis(90) {
            app.spinner_tick = app.spinner_tick.wrapping_add(1);
            last_spin = Instant::now();
        }
        if worker.as_ref().is_some_and(|w| w.ready()) {
            let active = worker.take().expect("worker was present");
            let finished_op = app.active_op.take();
            // Take the handle while keeping the error-path worker guard valid.
            let completion = finish(active, &mut app)?;
            app.busy = false;
            app.download_progress = None;
            match completion {
                Completion::Loaded(result) => match result {
                    Ok(data) => {
                        app.status = if data.warnings.is_empty() {
                            "Local state loaded. No network requests.".into()
                        } else {
                            data.warnings.join(" | ")
                        };
                        for warning in &data.warnings {
                            app.record(warning.clone());
                        }
                        app.data = Some(*data);
                        app.selected = app.selected.min(app.rows().len().saturating_sub(1));
                    }
                    Err(error) => {
                        app.data = None;
                        app.status = format!("Cannot load local state: {error:#}");
                        app.record(app.status.clone());
                    }
                },
                Completion::Prepared(result) => match result {
                    Ok(review) => {
                        app.overlay = Some(Overlay::Review {
                            review,
                            confirm: false,
                            scroll: 0,
                        })
                    }
                    Err(error) => app.text("Review blocked", format!("{error:#}")),
                },
                Completion::Finished(result) => {
                    handle_finished(&mut app, &root, finished_op, result);
                    worker = Some(Worker::start(root.clone(), Command::Reload));
                    app.active_op = Some(Command::Reload);
                    app.busy = true;
                }
            }
        }
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        if app.exit && worker.is_none() {
            break;
        }
        if event::poll(Duration::from_millis(50))? && !app.exit {
            let cmd = match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => app.key(key),
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                _ => None,
            };
            if let Some(command) = cmd {
                if worker.is_none() {
                    worker = Some(spawn_command(&mut app, &root, command));
                }
            }
        }
    }
    // TerminalGuard disables mouse capture and restores raw mode/alternate screen before printing.
    Ok(app.exit_command)
}

fn finish(mut worker: Worker, app: &mut App) -> Result<Completion> {
    // Worker::drop protects unfinished operations. At this point it is already finished.
    let result = worker
        .handle
        .take()
        .expect("worker handle exists")
        .join()
        .map_err(|_| anyhow::anyhow!("TUI worker panicked; inspect root recovery state"))
        .context("Worker failed");
    worker.drain(app);
    result
}
