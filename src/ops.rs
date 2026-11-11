//! What Athena can ask this computer to do, all inside the folder: read, write,
//! look around, search, and run commands in the sandbox. Every path goes
//! through the folder guard; every command through the sandbox.

use crate::jail::Jail;
use crate::sandbox::{self, Fence, Support};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

const MAX_READ: u64 = 10 * 1024 * 1024;
const MAX_LIST: usize = 200;
const MAX_GREP_LINES: usize = 400;
const MAX_OUTPUT: usize = 200_000;

pub struct Folder {
    pub jail: Jail,
    /// A private, empty temporary folder for this session (commands' TMPDIR and download caches).
    pub temp: PathBuf,
    /// Off until `enable_commands` has proven the sandbox on this computer.
    pub commands: Support,
    /// What commands may reach, and the shell they run in.
    pub fence: Fence,
    prepared: bool,
    /// What each shell's sandbox check printed (for diagnosing why one wasn't used).
    pub probe_log: Vec<String>,
    /// The program that locks a command down before running it: this one (`athena-connect __run`).
    pub helper: PathBuf,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    next_job: Mutex<u32>,
}

struct Job {
    child: Mutex<Child>,
    output: Arc<Mutex<Vec<u8>>>,
    read_upto: Mutex<usize>,
}

fn mtime_ms(m: &std::fs::Metadata) -> f64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

fn err<T>(msg: impl Into<String>) -> Result<T, String> {
    Err(msg.into())
}

