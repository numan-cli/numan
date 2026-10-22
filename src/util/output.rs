//! Thread-scoped lifecycle output. Ordinary CLI callers retain stdout/stderr.
use std::cell::RefCell;
use std::fmt;

#[derive(Debug, Clone)]
pub enum OutputEvent {
    Message { text: String, error: bool },
    Download { received: u64, total: Option<u64> },
}

type Sink = Box<dyn Fn(OutputEvent)>;
thread_local! {
    static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) };
}

/// Restore the previous sink even when a lifecycle operation unwinds.
pub fn with_sink<T>(sink: impl Fn(OutputEvent) + 'static, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Sink>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SINK.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(SINK.with(|slot| slot.replace(Some(Box::new(sink)))));
    run()
}

pub fn is_captured() -> bool {
    SINK.with(|slot| slot.borrow().is_some())
}

pub fn download(received: u64, total: Option<u64>) {
    SINK.with(|slot| {
        if let Some(sink) = slot.borrow().as_ref() {
            sink(OutputEvent::Download { received, total });
        }
    });
}

pub(crate) fn line(args: fmt::Arguments<'_>, error: bool) {
    SINK.with(|slot| {
        if let Some(sink) = slot.borrow().as_ref() {
            sink(OutputEvent::Message {
                text: args.to_string(),
                error,
            });
        } else if error {
            std::eprintln!("{args}");
        } else {
            std::println!("{args}");
        }
    });
}

macro_rules! println {
    () => { $crate::util::output::line(format_args!(""), false) };
    ($($arg:tt)*) => { $crate::util::output::line(format_args!($($arg)*), false) };
}
macro_rules! eprintln {
    () => { $crate::util::output::line(format_args!(""), true) };
    ($($arg:tt)*) => { $crate::util::output::line(format_args!($($arg)*), true) };
}
pub(crate) use {eprintln, println};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn capture_is_thread_local_and_restores_after_unwind() {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let saved = messages.clone();
        let _ = std::panic::catch_unwind(|| {
            with_sink(
                move |event| saved.lock().unwrap().push(event),
                || {
                    assert!(is_captured());
                    std::thread::spawn(|| assert!(!is_captured()))
                        .join()
                        .unwrap();
                    line(format_args!("captured"), true);
                    download(7, Some(10));
                    panic!("test unwind");
                },
            );
        });
        assert!(!is_captured());
        let events = messages.lock().unwrap();
        assert!(
            matches!(&events[0], OutputEvent::Message { text, error: true } if text == "captured")
        );
        assert!(matches!(
            events[1],
            OutputEvent::Download {
                received: 7,
                total: Some(10)
            }
        ));
    }
}
