use crate::storage::{checksum, windows_path};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

pub const DATABASES: [&str; 2] = ["state_5.sqlite", "thread_history_1.sqlite"];

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct App {
    install_location: PathBuf,
    pub version: String,
}

impl App {
    pub fn locate() -> Result<Self> {
        let mut command = Command::new("powershell.exe");
        hidden(&mut command);
        let result = command.args(["-NoProfile", "-NonInteractive", "-Command",
            "Get-AppxPackage OpenAI.Codex | Select-Object -First 1 InstallLocation,Version | ConvertTo-Json -Compress"]).output()?;
        ensure!(
            result.status.success() && !result.stdout.is_empty(),
            "Update or install the ChatGPT desktop app first"
        );
        Ok(serde_json::from_slice(&result.stdout)?)
    }

    pub fn reader(&self) -> PathBuf {
        self.install_location.join("app/resources/codex.exe")
    }
}

pub fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
}

#[cfg(windows)]
pub fn is_open() -> Result<bool> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
    };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    ensure!(
        snapshot != INVALID_HANDLE_VALUE,
        "Could not confirm whether ChatGPT is closed"
    );
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    let mut success = unsafe { Process32FirstW(snapshot, &mut entry) };
    if success == 0 {
        unsafe {
            CloseHandle(snapshot);
        }
        bail!("Could not check running apps");
    }
    let mut found = false;
    while success != 0 {
        let end = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..end]).to_ascii_lowercase();
        if ["chatgpt.exe", "codex.exe", "codex-app-server.exe"].contains(&name.as_str()) {
            found = true;
            break;
        }
        success = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe {
        CloseHandle(snapshot);
    }
    Ok(found)
}

#[cfg(not(windows))]
pub fn is_open() -> Result<bool> {
    Ok(false)
}

pub fn ro(path: &Path) -> Result<Connection> {
    Ok(
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("Read {}", path.display()))?,
    )
}

pub fn snapshot(src: &Path, dst: &Path) -> Result<()> {
    crate::storage::mkdir_parent(dst)?;
    ro(src)?.backup("main", dst, None)?;
    Ok(())
}

pub fn connect(path: &Path) -> Result<Connection> {
    Ok(Connection::open(path)?)
}