impl Folder {
    pub fn new(folder: &Path) -> std::io::Result<Folder> {
        let jail = Jail::new(folder)?;
        let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let temp = std::env::temp_dir().join(format!("athena-connect-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&temp)?;
        let fence = Fence::for_folder(jail.root(), &temp);
        Ok(Folder {
            jail,
            temp,
            commands: Support::Unavailable("they haven't been checked yet".into()),
            fence,
            prepared: false,
            probe_log: Vec::new(),
            helper: std::env::current_exe()?,
            jobs: Mutex::new(HashMap::new()),
            next_job: Mutex::new(1),
        })
    }

    /// The shell commands run in.
    pub fn shell(&self) -> &str {
        self.fence.shell.as_deref().unwrap_or("sh")
    }

    /// Turn commands on, but only once this computer has shown they're confined: the sandbox is set up,
    /// and a test command runs in it but can't read a file outside the folder. Otherwise they stay off, with the reason.
    pub fn enable_commands(&mut self) {
        self.commands = match self.prove_sandbox() {
            Ok(()) => Support::Available,
            Err(why) => Support::Unavailable(why),
        };
    }

    fn prove_sandbox(&mut self) -> Result<(), String> {
        if let Support::Unavailable(why) = sandbox::support() {
            return Err(why);
        }
        sandbox::prepare(&self.fence)?;
        self.prepared = true;
        // A file outside the folder (and outside the session's temp folder) that the test command must not read.
        let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let secret = format!("athena-probe-secret-{stamp}");
        let marker = [std::env::temp_dir(), dirs::config_dir().unwrap_or_default().join("athena-connect")]
            .into_iter()
            .map(|d| d.join(format!("athena-connect-probe-{stamp}.txt")))
            .find(|m| !m.starts_with(self.jail.root()) && !m.starts_with(&self.temp) && m.parent().is_some_and(|p| std::fs::create_dir_all(p).is_ok()) && std::fs::write(m, &secret).is_ok());
        let shells: Vec<String> = if cfg!(windows) { vec!["powershell".into(), "cmd".into()] } else { vec![self.shell().to_string()] };
        let mut last = String::new();
        let mut result = Err(String::new());
        for shell in shells {
            self.fence.shell = Some(shell.clone());
            let out = self.probe(&probe_script(&shell, marker.as_deref()));
            match out {
                Ok(out) if out.contains(&secret) => {
                    result = Err("a test command could read a file outside the folder, so they stay off".into());
                    break;
                }
                Ok(out) if out.contains("athena-probe-ok") => {
                    result = Ok(());
                    break;
                }
                Ok(out) => {
                    self.probe_log.push(format!("{shell}: {}", out.trim().replace(['\r', '\n'], " | ")));
                    last = format!("{shell}: {}", out.trim().lines().last().unwrap_or("no output"));
                }
                Err(e) => {
                    self.probe_log.push(format!("{shell}: {e}"));
                    last = format!("{shell}: {e}");
                }
            }
        }
        if let Some(m) = &marker {
            let _ = std::fs::remove_file(m);
        }
        result.map_err(|e| if e.is_empty() { format!("a test command couldn't run in the sandbox ({last})") } else { e })
    }

    /// Run a test command in the sandbox and return what it printed.
    fn probe(&self, script: &str) -> Result<String, String> {
        let mut child = self.build_command(script)?.spawn().map_err(|e| e.to_string())?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let readers = capture(&mut child, &output);
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() > Duration::from_secs(60) => {
                    kill_tree(&mut child);
                    let _ = child.wait();
                    let so_far = String::from_utf8_lossy(&output.lock().unwrap()).trim().to_string();
                    return Err(format!("it didn't finish in a minute (output so far: {})", if so_far.is_empty() { "none" } else { &so_far }));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => return Err(e.to_string()),
            }
        }
        for r in readers {
            let _ = r.join();
        }
        let out = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
        Ok(out)
    }

    fn path(&self, args: &Value, key: &str) -> Result<PathBuf, String> {
        let asked = args.get(key).and_then(Value::as_str).unwrap_or(".");
        self.jail.resolve(asked).map_err(|e| e.to_string())
    }

    pub fn handle(&self, op: &str, args: &Value) -> Result<Value, String> {
        match op {
            "stat" => self.stat(args),
            "read" => self.read(args),
            "write" => self.write(args),
            "glob" => self.glob(args),
            "grep" => self.grep(args),
            "exec" => self.exec(args),
            "exec_start" => self.exec_start(args),
            "exec_output" => self.exec_output(args),
            "exec_kill" => self.exec_kill(args),
            _ => err(format!("this connector doesn't know how to {op}")),
        }
    }

    fn stat(&self, args: &Value) -> Result<Value, String> {
        let p = self.path(args, "path")?;
        let shown = self.jail.display(&p);
        Ok(match std::fs::metadata(&p) {
            Ok(m) => json!({ "path": shown, "exists": true, "dir": m.is_dir(), "size": m.len(), "mtime": mtime_ms(&m) }),
            Err(_) => json!({ "path": shown, "exists": false, "dir": false, "size": 0, "mtime": 0 }),
        })
    }

    fn read(&self, args: &Value) -> Result<Value, String> {
        let p = self.path(args, "path")?;
        let shown = self.jail.display(&p);
        let m = std::fs::metadata(&p).map_err(|_| format!("No file at {shown}."))?;
        if m.is_dir() {
            return err(format!("{shown} is a folder. Use glob to list files."));
        }
        if m.len() > MAX_READ {
            return err(format!("{shown} is too large to read ({} MB).", m.len() / 1024 / 1024));
        }
        let bytes = std::fs::read(&p).map_err(|e| format!("Couldn't read {shown}: {e}"))?;
        let binary = bytes.iter().take(8000).any(|b| *b == 0);
        Ok(json!({
            "path": shown,
            "binary": binary,
            "text": if binary { Value::Null } else { Value::String(String::from_utf8_lossy(&bytes).into_owned()) },
            "size": m.len(),
            "mtime": mtime_ms(&m),
        }))
    }

    fn write(&self, args: &Value) -> Result<Value, String> {
        let p = self.path(args, "path")?;
        let shown = self.jail.display(&p);
        let text = args.get("text").and_then(Value::as_str).ok_or("write needs text")?;
        if let (Some(expect), Ok(m)) = (args.get("expectMtime").and_then(Value::as_f64), std::fs::metadata(&p)) {
            if (mtime_ms(&m) - expect).abs() > 0.5 {
                return err(format!("{shown} changed since you read it. Read it again first."));
            }
        }
        if let Some(parent) = p.parent() {
            // The parent was resolved inside the folder too (it's part of the same path).
            std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create the folder for {shown}: {e}"))?;
        }
        // Re-check after creating folders: a link swapped in meanwhile still can't lead outside.
        let p = self.jail.resolve(p.to_str().unwrap_or("")).map_err(|e| e.to_string())?;
        std::fs::write(&p, text).map_err(|e| format!("Couldn't write {shown}: {e}"))?;
        let m = std::fs::metadata(&p).map_err(|e| e.to_string())?;
        Ok(json!({ "path": shown, "mtime": mtime_ms(&m) }))
    }

    /// Files under the folder, skipping what .gitignore ignores and .git itself.
    fn walk(&self, base: &Path) -> impl Iterator<Item = PathBuf> + '_ {
        ignore::WalkBuilder::new(base)
            .hidden(false)
            .follow_links(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.into_path())
            .filter(move |p| self.jail.contains(p))
    }

