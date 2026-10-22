//! Optional, explicit terminal workbench. No network or mutation on view entry.
mod app;
pub mod catalog;
pub mod operations;
#[cfg(test)]
mod tests;
pub mod theme;
mod ui;

use crate::util::output::{self, OutputEvent};
use anyhow::{Context, Result};
use app::{App, Command, Overlay};
use crossterm::event::{self, Event, KeyEventKind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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
                    app.status = match total {
                        Some(total) => format!("Downloading: {received}/{total} bytes"),
                        None => format!("Downloading: {received} bytes (total unknown)"),
                    }
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

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

pub fn run(root: &Path) -> Result<Option<String>> {
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let _guard = TerminalGuard;
    let mut terminal = ratatui::try_init()?;
    let mut app = App::new(&root);
    let mut worker = Some(Worker::start(root.clone(), Command::Reload));
    app.busy = true;
    loop {
        if let Some(active) = &worker {
            active.drain(&mut app);
        }
        if worker.as_ref().is_some_and(|w| w.ready()) {
            let active = worker.take().expect("worker was present");
            // Take the handle while keeping the error-path worker guard valid.
            let completion = finish(active, &mut app)?;
            app.busy = false;
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
                    let summary = match result { Ok(summary) => summary, Err(error) => format!("Operation failed: {error:#}\nInspect state and retained journals before retrying.") };
                    app.record(summary.clone());
                    app.text("Operation result", summary);
                    app.busy = true;
                    worker = Some(Worker::start(root.clone(), Command::Reload));
                }
            }
        }
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        if app.exit && worker.is_none() {
            break;
        }
        if event::poll(Duration::from_millis(50))? && !app.exit {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Release {
                    if let Some(command) = app.key(key) {
                        if worker.is_none() {
                            app.busy = true;
                            app.status = "Working; cancellation unavailable. [l] Transcript".into();
                            worker = Some(Worker::start(root.clone(), command));
                        }
                    }
                }
            }
        }
    }
    // TerminalGuard restores raw mode/alternate screen before cmd::tui prints this value.
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
