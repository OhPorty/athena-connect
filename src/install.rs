//! Installing a toolchain a command needs, after the person said yes in Athena.
//!
//! This is the one thing the connector does outside the folder, because a toolchain lives in your user account,
//! not in a project. So it only runs these fixed recipes, from each toolchain's official source; what to run is
//! never taken from Athena.

use std::path::Path;
use std::process::Command;

/// The toolchains this computer can install, by id ("rust", "node", "python", "git").
pub fn installable() -> Vec<&'static str> {
    let mut out = Vec::new();
    if cfg!(windows) || cfg!(target_os = "linux") {
        out.push("rust");
    }
    if cfg!(windows) && has_winget() {
        out.extend(["node", "python", "git"]);
    }
    out
}

fn has_winget() -> bool {
    Command::new("winget").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Install a toolchain. Returns a short description of what was installed, or why it failed.
pub fn install(id: &str, temp: &Path) -> Result<String, String> {
    println!("\nInstalling {id} (you said yes in Athena)…");
    let result = match id {
        "rust" => rust(temp),
        "node" => winget("OpenJS.NodeJS.LTS", false),
        "python" => winget("Python.Python.3.12", true),
        "git" => winget("Git.Git", false),
        other => Err(format!("this connector doesn't install {other}")),
    };
    #[cfg(windows)]
    refresh_path();
    match &result {
        Ok(what) => println!("Installed {what}.\n"),
        Err(e) => println!("Installing {id} failed: {e}\n"),
    }
    result
}

/// Rust through rustup, for this user only (no administrator rights needed).
fn rust(temp: &Path) -> Result<String, String> {
    if cfg!(windows) {
        let exe = temp.join("rustup-init.exe");
        download("https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe", &exe)?;
        // The GNU toolchain links with its own bundled tools, so it doesn't need Microsoft's C++ Build Tools.
        run(Command::new(&exe).args(["-y", "--profile", "minimal", "--default-host", "x86_64-pc-windows-gnu"]))?;
    } else {
        let script = temp.join("rustup-init.sh");
        download("https://sh.rustup.rs", &script)?;
        run(Command::new("sh").arg(&script).args(["-y", "--profile", "minimal"]))?;
    }
    let cargo = dirs::home_dir().map(|h| h.join(".cargo").join("bin")).unwrap_or_default();
    let version = Command::new(cargo.join(if cfg!(windows) { "cargo.exe" } else { "cargo" })).arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    Ok(if version.is_empty() { "Rust".into() } else { version })
}

fn winget(package: &str, user_scope: bool) -> Result<String, String> {
    let mut cmd = Command::new("winget");
    cmd.args(["install", "--id", package, "-e", "--silent", "--accept-package-agreements", "--accept-source-agreements", "--disable-interactivity"]);
    if user_scope {
        cmd.args(["--scope", "user"]);
    }
    run(&mut cmd)?;
    Ok(package.to_string())
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let res = ureq::get(url).call().map_err(|e| format!("couldn't download {url}: {e}"))?;
    let mut body = res.into_body();
    let bytes = body.with_config().limit(200 * 1024 * 1024).read_to_vec().map_err(|e| format!("couldn't download {url}: {e}"))?;
    std::fs::write(to, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}

/// Run an installer, showing its output in this terminal.
fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| format!("couldn't start the installer: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the installer exited with {}", status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into())))
    }
}

/// Installers add their folders to your PATH in the registry; pick those up so commands find them now.
#[cfg(windows)]
fn refresh_path() {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", "[Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User')"])
        .output();
    if let Ok(o) = out {
        let fresh = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if !fresh.is_empty() {
            let mut all: Vec<std::path::PathBuf> = std::env::split_paths(&fresh).collect();
            for p in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
                if !all.contains(&p) {
                    all.push(p);
                }
            }
            if let Ok(joined) = std::env::join_paths(all) {
                std::env::set_var("PATH", joined);
            }
        }
    }
}
