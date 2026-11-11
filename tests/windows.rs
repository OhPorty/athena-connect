// Windows: commands run in the folder's AppContainer and can't reach anything outside it.
// Runs on GitHub's Windows machines for every push.
#![cfg(windows)]

use athena_connect::ops::Folder;
use athena_connect::sandbox::Support;
use serde_json::json;
use std::path::PathBuf;

#[test]
fn commands_run_in_the_folder_and_nowhere_else() {
    let base = tempfile::tempdir().unwrap();
    let folder = base.path().join("project");
    std::fs::create_dir_all(&folder).unwrap();
    let secret = base.path().join("secret.txt");
    std::fs::write(&secret, "outside").unwrap();
    let home = dirs::home_dir().unwrap();
    let escaped = home.join("athena-escaped.txt");

    let mut f = Folder::new(&folder).unwrap();
    f.helper = PathBuf::from(env!("CARGO_BIN_EXE_athena-connect"));
    f.enable_commands();
    for line in &f.probe_log {
        println!("check that didn't pass: {line}");
    }
    assert_eq!(f.commands, Support::Available, "the sandbox should prove itself here");
    println!("shell: {}", f.shell());
    let run = |command: String| f.handle("exec", &json!({ "command": command, "timeoutMs": 60000 })).unwrap();
    let ps = f.shell() == "powershell";

    // Inside: normal work, including the private temp folder.
    let out = run(if ps { "Set-Content -Path made.txt -Value hello; Get-Content made.txt; Set-Content -Path \"$env:TEMP\\t.txt\" -Value scratch; Get-Content \"$env:TEMP\\t.txt\"".into() } else { "echo hello> made.txt & type made.txt & echo scratch> \"%TEMP%\\t.txt\" & type \"%TEMP%\\t.txt\"".into() });
    assert_eq!(out["code"], 0, "{out}");
    assert_eq!(std::fs::read_to_string(folder.join("made.txt")).unwrap().trim(), "hello", "{out}");
    // Folders made inside are writable too (they inherit the folder's access and label).
    let out = run(if ps { "New-Item -ItemType Directory -Path a\\b | Out-Null; Set-Content -Path a\\b\\c.txt -Value deep".into() } else { "mkdir a\\b & echo deep> a\\b\\c.txt".into() });
    assert_eq!(out["code"], 0, "{out}");
    assert_eq!(std::fs::read_to_string(folder.join("a").join("b").join("c.txt")).unwrap().trim(), "deep", "{out}");

    // Outside: reading, changing, deleting and listing are all refused.
    let s = secret.display();
    let attempts: Vec<String> = if ps {
        vec![
            format!("Get-Content -LiteralPath '{s}'"),
            format!("Set-Content -LiteralPath '{s}' -Value changed"),
            format!("Remove-Item -LiteralPath '{s}'"),
            format!("Get-ChildItem -LiteralPath '{}'", home.display()),
            format!("Set-Content -LiteralPath '{}' -Value x", escaped.display()),
            "Get-Content ..\\secret.txt".into(),
        ]
    } else {
        vec![format!("type \"{s}\""), format!("echo changed> \"{s}\""), format!("del \"{s}\""), format!("dir \"{}\"", home.display()), format!("echo x> \"{}\"", escaped.display()), "type ..\\secret.txt".into()]
    };
    for cmd in attempts {
        let out = run(cmd.clone());
        assert!(!out["output"].as_str().unwrap().contains("outside"), "read outside: {cmd}\n{out}");
    }
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "outside");
    assert!(!escaped.exists());

    // A background command stops when the session ends.
    let started = f.handle("exec_start", &json!({ "command": if ps { "Start-Sleep -Seconds 300" } else { "for /l %i in (1,0,2) do @rem" } })).unwrap();
    let job = started["job"].as_str().unwrap().to_string();
    assert_eq!(f.handle("exec_output", &json!({ "job": job })).unwrap()["running"], true);
    let temp = f.temp.clone();
    f.cleanup();
    assert!(!temp.exists());
}