    fn glob(&self, args: &Value) -> Result<Value, String> {
        let base = self.path(args, "path")?;
        let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("**/*");
        let matcher = globset::GlobBuilder::new(pattern).literal_separator(false).build().map_err(|e| format!("Not a valid pattern: {e}"))?.compile_matcher();
        let mut found: Vec<(f64, String)> = self
            .walk(&base)
            .filter(|p| {
                let rel = p.strip_prefix(&base).unwrap_or(p);
                matcher.is_match(rel) || p.file_name().map(|n| matcher.is_match(n)).unwrap_or(false)
            })
            .map(|p| (std::fs::metadata(&p).map(|m| mtime_ms(&m)).unwrap_or(0.0), self.jail.display(&p)))
            .collect();
        found.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let total = found.len();
        Ok(json!({ "matches": found.into_iter().take(MAX_LIST).map(|f| f.1).collect::<Vec<_>>(), "total": total }))
    }

    fn grep(&self, args: &Value) -> Result<Value, String> {
        let base = self.path(args, "path")?;
        let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("");
        let re = regex::RegexBuilder::new(pattern)
            .case_insensitive(args.get("ignoreCase").and_then(Value::as_bool).unwrap_or(false))
            .build()
            .map_err(|e| format!("Not a valid regular expression: {e}"))?;
        let only = match args.get("glob").and_then(Value::as_str) {
            Some(g) => Some(globset::Glob::new(g).map_err(|e| format!("Not a valid glob: {e}"))?.compile_matcher()),
            None => None,
        };
        let mode = args.get("output").and_then(Value::as_str).unwrap_or("files");
        let context = args.get("context").and_then(Value::as_u64).unwrap_or(0) as usize;
        let files: Vec<PathBuf> = if base.is_file() { vec![base.clone()] } else { self.walk(&base).collect() };
        let mut lines = Vec::new();
        for f in files {
            if let Some(m) = &only {
                if !f.file_name().map(|n| m.is_match(n)).unwrap_or(false) && !m.is_match(f.strip_prefix(self.jail.root()).unwrap_or(&f)) {
                    continue;
                }
            }
            let Ok(bytes) = std::fs::read(&f) else { continue };
            if bytes.iter().take(8000).any(|b| *b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let shown = self.jail.display(&f);
            let all: Vec<&str> = text.lines().collect();
            let hits: Vec<usize> = all.iter().enumerate().filter(|(_, l)| re.is_match(l)).map(|(i, _)| i).collect();
            if hits.is_empty() {
                continue;
            }
            match mode {
                "count" => lines.push(format!("{shown}:{}", hits.len())),
                "content" => {
                    let mut shown_upto = 0usize;
                    for &h in &hits {
                        let from = h.saturating_sub(context).max(shown_upto);
                        let to = (h + context + 1).min(all.len());
                        for (i, line) in all.iter().enumerate().take(to).skip(from) {
                            let sep = if i == h { ':' } else { '-' };
                            lines.push(format!("{shown}{sep}{}{sep}{}", i + 1, line.chars().take(500).collect::<String>()));
                        }
                        shown_upto = to;
                    }
                }
                _ => lines.push(shown),
            }
        }
        let total = lines.len();
        let limit = if mode == "content" { MAX_GREP_LINES } else { MAX_LIST };
        lines.truncate(limit);
        Ok(json!({ "lines": lines, "total": total }))
    }

    // --- commands: always in the sandbox -----------------------------------

    fn command(&self, command: &str) -> Result<Command, String> {
        if let Support::Unavailable(why) = &self.commands {
            return err(format!("Commands are off on this computer: {why}"));
        }
        self.build_command(command)
    }

    fn build_command(&self, command: &str) -> Result<Command, String> {
        let mut cmd = Command::new(&self.helper);
        cmd.arg("__run").arg(serde_json::to_string(&self.fence).map_err(|e| e.to_string())?).arg(command);
        cmd.current_dir(self.jail.root()).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.env("TMPDIR", &self.temp).env("TMP", &self.temp).env("TEMP", &self.temp).env("PAGER", "cat").env("GIT_PAGER", "cat").env("TERM", "dumb");
        // Toolchain folders are read only: downloads (cargo, npm, pip) go to the private temp folder.
        cmd.env("CARGO_HOME", self.temp.join("cargo")).env("npm_config_cache", self.temp.join("npm")).env("PIP_CACHE_DIR", self.temp.join("pip"));
        // Windows PowerShell gets only its built-in modules: inherited from PowerShell 7 it would look in 7's (and fail),
        // and with every installed module it re-indexes them all for each session (it can't reach its usual index).
        #[cfg(windows)]
        cmd.env("PSModulePath", format!("{}\\System32\\WindowsPowerShell\\v1.0\\Modules", std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into())));
        // Programs that keep settings in your user folder get private ones instead (Windows). LOCALAPPDATA, TEMP and
        // TMP Windows itself points at the container's own folder (removed when the session ends).
        #[cfg(windows)]
        {
            let d = self.temp.join("appdata");
            let _ = std::fs::create_dir_all(&d);
            cmd.env("APPDATA", d);
        }
        if let Some(home) = dirs::home_dir() {
            if home.join(".rustup").exists() {
                cmd.env("RUSTUP_HOME", home.join(".rustup"));
            }
            let cargo_bin = home.join(".cargo").join("bin");
            let mut path: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
            if cargo_bin.exists() && !path.contains(&cargo_bin) {
                path.push(cargo_bin);
            }
            if let Ok(p) = std::env::join_paths(path) {
                cmd.env("PATH", p);
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        Ok(cmd)
    }

    fn exec(&self, args: &Value) -> Result<Value, String> {
        let command = args.get("command").and_then(Value::as_str).ok_or("exec needs a command")?;
        let timeout = Duration::from_millis(args.get("timeoutMs").and_then(Value::as_u64).unwrap_or(120_000).min(600_000));
        let mut child = self.command(command)?.spawn().map_err(|e| format!("Couldn't start the command: {e}"))?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let readers = capture(&mut child, &output);
        // Tracked like a background command, so ending the session stops it too.
        let id = self.job_id("run");
        let job = Arc::new(Job { child: Mutex::new(child), output: output.clone(), read_upto: Mutex::new(0) });
        self.jobs.lock().unwrap().insert(id.clone(), job.clone());
        let start = Instant::now();
        let waited = loop {
            let mut child = job.child.lock().unwrap();
            match child.try_wait() {
                Ok(Some(status)) => break Ok((status.code(), false)),
                Ok(None) if start.elapsed() > timeout => {
                    kill_tree(&mut child);
                    let _ = child.wait();
                    break Ok((None, true));
                }
                Ok(None) => {
                    drop(child);
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => break Err(e.to_string()),
            }
        };
        self.jobs.lock().unwrap().remove(&id);
        let (code, timed_out) = waited?;
        for r in readers {
            let _ = r.join();
        }
        let out = String::from_utf8_lossy(&output.lock().unwrap()).into_owned();
        Ok(json!({ "code": code, "output": out, "timedOut": timed_out }))
    }

    fn exec_start(&self, args: &Value) -> Result<Value, String> {
        let command = args.get("command").and_then(Value::as_str).ok_or("exec_start needs a command")?;
        let mut child = self.command(command)?.spawn().map_err(|e| format!("Couldn't start the command: {e}"))?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let _ = capture(&mut child, &output);
        let id = self.job_id("job");
        let job = Arc::new(Job { child: Mutex::new(child), output, read_upto: Mutex::new(0) });
        self.jobs.lock().unwrap().insert(id.clone(), job.clone());
        std::thread::sleep(Duration::from_millis(300));
        let first = take_new(&job);
        Ok(json!({ "job": id, "output": first }))
    }

    fn exec_output(&self, args: &Value) -> Result<Value, String> {
        let id = args.get("job").and_then(Value::as_str).unwrap_or("");
        let job = self.jobs.lock().unwrap().get(id).cloned().ok_or(format!("There's no background command {id}."))?;
        let status = job.child.lock().unwrap().try_wait().map_err(|e| e.to_string())?;
        Ok(json!({ "output": take_new(&job), "running": status.is_none(), "code": status.and_then(|s| s.code()) }))
    }

    fn exec_kill(&self, args: &Value) -> Result<Value, String> {
        let id = args.get("job").and_then(Value::as_str).unwrap_or("");
        let job = self.jobs.lock().unwrap().get(id).cloned().ok_or(format!("There's no background command {id}."))?;
        let mut child = job.child.lock().unwrap();
        let running = child.try_wait().map(|s| s.is_none()).unwrap_or(false);
        if running {
            kill_tree(&mut child);
            let _ = child.wait();
        }
        Ok(json!({ "stopped": running }))
    }

    fn job_id(&self, prefix: &str) -> String {
        let mut n = self.next_job.lock().unwrap();
        *n += 1;
        format!("{prefix}{}", *n - 1)
    }

    /// Stop every command still running and remove the temp folder (when the session ends).
    pub fn cleanup(&self) {
        for job in self.jobs.lock().unwrap().values() {
            let mut c = job.child.lock().unwrap();
            if c.try_wait().map(|s| s.is_none()).unwrap_or(false) {
                kill_tree(&mut c);
                let _ = c.wait();
            }
        }
        if self.prepared {
            sandbox::release(&self.fence);
        }
        let _ = std::fs::remove_dir_all(&self.temp);
    }
}

/// A test command: writes a word to a file in its temp folder and prints it back (so it can work), and tries to
/// print a file outside the folder (which must fail). The word is put together as it runs, so an error message
/// that repeats the command can't pass for it.
fn probe_script(shell: &str, marker: Option<&Path>) -> String {
    let m = marker.map(|m| m.display().to_string());
    let outside = |m: &Option<String>, f: &dyn Fn(&str) -> String| m.as_deref().map(f).unwrap_or_default();
    match shell {
        "powershell" => format!(
            "Set-Content -LiteralPath (Join-Path $env:TEMP 'probe.txt') -Value ('athena-probe-' + 'ok'); Get-Content -LiteralPath (Join-Path $env:TEMP 'probe.txt'){}",
            outside(&m, &|m| format!("; Get-Content -LiteralPath '{}' -ErrorAction SilentlyContinue", m.replace('\'', "''")))
        ),
        "cmd" => format!("echo athena-probe-^ok> \"%TEMP%\\probe.txt\" & type \"%TEMP%\\probe.txt\"{}", outside(&m, &|m| format!(" & type \"{m}\""))),
        _ => format!("p=athena-probe-; echo \"${{p}}ok\" > \"$TMPDIR/probe.txt\" && cat \"$TMPDIR/probe.txt\"{}", outside(&m, &|m| format!("; cat '{}'", m.replace('\'', "'\\''")))),
    }
}

fn take_new(job: &Job) -> String {
    let out = job.output.lock().unwrap();
    let mut upto = job.read_upto.lock().unwrap();
    let new = String::from_utf8_lossy(&out[*upto..]).into_owned();
    *upto = out.len();
    new
}

/// Collect stdout and stderr together, up to a limit.
fn capture(child: &mut Child, output: &Arc<Mutex<Vec<u8>>>) -> Vec<std::thread::JoinHandle<()>> {
    let mut handles = Vec::new();
    let streams: Vec<Box<dyn Read + Send>> = [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)].into_iter().flatten().collect();
    for mut s in streams {
        let out = output.clone();
        handles.push(std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = s.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut o = out.lock().unwrap();
                if o.len() < MAX_OUTPUT {
                    o.extend_from_slice(&buf[..n]);
                }
            }
        }));
    }
    handles
}

fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        // The command runs in its own process group: stop it and everything it started.
        libc_kill(-(child.id() as i32), 9);
    }
    let _ = child.kill();
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
