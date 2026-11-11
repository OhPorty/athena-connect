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
//! Windows: an AppContainer (see sandbox_windows.rs), with the same rules.
//!
//! Where commands can't be locked in, they're switched off rather than run
//! unconfined. And before turning them on, the connector proves the lock on
//! this computer: a test command must run, and must fail to read a file
//! outside the folder (see ops.rs).

use std::path::{Path, PathBuf};

/// What a confined command may see.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fence {
    /// Read and write. The first is the folder itself.
    pub read_write: Vec<PathBuf>,
    /// Read only (and run programs from).
    pub read_only: Vec<PathBuf>,
    /// Windows: the folder's own AppContainer.
    #[serde(default)]
    pub container: Option<String>,
    /// The shell commands run in ("bash", "sh", "powershell" or "cmd").
    #[serde(default)]
    pub shell: Option<String>,
    /// Windows: a drive letter standing for the folder during the session (e.g. "Z:"), where commands start.
    /// PowerShell checks the folders above where it starts, which the sandbox can't see; a drive's root has none.
    #[serde(default)]
    pub drive: Option<String>,
}

/// System folders programs need, read only.
#[cfg(not(windows))]
const SYSTEM_READ_ONLY: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/snap", "/sys", "/proc/self", "/proc/thread-self", "/proc/cpuinfo", "/proc/meminfo", "/proc/stat", "/proc/loadavg", "/proc/version", "/proc/sys"];

impl Fence {
    /// The fence for a folder: the folder and a private temp folder writable; system and toolchains readable.
    #[cfg(not(windows))]
    pub fn for_folder(folder: &Path, temp: &Path) -> Fence {
        let mut read_only: Vec<PathBuf> = SYSTEM_READ_ONLY.iter().map(PathBuf::from).filter(|p| p.exists()).collect();
        read_only.extend(toolchains());
        let shell = if Path::new("/bin/bash").exists() { "bash" } else { "sh" };
        Fence { read_write: vec![folder.to_path_buf(), temp.to_path_buf(), PathBuf::from("/dev")], read_only, container: None, shell: Some(shell.into()), drive: None }
    }

    /// The fence for a folder: the folder and a private temp folder writable; toolchains readable
    /// (Windows' own folders are readable to every container already).
    #[cfg(windows)]
    pub fn for_folder(folder: &Path, temp: &Path) -> Fence {
        Fence { read_write: vec![folder.to_path_buf(), temp.to_path_buf()], read_only: toolchains(), container: Some(imp::container_name(folder)), shell: Some("powershell".into()), drive: None }
    }
}

/// Toolchains installed for this user, readable (not writable) by commands.
/// Only program folders: not ~/.cargo itself, which holds crates.io tokens (commands get their own CARGO_HOME).
#[cfg(not(windows))]
pub fn toolchains() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return vec![] };
    [".cargo/bin", ".rustup", ".local/bin", ".local/lib", ".nvm", ".bun", ".deno", ".pyenv", "go/bin", ".dotnet"]
        .iter()
        .map(|d| home.join(d))
        .filter(|p| p.exists())
        .collect()
}

#[cfg(windows)]
pub fn toolchains() -> Vec<PathBuf> {
    imp::toolchains()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Support {
    Available,
    /// Commands can't be confined here, so they're off. The reason, in plain words.
    Unavailable(String),
}

/// Whether commands can be confined on this computer (before trying it: see ops.rs).
pub fn support() -> Support {
    imp::support()
}

/// Set up before the first command (Windows: the container and its access). Nothing to do on Linux.
pub fn prepare(fence: &mut Fence) -> Result<(), String> {
    imp::prepare(fence)
}

/// Let commands read toolchains added since `prepare` (Windows opens them to the sandbox; Linux reads the fence).
pub fn open_toolchains(fence: &Fence) {
    imp::open_toolchains(fence)
}

/// Undo what `prepare` set up for the session.
pub fn release(fence: &Fence) {
    imp::release(fence)
}

/// Run a command confined to the fence (the `__run` helper does this). Returns its exit code;
/// if it can't be confined, it doesn't run.
pub fn run(fence: &Fence, command: &str) -> i32 {
    imp::run(fence, command)
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

    pub fn prepare(_fence: &mut Fence) -> Result<(), String> {
        Ok(())
    }

    pub fn release(_fence: &Fence) {}

    pub fn open_toolchains(_fence: &Fence) {}

    pub fn run(fence: &Fence, command: &str) -> i32 {
        use std::os::unix::process::CommandExt;
        if let Err(e) = restrict_self(fence) {
            // Never run a command we couldn't confine.
            eprintln!("athena-connect: refusing to run the command, because it couldn't be confined to the folder: {e}");
            return 126;
        }
        let shell = if fence.shell.as_deref() == Some("sh") { "/bin/sh" } else { "/bin/bash" };
        let err = std::process::Command::new(shell).arg("-c").arg(command).exec();
        eprintln!("athena-connect: couldn't start the shell: {err}");
        127
    }

    fn restrict_self(fence: &Fence) -> Result<(), String> {
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

#[cfg(windows)]
#[path = "sandbox_windows.rs"]
mod imp;

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::{Fence, Support};
    const NOT_YET: &str = "confining commands isn't built for this system yet";
    pub fn support() -> Support {
        Support::Unavailable(NOT_YET.into())
    }
    pub fn prepare(_fence: &mut Fence) -> Result<(), String> {
        Err(NOT_YET.into())
    }
    pub fn release(_fence: &Fence) {}
    pub fn open_toolchains(_fence: &Fence) {}
    pub fn run(_fence: &Fence, _command: &str) -> i32 {
        eprintln!("athena-connect: {NOT_YET}");
        126
    }
}
