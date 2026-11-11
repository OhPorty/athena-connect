//! Pairing with an Athena server, and a session: connect, say which folder,
//! answer Athena's requests, and show the conversation in this terminal.

use crate::ops::Folder;
use crate::sandbox::Support;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const TEAL: &str = "\x1b[38;2;43;192;180m";
const GOLD: &str = "\x1b[38;2;227;185;74m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[38;2;236;117;100m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub server: String,
    pub key: String,
    pub account: String,
}

pub fn config_file() -> PathBuf {
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("athena-connect").join("config.json")
}

pub fn load_config() -> Option<Config> {
    serde_json::from_str(&std::fs::read_to_string(config_file()).ok()?).ok()
}

fn save_config(c: &Config) -> std::io::Result<()> {
    let file = config_file();
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, serde_json::to_string_pretty(c).unwrap())?;
    // The key gives access to your Athena account's folders: readable by you only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn machine_name() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
    }
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "computer".into())
}

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

/// `athena-connect login <server>`: sign in once; this computer gets its own key.
pub fn login(server: &str) -> Result<(), String> {
    let server = server.trim().trim_end_matches('/').to_string();
    if !server.starts_with("http://") && !server.starts_with("https://") {
        return Err("Give your Athena's address, e.g. https://athena.example.com".into());
    }
    if server.starts_with("http://") && !is_local(&server) {
        eprintln!("{GOLD}Warning:{RESET} {server} isn't https, so your password crosses the network unencrypted.");
    }
    println!("Sign in to {BOLD}{server}{RESET} to pair this computer ({}).", machine_name());
    let username = ask("Username: ");
    let password = rpassword::prompt_password("Password: ").map_err(|e| e.to_string())?;
    let code = ask("Authenticator code: ");
    let res = ureq::post(&format!("{server}/api/connect/pair"))
        .config()
        .http_status_as_error(false)
        .build()
        .send_json(json!({ "username": username, "password": password, "code": code, "machine": machine_name() }))
        .map_err(|e| format!("Couldn't reach {server}: {e}"))?;
    let status = res.status().as_u16();
    let body: Value = res.into_body().read_json().unwrap_or(json!({}));
    if status != 200 {
        return Err(body.get("error").and_then(Value::as_str).unwrap_or("Signing in failed.").to_string());
    }
    let key = body.get("key").and_then(Value::as_str).ok_or("The server didn't send a key.")?;
    save_config(&Config { server: server.clone(), key: key.to_string(), account: username.clone() }).map_err(|e| format!("Couldn't save the key: {e}"))?;
    println!("{TEAL}Paired.{RESET} This computer can now give Athena access to a folder: run {BOLD}athena-connect{RESET} in that folder.");
    println!("{DIM}You can remove this computer any time in Athena's settings (Account & security).{RESET}");
    Ok(())
}

fn is_local(url: &str) -> bool {
    url.contains("://localhost") || url.contains("://127.") || url.contains("://[::1]")
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

fn connect(cfg: &Config) -> Result<Socket, String> {
    let ws_url = format!("{}/api/connect", cfg.server.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1));
    let mut req = ws_url.into_client_request().map_err(|e| e.to_string())?;
    req.headers_mut().insert("Authorization", format!("Bearer {}", cfg.key).parse().map_err(|_| "bad key")?);
    let (mut socket, _) = tungstenite::connect(req).map_err(|e| match e {
        tungstenite::Error::Http(r) if r.status() == 401 => "This computer's key was refused (it may have been removed in Athena's settings). Run athena-connect login again.".to_string(),
        other => format!("Couldn't connect to {}: {other}", cfg.server),
    })?;
    // Short reads, so the loop can also send replies and typed messages.
    let timeout = Some(Duration::from_millis(50));
    match socket.get_mut() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(s) => s.get_mut().set_read_timeout(timeout),
        _ => Ok(()),
    }
    .map_err(|e| e.to_string())?;
    Ok(socket)
}

enum Out {
    Text(String),
    Line(String),
}

