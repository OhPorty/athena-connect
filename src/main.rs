//! athena-connect: gives an Athena server access to one folder on this
//! computer, and nothing else.

use athena_connect::sandbox;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // The command helper: lock down, then become the shell. Never returns on success.
    if args.get(1).map(String::as_str) == Some("__run") {
        std::process::exit(run_helper(&args[2..]));
    }
}

/// `athena-connect __run <fence json> <command>`: apply the sandbox, then exec the shell.
fn run_helper(args: &[String]) -> i32 {
    let (Some(fence), Some(command)) = (args.first(), args.get(1)) else {
        eprintln!("athena-connect __run: missing arguments");
        return 2;
    };
    let fence: sandbox::Fence = match serde_json::from_str(fence) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("athena-connect __run: bad fence: {e}");
            return 2;
        }
    };
    if let Err(e) = sandbox::restrict_self(&fence) {
        // Never run a command we couldn't confine.
        eprintln!("athena-connect: refusing to run the command, because it couldn't be confined to the folder: {e}");
        return 126;
    }
    exec_shell(command)
}

#[cfg(unix)]
fn exec_shell(command: &str) -> i32 {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new("/bin/sh").arg("-c").arg(command).exec();
    eprintln!("athena-connect: couldn't start the shell: {err}");
    127
}

#[cfg(not(unix))]
fn exec_shell(_command: &str) -> i32 {
    eprintln!("athena-connect: commands aren't supported on this system yet");
    126
}
