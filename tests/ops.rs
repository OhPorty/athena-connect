// What Athena asks the connector to do: every path stays inside the folder.
#![cfg(target_os = "linux")]

use athena_connect::ops::Folder;
use serde_json::json;
use std::path::PathBuf;

fn setup() -> (tempfile::TempDir, PathBuf, Folder) {
    let base = tempfile::tempdir().unwrap();
    let folder = base.path().join("project");
    std::fs::create_dir_all(folder.join("src")).unwrap();
    std::fs::write(folder.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(base.path().join("secret.txt"), "outside").unwrap();
    let mut f = Folder::new(&folder).unwrap();
    f.helper = PathBuf::from(env!("CARGO_BIN_EXE_athena-connect"));
    f.enable_commands();
    assert_eq!(f.commands, athena_connect::sandbox::Support::Available, "the sandbox should prove itself here");
    (base, folder, f)
}

#[test]
fn file_operations_stay_in_the_folder() {
    let (base, folder, f) = setup();
    let read = f.handle("read", &json!({ "path": "src/main.rs" })).unwrap();
    assert_eq!(read["text"], "fn main() {}\n");

    for path in ["../secret.txt", base.path().join("secret.txt").to_str().unwrap(), "/etc/passwd", "src/../../secret.txt"] {
        assert!(f.handle("read", &json!({ "path": path })).is_err(), "read {path}");
        assert!(f.handle("write", &json!({ "path": path, "text": "x" })).is_err(), "write {path}");
        assert!(f.handle("stat", &json!({ "path": path })).is_err(), "stat {path}");
        assert!(f.handle("glob", &json!({ "path": path, "pattern": "*" })).is_err(), "glob {path}");
        assert!(f.handle("grep", &json!({ "path": path, "pattern": "o" })).is_err(), "grep {path}");
    }
    assert_eq!(std::fs::read_to_string(base.path().join("secret.txt")).unwrap(), "outside");

    // A link inside the folder that points out of it is refused too.
    std::os::unix::fs::symlink(base.path().join("secret.txt"), folder.join("link.txt")).unwrap();
    assert!(f.handle("read", &json!({ "path": "link.txt" })).is_err());
    assert!(f.handle("write", &json!({ "path": "link.txt", "text": "x" })).is_err());
    assert_eq!(std::fs::read_to_string(base.path().join("secret.txt")).unwrap(), "outside");
    f.cleanup();
}

#[test]
fn writes_refuse_a_file_changed_since_it_was_read() {
    let (_base, folder, f) = setup();
    let read = f.handle("read", &json!({ "path": "src/main.rs" })).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(folder.join("src/main.rs"), "fn main() { changed() }\n").unwrap();
    assert!(f.handle("write", &json!({ "path": "src/main.rs", "text": "x", "expectMtime": read["mtime"] })).is_err());
    let w = f.handle("write", &json!({ "path": "new/dir/a.txt", "text": "hi" })).unwrap();
    assert_eq!(w["path"], "new/dir/a.txt");
    assert_eq!(std::fs::read_to_string(folder.join("new/dir/a.txt")).unwrap(), "hi");
    f.cleanup();
}

#[test]
fn glob_and_grep_find_files_inside() {
    let (_base, _folder, f) = setup();
    let g = f.handle("glob", &json!({ "pattern": "**/*.rs" })).unwrap();
    assert!(g.to_string().contains("src/main.rs"), "{g}");
    let r = f.handle("grep", &json!({ "pattern": "fn main" })).unwrap();
    assert!(r.to_string().contains("src/main.rs"), "{r}");
    f.cleanup();
}

#[test]
fn commands_run_confined() {
    let (base, folder, f) = setup();
    let ok = f.handle("exec", &json!({ "command": "echo hi > made.txt && cat made.txt" })).unwrap();
    assert_eq!(ok["code"], 0, "{ok}");
    assert!(folder.join("made.txt").exists());
    let out = f.handle("exec", &json!({ "command": format!("cat {0}/secret.txt; echo x > {0}/secret.txt", base.path().display()) })).unwrap();
    assert!(!out["output"].as_str().unwrap().contains("outside"), "{out}");
    assert_eq!(std::fs::read_to_string(base.path().join("secret.txt")).unwrap(), "outside");

    let started = f.handle("exec_start", &json!({ "command": "sleep 30" })).unwrap();
    let job = started["job"].as_str().unwrap().to_string();
    assert_eq!(f.handle("exec_output", &json!({ "job": job })).unwrap()["running"], true);
    f.handle("exec_kill", &json!({ "job": job })).unwrap();
    assert_eq!(f.handle("exec_output", &json!({ "job": job })).unwrap()["running"], false);
    let temp = f.temp.clone();
    f.cleanup();
    assert!(!temp.exists());
}

#[test]
fn commands_stay_off_when_the_sandbox_does_not_hold() {
    let base = tempfile::tempdir().unwrap();
    let folder = base.path().join("project");
    std::fs::create_dir_all(&folder).unwrap();
    // A stand-in helper that runs commands without any confinement: the check must catch it.
    let fake = base.path().join("unconfined-helper");
    std::fs::write(&fake, "#!/bin/sh\nexec /bin/sh -c \"$3\"\n").unwrap();
    std::process::Command::new("chmod").arg("755").arg(&fake).status().unwrap();
    let mut f = Folder::new(&folder).unwrap();
    f.helper = fake;
    f.enable_commands();
    match &f.commands {
        athena_connect::sandbox::Support::Unavailable(why) => assert!(why.contains("outside the folder"), "{why}"),
        other => panic!("commands were turned on with a leaky sandbox: {other:?}"),
    }
    assert!(f.handle("exec", &json!({ "command": "echo hi" })).is_err());
    f.cleanup();
}
