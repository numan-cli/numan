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
    let text = redact_uuids(&args.to_string());
    SINK.with(|slot| {
        if let Some(sink) = slot.borrow().as_ref() {
            sink(OutputEvent::Message { text, error });
        } else if error {
            std::eprintln!("{text}");
        } else {
            std::println!("{text}");
        }
    });
}

fn redact_uuids(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut start = 0;
    while start + 36 <= bytes.len() {
        let uuid = (0..36).all(|offset| match offset {
            8 | 13 | 18 | 23 => bytes[start + offset] == b'-',
            _ => bytes[start + offset].is_ascii_hexdigit(),
        });
        let boundary_before = start == 0 || !bytes[start - 1].is_ascii_hexdigit();
        let boundary_after = start + 36 == bytes.len() || !bytes[start + 36].is_ascii_hexdigit();
        if uuid && boundary_before && boundary_after {
            result.push_str(&text[cursor..start]);
            result.push_str("[redacted UUID]");
            cursor = start + 36;
            start = cursor;
        } else {
            start += 1;
        }
    }
    result.push_str(&text[cursor..]);
    result
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
    fn redacts_uuid_values_from_output() {
        assert_eq!(
            redact_uuids("Created snapshot 018f1f1f-1234-7abc-8def-1234567890ab."),
            "Created snapshot [redacted UUID]."
        );
        assert_eq!(redact_uuids("No identifier"), "No identifier");
    }

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