/// `athena-connect [folder]`: give Athena access to this folder until the terminal closes.
pub fn run(folder: &Path) -> Result<(), String> {
    let cfg = load_config().ok_or("This computer isn't paired with an Athena yet. Run: athena-connect login https://your-athena")?;
    let folder = Arc::new(Folder::new(folder).map_err(|e| format!("Can't use {}: {e}", folder.display()))?);
    let root = folder.jail.root().display().to_string();
    let (commands, commands_off) = match &folder.commands {
        Support::Available => (true, None),
        Support::Unavailable(why) => (false, Some(why.clone())),
    };
    let shell = if cfg!(windows) { "powershell" } else if Path::new("/bin/bash").exists() { "bash" } else { "sh" };

    let mut socket = connect(&cfg)?;
    send(&mut socket, json!({ "type": "hello", "info": { "machine": machine_name(), "os": std::env::consts::OS, "shell": shell, "folder": root, "commands": commands, "commandsOff": commands_off, "version": env!("CARGO_PKG_VERSION") } }))?;

    println!("{TEAL}{BOLD}Athena has access to {root}{RESET}");
    println!("{DIM}Only this folder: anything outside it is refused here, on this computer.{RESET}");
    if let Some(why) = &commands_off {
        println!("{GOLD}Commands are off:{RESET} {why}. Athena can still read and edit files here.");
    }

    // Replies from request threads, and lines typed here, go out through this channel.
    let (tx, rx) = mpsc::channel::<Out>();
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if tx.send(Out::Line(line)).is_err() {
                    break;
                }
            }
        });
    }

    // Ctrl+C, closing the terminal, or being told to stop: end access cleanly
    // (stop Athena's commands, remove the temp folder) instead of just vanishing.
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }

    let mut view = View::default();
    let result = loop {
        if stop.load(Ordering::SeqCst) {
            let _ = socket.close(None);
            let _ = socket.flush();
            break Ok(());
        }
        // Incoming.
        match socket.read() {
            Ok(Message::Text(t)) => {
                let Ok(msg) = serde_json::from_str::<Value>(t.as_str()) else { continue };
                match msg.get("type").and_then(Value::as_str) {
                    Some("request") => {
                        let (id, op, args) = (msg["id"].clone(), msg["op"].as_str().unwrap_or("").to_string(), msg["args"].clone());
                        let (folder, tx) = (folder.clone(), tx.clone());
                        std::thread::spawn(move || {
                            let reply = match folder.handle(&op, &args) {
                                Ok(result) => json!({ "type": "response", "id": id, "ok": true, "result": result }),
                                Err(error) => json!({ "type": "response", "id": id, "ok": false, "error": error }),
                            };
                            let _ = tx.send(Out::Text(reply.to_string()));
                        });
                    }
                    Some("ready") => view.ready(&msg["session"]),
                    Some("refused") => break Err(msg["message"].as_str().unwrap_or("The server refused the connection.").to_string()),
                    Some("session") => view.show(&msg["msg"]),
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => break Ok(()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => break Ok(()),
            Err(e) => break Err(format!("The connection dropped: {e}")),
        }
        // Outgoing.
        let mut failed = None;
        while let Ok(out) = rx.try_recv() {
            let text = match out {
                Out::Text(t) => t,
                Out::Line(line) => match view.typed(&line) {
                    Some(msg) => json!({ "type": "client", "msg": msg }).to_string(),
                    None => continue,
                },
            };
            if let Err(e) = socket.send(Message::text(text)) {
                failed = Some(e.to_string());
                break;
            }
        }
        if let Some(e) = failed {
            break Err(format!("The connection dropped: {e}"));
        }
    };
    folder.cleanup();
    println!("\n{DIM}Athena no longer has access to {root}.{RESET}");
    result
}

fn send(socket: &mut Socket, v: Value) -> Result<(), String> {
    socket.send(Message::text(v.to_string())).map_err(|e| e.to_string())
}

/// What this terminal shows of the conversation, and what typing here means.
#[derive(Default)]
struct View {
    /// A permission or plan question waiting for y/n here.
    question: Option<(String, bool)>,
    in_text: bool,
    busy: bool,
}

impl View {
    fn ready(&mut self, session: &Value) {
        let url = session["url"].as_str().unwrap_or("");
        println!("\nOpen it on your phone or another computer:\n{BOLD}{url}{RESET}");
        if let Ok(code) = qrcode::QrCode::new(url.as_bytes()) {
            println!("{}", code.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build());
        }
        println!("{DIM}Type here to talk to Athena too. Close this terminal (or press Ctrl+C) to end its access.{RESET}\n");
    }

    fn end_text(&mut self) {
        if self.in_text {
            println!();
            self.in_text = false;
        }
    }

    fn show(&mut self, m: &Value) {
        match m["type"].as_str() {
            Some("turn_start") => {
                self.busy = true;
                self.end_text();
                println!("{TEAL}>{RESET} {}", m["text"].as_str().unwrap_or(""));
            }
            Some("event") => {
                let ev = &m["event"];
                match ev["type"].as_str() {
                    Some("text") => {
                        print!("{}", ev["text"].as_str().unwrap_or(""));
                        let _ = std::io::stdout().flush();
                        self.in_text = true;
                    }
                    Some("tool_start") => {
                        self.end_text();
                        let call = &ev["call"];
                        let arg = ["path", "command", "pattern"].iter().find_map(|k| call["args"][k].as_str()).unwrap_or("");
                        println!("{DIM}  · {} {}{RESET}", call["name"].as_str().unwrap_or(""), arg.chars().take(100).collect::<String>());
                    }
                    Some("tool_end") if ev["result"]["isError"].as_bool() == Some(true) => {
                        let first = ev["result"]["content"].as_str().unwrap_or("").lines().next().unwrap_or("").chars().take(160).collect::<String>();
                        println!("{RED}    ✕ {first}{RESET}");
                    }
                    Some("turn_end") => {
                        self.end_text();
                        self.busy = false;
                        if ev["reason"].as_str() == Some("error") {
                            println!("{RED}Error: {}{RESET}", ev["error"].as_str().unwrap_or(""));
                        }
                    }
                    _ => {}
                }
            }
            Some("question") | Some("hello") => {
                let q = if m["type"] == "hello" { m["pending"].get(0).cloned().unwrap_or(Value::Null) } else { m["question"].clone() };
                if q.is_null() {
                    return;
                }
                self.end_text();
                let id = q["id"].as_str().unwrap_or("").to_string();
                if q["type"] == "plan" {
                    println!("{GOLD}{BOLD}Athena's plan:{RESET} {}", q["plan"]["summary"].as_str().unwrap_or(""));
                    for (i, s) in q["plan"]["steps"].as_array().cloned().unwrap_or_default().iter().enumerate() {
                        println!("  {}. {}", i + 1, s.as_str().unwrap_or(""));
                    }
                    println!("{GOLD}Go ahead? [y/n]{RESET}");
                    self.question = Some((id, true));
                } else {
                    println!("{GOLD}{BOLD}Athena asks:{RESET} {}  {DIM}({}){RESET}", q["request"]["summary"].as_str().unwrap_or(""), q["request"]["reason"].as_str().unwrap_or(""));
                    println!("{GOLD}Allow? [y/n]{RESET}");
                    self.question = Some((id, false));
                }
            }
            Some("question_closed") => {
                if self.question.as_ref().map(|q| q.0.as_str()) == m["id"].as_str() {
                    self.question = None;
                }
            }
            Some("error") => {
                self.end_text();
                println!("{RED}{}{RESET}", m["message"].as_str().unwrap_or(""));
            }
            _ => {}
        }
    }

    /// A line typed here: an answer to a waiting question, or a message to Athena.
    fn typed(&mut self, line: &str) -> Option<Value> {
        let line = line.trim();
        if let Some((id, plan)) = self.question.take() {
            let yes = matches!(line.to_lowercase().as_str(), "y" | "yes");
            let no = matches!(line.to_lowercase().as_str(), "n" | "no");
            if !yes && !no {
                self.question = Some((id, plan));
                println!("{GOLD}Answer y or n.{RESET}");
                return None;
            }
            return Some(if plan { json!({ "type": "plan_answer", "id": id, "approved": yes }) } else { json!({ "type": "answer", "id": id, "allow": yes }) });
        }
        if line.is_empty() {
            return None;
        }
        if self.busy {
            println!("{DIM}Athena is still working; send that when it's done.{RESET}");
            return None;
        }
        Some(json!({ "type": "prompt", "text": line }))
    }
}
