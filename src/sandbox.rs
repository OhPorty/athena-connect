//! Running Athena's commands so they can't reach outside the folder.
//!
//! A command could name any path (`rm -rf ~`, `cat ~/.ssh/id_ed25519`), so
//! checking its text isn't enough: the operating system has to enforce it.
//! The connector starts every command through itself (`athena-connect
//! __run`), and that helper locks itself down before starting the shell, so
//! the lock covers the command and everything it starts.
//!
//! Linux: Landlock (kernel 6.2+). The command may read and write the folder
//! and a private temporary folder; it may read (not change) the system's
//! program folders and toolchains like Rust's; everything else, your home
//! folder included, doesn't exist as far as it can tell.
//! Where the lock isn't available, commands are switched off rather than run
//! unconfined.

use std::path::{Path, PathBuf};

/// What a confined command may see.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fence {
    /// Read and write.
    pub read_write: Vec<PathBuf>,
    /// Read only (and run programs from).
    pub read_only: Vec<PathBuf>,
}

/// System folders programs need, read only.
const SYSTEM_READ_ONLY: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/snap", "/sys", "/proc/self", "/proc/thread-self", "/proc/cpuinfo", "/proc/meminfo", "/proc/stat", "/proc/loadavg", "/proc/version", "/proc/sys"];

impl Fence {
    /// The fence for a folder: the folder and a private temp folder writable; system and toolchains readable.
    pub fn for_folder(folder: &Path, temp: &Path) -> Fence {
        let mut read_only: Vec<PathBuf> = SYSTEM_READ_ONLY.iter().map(PathBuf::from).filter(|p| p.exists()).collect();
        read_only.extend(toolchains());
        Fence { read_write: vec![folder.to_path_buf(), temp.to_path_buf(), PathBuf::from("/dev")], read_only }
    }
}

/// Toolchains installed for this user, readable (not writable) by commands.
pub fn toolchains() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return vec![] };
    [".cargo", ".rustup", ".local/bin", ".local/lib", ".nvm", ".bun", ".deno", ".pyenv", "go/bin", ".dotnet"]
        .iter()
        .map(|d| home.join(d))
        .filter(|p| p.exists())
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Support {
    Available,
    /// Commands can't be confined here, so they're off. The reason, in plain words.
    Unavailable(String),
}

/// Whether commands can be confined on this computer.
pub fn support() -> Support {
    imp::support()
}

/// Lock this process down to the fence (the helper calls this before starting the shell).
pub fn restrict_self(fence: &Fence) -> Result<(), String> {
    imp::restrict_self(fence)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Fence, Support};
    use landlock::{path_beneath_rules, Access, AccessFs, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, ABI};

    /// ABI 3 (Linux 6.2) is the first that also stops truncating files outside; earlier ones leave a hole.
    const NEEDED: ABI = ABI::V3;

    pub fn support() -> Support {
        // Probe in a child thread-free way: create (but don't apply) a ruleset that requires ABI 3.
        match Ruleset::default().set_compatibility(landlock::CompatLevel::HardRequirement).handle_access(AccessFs::from_all(NEEDED)) {
            Ok(r) => match r.create() {
                Ok(_) => Support::Available,
                Err(e) => Support::Unavailable(format!("this kernel's Landlock can't confine commands ({e}); it needs Linux 6.2 or newer with Landlock enabled")),
            },
            Err(e) => Support::Unavailable(format!("this kernel's Landlock is too old to confine commands ({e}); it needs Linux 6.2 or newer")),
        }
    }

    pub fn restrict_self(fence: &Fence) -> Result<(), String> {
        let status = Ruleset::default()
            .set_compatibility(landlock::CompatLevel::HardRequirement)
            .handle_access(AccessFs::from_all(NEEDED))
            .and_then(|r| r.create())
            .and_then(|r| r.add_rules(path_beneath_rules(&fence.read_only, AccessFs::from_read(NEEDED))))
            .and_then(|r| r.add_rules(path_beneath_rules(&fence.read_write, AccessFs::from_all(NEEDED))))
            .and_then(|r| r.restrict_self())
            .map_err(|e| e.to_string())?;
        match status.ruleset {
            RulesetStatus::FullyEnforced => Ok(()),
            other => Err(format!("the sandbox wasn't fully applied ({other:?})")),
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{Fence, Support};
    pub fn support() -> Support {
        Support::Unavailable("confining commands isn't built for this system yet".into())
    }
    pub fn restrict_self(_fence: &Fence) -> Result<(), String> {
        Err("confining commands isn't built for this system yet".into())
    }
}

