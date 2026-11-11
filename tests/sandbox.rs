// Commands run through the connector must not reach anything outside the folder.
#![cfg(target_os = "linux")]

use athena_connect::sandbox::*;
use std::path::Path;
use std::process::Command;

/// Run a shell command the way the connector does: through the helper, locked to `folder`.
fn confined(folder: &Path, temp: &Path, cmd: &str) -> (bool, String) {
    let fence = Fence::for_folder(folder, temp);
    let out = Command::new(env!("CARGO_BIN_EXE_athena-connect"))
        .arg("__run")
        .arg(serde_json::to_string(&fence).unwrap())
        .arg(cmd)
        .current_dir(folder)
        .env("TMPDIR", temp)
        .output()
        .unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn commands_work_inside_and_cannot_touch_anything_outside() {
    assert_eq!(support(), Support::Available, "these tests need Landlock (Linux 6.2+)");
    let base = tempfile::tempdir().unwrap();
    let folder = base.path().join("project");
    let temp = base.path().join("tmp");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(&temp).unwrap();
    let secret = base.path().join("secret.txt");
    std::fs::write(&secret, "outside").unwrap();
    let home = std::env::var("HOME").unwrap();

    // Inside: normal work.
    let (ok, out) = confined(&folder, &temp, "echo hello > note.txt && cat note.txt && mkdir -p a/b && ls");
    assert!(ok, "{out}");
    assert!(out.contains("hello"));
    let (ok, out) = confined(&folder, &temp, "echo scratch > \"$TMPDIR/t\" && cat \"$TMPDIR/t\"");
    assert!(ok && out.contains("scratch"), "{out}");

    // Outside: reading, writing, deleting, listing and truncating are all refused.
    let s = secret.display();
    for cmd in [
        format!("cat {s}"),
        format!("echo changed > {s}"),
        format!("rm -f {s}"),
        format!(": > {s}"),
        format!("truncate -s 0 {s}"),
        format!("ls {home}"),
        format!("cat {home}/.bashrc"),
        format!("touch {home}/athena-escaped"),
        "ls ..".to_string(),
        "cat ../secret.txt".to_string(),
    ] {
        let (ok, out) = confined(&folder, &temp, &cmd);
        assert!(!ok, "should have been refused: {cmd}\n{out}");
    }
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "outside");
    assert!(!Path::new(&format!("{home}/athena-escaped")).exists());

    // Reading the system to run programs still works.
    let (ok, out) = confined(&folder, &temp, "uname -s && which sh");
    assert!(ok, "{out}");
}
