use serde::{Deserialize, Serialize};
use std::env::consts::{ARCH, OS};
use std::fmt;
use std::path::PathBuf;

#[cfg(target_env = "gnu")]
const LIBC: &str = "gnu";

#[cfg(target_env = "musl")]
const LIBC: &str = "musl";

#[cfg(not(any(target_env = "gnu", target_env = "musl")))]
const LIBC: &str = "";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Os {
    Windows,
    Macos,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Env {
    Gnu,
    Musl,
    Msvc,
    Darwin,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
    pub env: Env,
    pub triple: String,
}

impl Platform {
    pub fn detect() -> Self {
        let os = match OS {
            "windows" => Os::Windows,
            "macos" => Os::Macos,
            "linux" => Os::Linux,
            _ => panic!("Unsupported OS: {OS}"),
        };

        let arch = match ARCH {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            _ => panic!("Unsupported architecture: {ARCH}"),
        };

        let env = match os {
            Os::Windows => Env::Msvc,
            Os::Macos => Env::Darwin,
            Os::Linux => match LIBC {
                "musl" => Env::Musl,
                _ => Env::Gnu,
            },
        };

        let triple = Self::build_triple(os, arch, env);

        Self {
            os,
            arch,
            env,
            triple,
        }
    }

    fn build_triple(os: Os, arch: Arch, env: Env) -> String {
        let arch_str = match arch {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        };

        match os {
            Os::Windows => format!("{arch_str}-pc-windows-msvc"),
            Os::Macos => format!("{arch_str}-apple-darwin"),
            Os::Linux => {
                let env_str = match env {
                    Env::Gnu => "gnu",
                    Env::Musl => "musl",
                    _ => unreachable!(),
                };
                format!("{arch_str}-unknown-linux-{env_str}")
            }
        }
    }

    pub fn default_root(&self) -> PathBuf {
        match self.os {
            Os::Windows => {
                let local_app_data = std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA not set");
                PathBuf::from(local_app_data).join("numan")
            }
            Os::Macos => dirs::home_dir()
                .expect("Home directory not found")
                .join("Library/Application Support/numan"),
            Os::Linux => dirs::home_dir()
                .expect("Home directory not found")
                .join(".local/share/numan"),
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.triple)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_current_platform() {
        let platform = Platform::detect();
        assert!(!platform.triple.is_empty());
    }

    #[test]
    fn windows_triple() {
        assert_eq!(
            Platform::build_triple(Os::Windows, Arch::X86_64, Env::Msvc),
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            Platform::build_triple(Os::Windows, Arch::Aarch64, Env::Msvc),
            "aarch64-pc-windows-msvc"
        );
    }

    #[test]
    fn macos_triple() {
        assert_eq!(
            Platform::build_triple(Os::Macos, Arch::X86_64, Env::Darwin),
            "x86_64-apple-darwin"
        );
        assert_eq!(
            Platform::build_triple(Os::Macos, Arch::Aarch64, Env::Darwin),
            "aarch64-apple-darwin"
        );
    }

    #[test]
    fn linux_triples() {
        assert_eq!(
            Platform::build_triple(Os::Linux, Arch::X86_64, Env::Gnu),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            Platform::build_triple(Os::Linux, Arch::X86_64, Env::Musl),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            Platform::build_triple(Os::Linux, Arch::Aarch64, Env::Gnu),
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            Platform::build_triple(Os::Linux, Arch::Aarch64, Env::Musl),
            "aarch64-unknown-linux-musl"
        );
    }
}