pub fn threads(c: &Connection) -> Result<Vec<(String, String, String)>> {
    let mut q =
        c.prepare("SELECT id,coalesce(name,title,''),rollout_path FROM threads ORDER BY id")?;
    Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn logical_path(value: &str) -> String {
    let text = value.replace('\\', "/").to_ascii_lowercase();
    text.strip_suffix(".zst").unwrap_or(&text).to_owned()
}

pub fn remap_heads(profile: &Path, keys: &[String]) -> Result<()> {
    let mut c = connect(&profile.join(DATABASES[0]))?;
    let rows = threads(&c)?;
    let transaction = c.transaction()?;
    for (id, _, old) in rows {
        let previous = logical_path(&old);
        let key = keys
            .iter()
            .find(|k| previous.ends_with(&format!("/{}", logical_path(k))));
        if let Some(key) = key {
            let name = windows_path(&profile.join(key.trim_end_matches(".zst")));
            transaction.execute("UPDATE threads SET rollout_path=? WHERE id=?", (name, id))?;
        }
    }
    transaction.commit()?;
    Ok(())
}

pub struct Reader {
    child: Child,
    input: ChildStdin,
    output: Receiver<String>,
    request: u64,
}

impl Reader {
    pub fn open(app: &App, profile: &Path) -> Result<Self> {
        let mut command = Command::new(app.reader());
        hidden(&mut command);
        let mut child = command
            .args(["app-server", "--listen", "stdio://"])
            .env("CODEX_HOME", profile)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Start ChatGPT's conversation reader")?;
        let input = child
            .stdin
            .take()
            .context("Conversation reader has no input")?;
        let output = child
            .stdout
            .take()
            .context("Conversation reader has no output")?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut reader = Self {
            child,
            input,
            output: rx,
            request: 0,
        };
        reader.call("initialize", json!({"clientInfo":{"name":"between","version":"3.0"},"capabilities":{"experimentalApi":true}}))?;
        reader.message(&json!({"method":"initialized","params":{}}))?;
        Ok(reader)
    }
    fn message(&mut self, payload: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, payload)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request += 1;
        self.message(&json!({"id":self.request,"method":method,"params":params}))?;
        let expires = Instant::now() + Duration::from_secs(90);
        loop {
            let line = self
                .output
                .recv_timeout(expires.saturating_duration_since(Instant::now()))
                .context("ChatGPT's conversation reader did not finish")?;
            let Ok(mut reply) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if reply["id"].as_u64() != Some(self.request) {
                continue;
            }
            ensure!(
                reply.get("error").is_none(),
                "ChatGPT could not read a conversation: {}",
                reply["error"]
            );
            return Ok(reply["result"].take());
        }
    }

    pub fn tokens(&mut self, id: &str) -> Result<Vec<String>> {
        let mut cursor = None;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        loop {
            let mut input =
                json!({"threadId":id,"limit":100,"sortDirection":"asc","itemsView":"full"});
            if let Some(next) = cursor {
                input["cursor"] = Value::String(next);
            }
            let output = self.call("thread/turns/list", input)?;
            for turn in output["data"]
                .as_array()
                .context("Conversation reader returned invalid turns")?
            {
                out.push(format!(
                    "turn:{}",
                    turn["id"].as_str().context("Turn has no identifier")?
                ));
                for item in turn["items"]
                    .as_array()
                    .context("Conversation reader returned invalid items")?
                {
                    let kind = item["type"]
                        .as_str()
                        .context("Conversation item has no type")?;
                    let ident = item["id"]
                        .as_str()
                        .context("Conversation item has no identifier")?;
                    let content = match kind {
                        "agentMessage" => normalize(item["text"].as_str().unwrap_or("")),
                        "userMessage" => serde_json::to_string(
                            &item["content"]
                                .as_array()
                                .context("Invalid user message")?
                                .iter()
                                .map(|part| {
                                    if part["type"] == "text" {
                                        json!([
                                            part["type"],
                                            normalize(part["text"].as_str().unwrap_or(""))
                                        ])
                                    } else if let Some(path) = part["path"].as_str() {
                                        json!([part["type"], normalize(path)])
                                    } else {
                                        part.clone()
                                    }
                                })
                                .collect::<Vec<_>>(),
                        )?,
                        _ => String::new(),
                    };
                    out.push(format!(
                        "item:{kind}:{ident}:{}",
                        checksum(content.as_bytes())
                    ));
                }
            }
            cursor = output["nextCursor"].as_str().map(str::to_owned);
            let Some(ref next) = cursor else {
                break;
            };
            ensure!(
                seen.insert(next.clone()),
                "Conversation reader returned a repeated page"
            );
        }
        Ok(out)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn normalize(value: &str) -> String {
    use std::sync::LazyLock;
    static FILES: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?i)(\]\(<?)/?[a-z]:[\\/][^\n>)]*(>?\))|((?:path|file_path)=")/?[a-z]:[\\/][^"\n]+(" )?"#).unwrap()
    });
    static PATH: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"(?mi)(^## [^\n]+?: )/?[a-z]:[\\/][^\n]+|^/?[a-z]:[\\/][^\r\n]+$"#)
            .unwrap()
    });
    let text = FILES.replace_all(value, |c: &regex::Captures<'_>| {
        if let Some(left) = c.get(1) {
            format!(
                "{}<LOCAL_FILE>{}",
                left.as_str(),
                c.get(2).unwrap().as_str()
            )
        } else {
            format!("{}<LOCAL_FILE>", c.get(3).unwrap().as_str())
        }
    });
    PATH.replace_all(&text, "${1}<LOCAL_FILE>").into_owned()
}

pub fn read_profiles(
    app: &App,
    profile: &Path,
    selected: Option<&HashSet<String>>,
    progress: impl Fn(&str),
) -> Result<BTreeMap<String, Vec<String>>> {
    let rows = threads(&ro(&profile.join(DATABASES[0]))?)?;
    let rows = rows
        .into_iter()
        .filter(|r| selected.is_none_or(|ids| ids.contains(&r.0)))
        .collect::<Vec<_>>();
    let mut reader = Reader::open(app, profile)?;
    let mut result = BTreeMap::new();
    for (i, (id, title, _)) in rows.iter().enumerate() {
        result.insert(
            id.clone(),
            reader
                .tokens(id)
                .with_context(|| format!("Check chat '{title}'"))?,
        );
        if i % 10 == 0 {
            progress(&format!("Checking chat history · {}/{}", i + 1, rows.len()));
        }
    }
    Ok(result)
}

pub fn home() -> Result<PathBuf> {
    let path = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".codex")))
        .context("Find the ChatGPT profile")?;
    ensure!(
        path.join(DATABASES[0]).is_file() && path.join(DATABASES[1]).is_file(),
        "Open the ChatGPT desktop app once before syncing"
    );
    Ok(fs::canonicalize(path)?)
}
