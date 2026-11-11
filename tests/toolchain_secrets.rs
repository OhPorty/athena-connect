// Toolchains are readable to commands so they can run, but not the secrets kept beside them.
// (Its own test program, because it changes HOME for the whole process.)
#![cfg(target_os = "linux")]

use athena_connect::sandbox::*;
use std::process::Command;

#[test]
fn commands_can_run_cargo_but_not_read_its_tokens() {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("home");
    let folder = base.path().join("project");
    let temp = base.path().join("tmp");
    for d in [home.join(".cargo/bin"), folder.clone(), temp.clone()] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(home.join(".cargo/credentials.toml"), "[registry]\ntoken = \"cio-secret\"\n").unwrap();
    std::fs::write(home.join(".cargo/bin/tool"), "#!/bin/sh\necho tool ran\n").unwrap();
    Command::new("chmod").arg("755").arg(home.join(".cargo/bin/tool")).status().unwrap();
    std::env::set_var("HOME", &home);

    let fence = Fence::for_folder(&folder, &temp);
    let run = |cmd: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_athena-connect")).arg("__run").arg(serde_json::to_string(&fence).unwrap()).arg(cmd).current_dir(&folder).output().unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    };
    let (ok, out) = run(&format!("{}/.cargo/bin/tool", home.display()));
    assert!(ok && out.contains("tool ran"), "{out}");
    let (ok, out) = run(&format!("cat {}/.cargo/credentials.toml", home.display()));
    assert!(!ok && !out.contains("cio-secret"), "{out}");
}
