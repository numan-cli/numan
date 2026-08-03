//! Temporarily redirect stdout to stderr so JSON emitters stay parseable.

use std::io::{self, Write};
use std::marker::PhantomData;

/// RAII guard that redirects process stdout to stderr for nested CLI chatter.
///
/// This type is intentionally `!Send + !Sync`: it captures and restores
/// process-global stdio state via `dup`/`dup2` (Unix) or `SetStdHandle`
/// (Windows), which is not safe to use from multiple threads concurrently.
///
/// The redirection is process-global: while the guard is alive, stdout writes
/// from every thread in the process follow the redirected descriptor/handle.
pub struct StdoutToStderr {
    #[cfg(unix)]
    saved_fd: i32,
    #[cfg(windows)]
    saved_handle: *mut core::ffi::c_void,
    /// Makes the type `!Send + !Sync` on every platform. `*const ()` does not
    /// implement those auto-traits; needed on Unix where the saved fd alone
    /// would otherwise be `Send + Sync`.
    _not_send_sync: PhantomData<*const ()>,
}

#[cfg(windows)]
mod win_stdio {
    pub const STD_OUTPUT_HANDLE: u32 = 0xFFFFFFF5;
    pub const STD_ERROR_HANDLE: u32 = 0xFFFFFFF4;
    pub const INVALID_HANDLE_VALUE: isize = -1;

    #[allow(non_snake_case)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetStdHandle(n_std_handle: u32) -> *mut core::ffi::c_void;
        pub fn SetStdHandle(n_std_handle: u32, h_handle: *mut core::ffi::c_void) -> i32;
    }
}

impl StdoutToStderr {
    /// Redirect process stdout to stderr until this guard is dropped.
    pub fn redirect() -> io::Result<Self> {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();

        #[cfg(unix)]
        {
            // SAFETY: dup/dup2 operate on the standard stdio descriptors.
            unsafe {
                let saved_fd = libc::dup(libc::STDOUT_FILENO);
                if saved_fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
                    let err = io::Error::last_os_error();
                    let _ = libc::close(saved_fd);
                    return Err(err);
                }
                Ok(Self {
                    saved_fd,
                    _not_send_sync: PhantomData,
                })
            }
        }

        #[cfg(windows)]
        {
            use win_stdio::*;

            // SAFETY: Win32 stdio handle swap for the current process only.
            // Save the existing GetStdHandle value and point STD_OUTPUT_HANDLE at
            // stderr. SetStdHandle does not close the previous handle, so the
            // original remains valid for restore without DuplicateHandle.
            unsafe {
                let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
                let stderr = GetStdHandle(STD_ERROR_HANDLE);
                if stdout as isize == INVALID_HANDLE_VALUE
                    || stdout.is_null()
                    || stderr as isize == INVALID_HANDLE_VALUE
                    || stderr.is_null()
                {
                    return Err(io::Error::last_os_error());
                }

                if SetStdHandle(STD_OUTPUT_HANDLE, stderr) == 0 {
                    return Err(io::Error::last_os_error());
                }

                Ok(Self {
                    saved_handle: stdout,
                    _not_send_sync: PhantomData,
                })
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                _not_send_sync: PhantomData,
            })
        }
    }
}

impl Drop for StdoutToStderr {
    fn drop(&mut self) {
        let _ = io::stdout().flush();

        #[cfg(unix)]
        // SAFETY: restore the saved stdout descriptor captured in `redirect`.
        unsafe {
            let _ = libc::dup2(self.saved_fd, libc::STDOUT_FILENO);
            let _ = libc::close(self.saved_fd);
        }

        #[cfg(windows)]
        {
            use win_stdio::{SetStdHandle, STD_OUTPUT_HANDLE};

            // SAFETY: restore the original stdout handle captured in `redirect`.
            // Do not CloseHandle it: it is the process's prior standard handle.
            unsafe {
                let _ = SetStdHandle(STD_OUTPUT_HANDLE, self.saved_handle);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::StdoutToStderr;

    /// Exercise process-global FD remounts in a forked child so parallel lib
    /// tests cannot observe or race on the parent's stdout/stderr descriptors.
    #[test]
    fn redirect_sends_stdout_writes_to_stderr_pipe_then_restores() {
        use std::io::Write;

        // Own both stdio locks on the forking thread. A fork copies only this
        // thread, so an inherited lock held by another thread would deadlock
        // the child inside `redirect()` (which flushes stdout/stderr).
        let mut out_lock = std::io::stdout().lock();
        let mut err_lock = std::io::stderr().lock();
        let _ = out_lock.flush();
        let _ = err_lock.flush();

        // SAFETY: fork isolates FD mutations; child calls `_exit` and never returns.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());
            if pid == 0 {
                let code = match run_redirect_pipe_assertions() {
                    Ok(()) => 0,
                    Err(_) => 1,
                };
                libc::_exit(code);
            }

            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            assert!(
                libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
                "child redirect assertions failed (status={status})"
            );
        }
    }

    fn run_redirect_pipe_assertions() -> Result<(), ()> {
        // SAFETY: runs only in the forked child from the test above.
        unsafe {
            let mut stdout_pipe = [0; 2];
            let mut stderr_pipe = [0; 2];
            if libc::pipe(stdout_pipe.as_mut_ptr()) != 0
                || libc::pipe(stderr_pipe.as_mut_ptr()) != 0
            {
                return Err(());
            }

            if libc::dup2(stdout_pipe[1], libc::STDOUT_FILENO) < 0
                || libc::dup2(stderr_pipe[1], libc::STDERR_FILENO) < 0
            {
                return Err(());
            }
            let _ = libc::close(stdout_pipe[1]);
            let _ = libc::close(stderr_pipe[1]);

            {
                let guard = StdoutToStderr::redirect().map_err(|_| ())?;
                let msg = b"during-redirect\n";
                if libc::write(libc::STDOUT_FILENO, msg.as_ptr().cast(), msg.len())
                    != msg.len() as isize
                {
                    return Err(());
                }
                drop(guard);
            }

            let restore_msg = b"after-restore\n";
            if libc::write(
                libc::STDOUT_FILENO,
                restore_msg.as_ptr().cast(),
                restore_msg.len(),
            ) != restore_msg.len() as isize
            {
                return Err(());
            }

            let mut err_buf = [0u8; 64];
            let err_n = libc::read(stderr_pipe[0], err_buf.as_mut_ptr().cast(), err_buf.len());
            let mut out_buf = [0u8; 64];
            let out_n = libc::read(stdout_pipe[0], out_buf.as_mut_ptr().cast(), out_buf.len());
            let _ = libc::close(stdout_pipe[0]);
            let _ = libc::close(stderr_pipe[0]);

            if err_n <= 0 || &err_buf[..err_n as usize] != b"during-redirect\n" {
                return Err(());
            }
            if out_n <= 0 || &out_buf[..out_n as usize] != b"after-restore\n" {
                return Err(());
            }
            Ok(())
        }
    }
}
