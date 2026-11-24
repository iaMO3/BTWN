use crate::{
    codex,
    lan::{Entry, Offer, Outcome, Pointer, object},
    sidebar,
    storage::*,
};
use anyhow::{Context, Result, ensure};
use regex::Regex;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::SystemTime,
};
use walkdir::WalkDir;

const HISTORY: [&str; 4] = [
    "thread_turns",
    "thread_items",
    "thread_history_projection_state",
    "thread_realtime_items",
];
const EXTRA: [(&str, &str); 4] = [
    ("thread_attachments", "thread_id"),
    ("thread_dynamic_tools", "thread_id"),
    ("thread_read_receipts", "thread_id"),
    ("thread_spawn_edges", "child_thread_id"),
];

#[derive(Clone, Serialize, Deserialize)]
struct Index {
    snapshot: String,
    source: String,
    app_version: String,
    files: Vec<Entry>,
    chats: usize,
    #[serde(default)]
    project_files: usize,
}

#[derive(Clone, Serialize, Deserialize)]
struct Asset {
    original: String,
    #[serde(default)]
    object: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Archive {
    name: String,
    files: Vec<Entry>,
}
#[derive(Default, Serialize, Deserialize)]
struct Projects {
    #[serde(default)]
    archives: Vec<Archive>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<Entry>,
}

#[derive(PartialEq, Serialize, Deserialize)]
struct ProjectStamp {
    bytes: u64,
    modified: SystemTime,
    hash: String,
}

#[derive(Default, Serialize, Deserialize)]
struct PublishCatalog {
    files: BTreeMap<PathBuf, ProjectStamp>,
    references: BTreeMap<String, Vec<Asset>>,
}

struct PublishFiles<'a> {
    root: &'a Path,
    objects: Option<&'a Path>,
    catalog: PublishCatalog,
    next_catalog: PublishCatalog,
    entries: BTreeMap<String, Entry>,
}

impl PublishFiles<'_> {
    fn digest(&mut self, path: &Path, metadata: &fs::Metadata) -> Result<String> {
        let bytes = metadata.len();
        let modified = metadata.modified()?;
        let previous = self.catalog.files.get(path);
        let digest = match (self.objects, previous) {
            (Some(store), Some(stamp))
                if stamp.bytes == bytes
                    && stamp.modified == modified
                    && object(store, &stamp.hash)?
                        .metadata()
                        .is_ok_and(|m| m.is_file() && m.len() == bytes) =>
            {
                stamp.hash.clone()
            }
            _ => hash(path)?,
        };
        self.next_catalog.files.insert(
            path.to_path_buf(),
            ProjectStamp {
                bytes,
                modified,
                hash: digest.clone(),
            },
        );
        Ok(digest)
    }

    fn stage(&mut self, path: &Path, relative: String, digest: String) -> Result<()> {
        let stamp = &self.next_catalog.files[path];
        let destination = scoped(self.root, &relative)?;
        let blob = self.objects.map(|root| object(root, &digest)).transpose()?;
        let stored = blob.as_deref().unwrap_or(&destination);
        if !stored.try_exists()? {
            verified_stream(&mut fs::File::open(path)?, stored, stamp.bytes, &digest)?;
        }
        let existing = fs::symlink_metadata(stored)?;
        ensure!(
            existing.is_file() && !linked(&existing) && existing.len() == stamp.bytes,
            "A stored chat file has an unexpected size or type"
        );
        if blob.is_some() {
            copy_link(stored, &destination)?;
        }
        let current = fs::symlink_metadata(path)?;
        ensure!(
            current.is_file()
                && !linked(&current)
                && current.len() == stamp.bytes
                && current.modified()? == stamp.modified,
            "A chat file changed while being prepared. Close ChatGPT and try again."
        );
        self.entries.insert(
            relative.clone(),
            Entry {
                path: relative,
                bytes: stamp.bytes,
                sha256: digest,
            },
        );
        Ok(())
    }
}

#[derive(Clone)]
struct Raw {
    relative: String,
    path: PathBuf,
    physical: String,
    owner: String,
    parent: Option<String>,
}

fn input(path: &Path) -> Result<Box<dyn BufRead>> {
    let file = fs::File::open(path)?;
    if path.extension().is_some_and(|name| name == "zst") {
        Ok(Box::new(BufReader::new(zstd::stream::read::Decoder::new(
            file,
        )?)))
    } else {
        Ok(Box::new(BufReader::new(file)))
    }
}

fn linked(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.is_symlink()
    }
}

fn raw_files(home: &Path) -> Result<Vec<Raw>> {
    let ids = Regex::new(r"(?i)[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")?;
    let mut result = Vec::new();
    for folder in ["sessions", "archived_sessions"] {
        let root = home.join(folder);
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(&root).follow_links(false) {
            let p = entry?.into_path();
            let metadata = fs::symlink_metadata(&p)?;
            ensure!(
                !linked(&metadata),
                "An unexpected linked file was found in ChatGPT history"
            );
            if !metadata.is_file() {
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy();
            if !name.ends_with(".jsonl") && !name.ends_with(".jsonl.zst") {
                continue;
            }
            let physical = ids
                .find_iter(&name)
                .last()
                .context("Could not identify a saved conversation")?
                .as_str()
                .to_ascii_lowercase();
            let mut line = String::new();
            input(&p)?.read_line(&mut line)?;
            let header: Value = serde_json::from_str(&line)
                .with_context(|| format!("Read conversation header {name}"))?;
            let owner = header["payload"]["id"]
                .as_str()
                .context("Conversation header has no owner")?
                .to_owned();
            let parent = header["payload"]["history_base"]["thread_id"]
                .as_str()
                .map(str::to_owned);
            result.push(Raw {
                relative: p.strip_prefix(home)?.to_string_lossy().replace('\\', "/"),
                path: p,
                physical,
                owner,
                parent,
            });
        }
    }
    result.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(result)
}

fn copy_raw(raw: &[Raw], to: &Path) -> Result<()> {
    for record in raw {
        let out = scoped(to, &record.relative)?;
        mkdir_parent(&out)?;
        let before = record.path.metadata()?;
        fs::copy(&record.path, &out)?;
        let after = record.path.metadata()?;
        ensure!(
            before.len() == after.len() && before.modified()? == after.modified()?,
            "A conversation changed during backup. Close ChatGPT and try again."
        );
    }
    Ok(())
}

fn copy_databases(src: &Path, dst: &Path) -> Result<()> {
    for file in codex::DATABASES {
        codex::snapshot(&src.join(file), &dst.join(file))?;
    }
    Ok(())
}

fn remap_raw(dst: &Path, raw: &[Raw]) -> Result<()> {
    codex::remap_heads(
        dst,
        &raw.iter().map(|r| r.relative.clone()).collect::<Vec<_>>(),
    )
}

fn profile(src: &Path, raw: &[Raw], dst: &Path) -> Result<()> {
    copy_databases(src, dst)?;
    copy_raw(raw, dst)?;
    remap_raw(dst, raw)
}

// Scratch profiles can share transcripts with immutable staged or recovery profiles.
fn link_raw(raw: &[Raw], dst: &Path) -> Result<()> {
    for record in raw {
        copy_link(&record.path, &scoped(dst, &record.relative)?)?;
    }
    Ok(())
}

pub fn revision(home: &Path) -> Result<String> {
    let mut state = profile_files(home)?;
    for file in profile_databases(home).flat_map(|n| [n.to_owned(), format!("{n}-wal")]) {
        if let Ok(m) = home.join(&file).metadata()
            && !(file.ends_with("-wal") && m.len() == 0)
        {
            state.insert(file, (m.len(), format!("{:?}", m.modified()?)));
        }
    }
    Ok(checksum(&serde_json::to_vec(&state)?))
}

fn profile_databases(home: &Path) -> impl Iterator<Item = &'static str> {
    codex::DATABASES.into_iter().chain(
        home.join(sidebar::SQLITE)
            .is_file()
            .then_some(sidebar::SQLITE),
    )
}

fn profile_files(home: &Path) -> Result<BTreeMap<String, (u64, String)>> {
    let mut state = BTreeMap::new();
    if let Ok(file) = home.join(sidebar::GLOBAL).metadata() {
        state.insert(
            sidebar::GLOBAL.into(),
            (file.len(), format!("{:?}", file.modified()?)),
        );
    }
    if home.join(sidebar::SQLITE).is_file() {
        state.insert(sidebar::SQLITE.into(), (0, String::new()));
    }
    for area in ["sessions", "archived_sessions"] {
        let root = home.join(area);
        if !root.exists() {
            continue;
        }
        for node in WalkDir::new(root).follow_links(false) {
            let p = node?.into_path();
            let m = fs::symlink_metadata(&p)?;
            if m.is_file()
                && !linked(&m)
                && p.to_string_lossy()
                    .trim_end_matches(".zst")
                    .ends_with(".jsonl")
            {
                state.insert(
                    p.strip_prefix(home)?.to_string_lossy().into_owned(),
                    (m.len(), format!("{:?}", m.modified()?)),
                );
            }
        }
    }
    Ok(state)
}

struct LiveGuard {
    databases: Vec<(Connection, i64, String)>,
    files: BTreeMap<String, (u64, String)>,
}

impl LiveGuard {
    fn capture(home: &Path) -> Result<Self> {
        let mut databases = Vec::new();
        for name in profile_databases(home) {
            let db = codex::ro(&home.join(name))?;
            let version = Self::version(&db)?;
            let contents = Self::contents(&db)?;
            databases.push((db, version, contents));
        }
        Ok(Self {
            databases,
            files: profile_files(home)?,
        })
    }

    fn version(db: &Connection) -> Result<i64> {
        Ok(db.query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    fn contents(db: &Connection) -> Result<String> {
        use rusqlite::types::ValueRef;
        use sha2::{Digest, Sha256};
        let quoted = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
        db.execute_batch("BEGIN")?;
        let result = (|| -> Result<String> {
            let mut digest = Sha256::new();
            let mut schema = db.prepare("SELECT name,type,coalesce(sql,'') FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name")?;
            let mut definitions = schema.query([])?;
            while let Some(definition) = definitions.next()? {
                let name: String = definition.get(0)?;
                let kind: String = definition.get(1)?;
                let sql: String = definition.get(2)?;
                for field in [&name, &kind, &sql] {
                    digest.update((field.len() as u64).to_be_bytes());
                    digest.update(field);
                }
                if kind != "table" {
                    continue;
                }
                let table = quoted(&name);
                let mut columns = db.prepare(&format!("PRAGMA table_info({table})"))?;
                let keys = columns
                    .query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
                let order = keys
                    .iter()
                    .filter(|(pk, _)| **pk > 0)
                    .map(|(_, name)| quoted(name))
                    .collect::<Vec<_>>();
                let order = if order.is_empty() {
                    "rowid".to_owned()
                } else {
                    order.join(",")
                };
                let mut query = db.prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))?;
                let fields = query.column_count();
                let mut rows = query.query([])?;
                while let Some(row) = rows.next()? {
                    digest.update([0xFF]);
                    for index in 0..fields {
                        match row.get_ref(index)? {
                            ValueRef::Null => digest.update([0]),
                            ValueRef::Integer(value) => {
                                digest.update([1]);
                                digest.update(value.to_be_bytes());
                            }
                            ValueRef::Real(value) => {
                                digest.update([2]);
                                digest.update(value.to_bits().to_be_bytes());
                            }
                            ValueRef::Text(value) | ValueRef::Blob(value) => {
                                digest.update([
                                    if matches!(row.get_ref(index)?, ValueRef::Text(_)) {
                                        3
                                    } else {
                                        4
                                    },
                                ]);
                                digest.update((value.len() as u64).to_be_bytes());
                                digest.update(value);
                            }
                        }
                    }
                }
            }
            Ok(format!("{:x}", digest.finalize()))
        })();
        db.execute_batch("ROLLBACK")?;
        result
    }

    fn check(&self, home: &Path, is_open: &impl Fn() -> Result<bool>) -> Result<()> {
        ensure!(
            !is_open()?,
            "Codex opened on the receiving computer. Close it to apply the verified backup."
        );
        for (db, previous, contents) in &self.databases {
            ensure!(
                Self::version(db)? == *previous || Self::contents(db)? == *contents,
                "The receiving chat database was edited during verification. Close Codex and retry."
            );
        }
        ensure!(
            profile_files(home)? == self.files,
            "A receiving transcript or the Codex sidebar changed. Close Codex and retry."
        );
        Ok(())
    }
}

fn entry(root: &Path, p: &Path) -> Result<Entry> {
    Ok(Entry {
        path: p.strip_prefix(root)?.to_string_lossy().replace('\\', "/"),
        bytes: p.metadata()?.len(),
        sha256: hash(p)?,
    })
}

fn project_name(path: &Path) -> Result<String> {
    let name = path
        .file_name()
        .context("Project folder has no name")?
        .to_string_lossy()
        .into_owned();
    scoped(Path::new(""), &name)?;
    Ok(name)
}

fn project_roots(db: &Path) -> Result<Vec<(String, i64, String)>> {
    let c = codex::ro(db)?;
    let mut s = c.prepare(
        "SELECT project_id,position,path FROM project_roots ORDER BY project_id,position",
    )?;
    Ok(s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

fn excluded(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        ".git",
        ".svn",
        ".hg",
        ".codex",
        ".ssh",
        ".aws",
        ".azure",
        ".kube",
        "node_modules",
        ".venv",
        "venv",
        "__pycache__",
        "vendor",
        "dist",
        "build",
        "target",
        "cache",
        "coverage",
        "bin",
        "obj",
        "data",
        "syntheticruns",
        "testruns",
    ]
    .contains(&n.as_str())
        || [".env", "credentials", "secret", "id_rsa", "id_ed25519"]
            .iter()
            .any(|prefix| n.starts_with(prefix))
        || ["auth.json", ".npmrc", ".pypirc", ".netrc"].contains(&n.as_str())
        || [
            ".pem",
            ".key",
            ".pfx",
            ".p12",
            ".ppk",
            ".sqlite",
            ".sqlite3",
            ".db",
            ".db-wal",
            ".db-shm",
            ".jks",
            ".keystore",
        ]
        .iter()
        .any(|ext| n.ends_with(ext))
}

pub fn scan_projects(home: &Path, cache: &Path, progress: &impl Fn(&str)) -> Result<Vec<Entry>> {
    let roots = project_roots(&home.join(codex::DATABASES[0]))?;
    let cache_index = cache.join("project-stamps.json");
    let old: HashMap<PathBuf, ProjectStamp> = if cache_index.is_file() {
        read(&cache_index)?
    } else {
        HashMap::new()
    };
    let mut stamps = HashMap::new();
    let mut seen = HashMap::<String, PathBuf>::new();
    let mut files = Vec::new();
    for (_, _, path) in roots {
        let folder = PathBuf::from(path);
        if !folder.is_dir() {
            continue;
        }
        let name = project_name(&folder)?;
        let root = fs::canonicalize(folder)?;
        ensure!(
            !root.parent().is_none()
                && !root
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains("\\.codex"),
            "A project points at a drive or ChatGPT profile"
        );
        if let Some(earlier) = seen.insert(name.to_ascii_lowercase(), root.clone()) {
            ensure!(
                earlier == root,
                "Two different projects use the folder name '{name}'"
            );
            continue;
        }
        progress(&format!("Preparing project · {name}"));
        for node in WalkDir::new(&root)
            .sort_by_file_name()
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || (!excluded(&e.file_name().to_string_lossy())
                        && fs::symlink_metadata(e.path()).is_ok_and(|m| !linked(&m)))
            })
        {
            let node = node?;
            if !node.file_type().is_file() {
                continue;
            }
            let file = node.path();
            let relative = format!(
                "{name}/{}",
                file.strip_prefix(&root)?
                    .to_string_lossy()
                    .replace('\\', "/")
            );
            let before = file.metadata()?;
            let bytes = before.len();
            let modified = before.modified()?;
            let sha256 = match old.get(file) {
                Some(stamp) if stamp.bytes == bytes && stamp.modified == modified => {
                    stamp.hash.clone()
                }
                _ => hash(file)?,
            };
            let after = file.metadata()?;
            ensure!(
                bytes == after.len() && modified == after.modified()?,
                "Project changed during backup: {relative}"
            );
            stamps.insert(
                file.to_path_buf(),
                ProjectStamp {
                    bytes,
                    modified,
                    hash: sha256.clone(),
                },
            );
            files.push(Entry {
                path: relative,
                bytes,
                sha256,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if stamps != old {
        write(&cache_index, &stamps)?;
    }
    Ok(files)
}

pub fn project_source(
    store: &Path,
    digest: &str,
    expected_size: Option<u64>,
) -> Result<Option<PathBuf>> {
    let index = store.join("project-stamps.json");
    if !index.is_file() {
        return Ok(None);
    }
    let stamps: HashMap<PathBuf, ProjectStamp> = read(&index)?;
    for (path, stamp) in stamps {
        if stamp.hash != digest || expected_size.is_some_and(|size| size != stamp.bytes) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_file()
            && !linked(&metadata)
            && metadata.len() == stamp.bytes
            && hash(&path)? == digest
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn asset_references(path: &Path, references: &Regex) -> Result<Vec<Asset>> {
    let mut assets = BTreeMap::new();
    let mut stream = input(path)?;
    let mut line = String::new();
    while stream.read_line(&mut line)? != 0 {
        if (line.contains("agent_message")
            || line.contains("user_message")
            || line.contains("response_item"))
            && (line.contains("](") || line.contains("path="))
        {
            let value = serde_json::from_str::<Value>(&line)?;
            collect_refs(&value["payload"], references, &mut assets);
        }
        line.clear();
    }
    Ok(assets.into_values().collect())
}

fn collect_assets(raw: &[Raw], files: &mut PublishFiles<'_>) -> Result<Vec<Asset>> {
    let references =
        Regex::new(r#"(?i)(?:\]\(<?|(?:path|file_path)=")/?([a-z]:[\\/][^>\)"\r\n]+)"#)?;
    let mut assets = BTreeMap::new();
    for record in raw {
        let relative = format!("VerifiedChatData/{}", record.relative);
        let digest = files.entries[&relative].sha256.clone();
        let found = match files.catalog.references.remove(&digest) {
            Some(found) => found,
            None => asset_references(&scoped(files.root, &relative)?, &references)?,
        };
        for asset in &found {
            assets
                .entry(asset.original.to_ascii_lowercase())
                .or_insert_with(|| asset.clone());
        }
        files.next_catalog.references.insert(digest, found);
    }
    for asset in assets.values_mut() {
        let p = Path::new(&asset.original);
        if p.to_string_lossy()
            .to_ascii_lowercase()
            .contains("\\.codex\\")
            || p.components()
                .any(|c| excluded(&c.as_os_str().to_string_lossy()))
        {
            continue;
        }
        if let Ok(metadata) = fs::symlink_metadata(p)
            && metadata.is_file()
            && !linked(&metadata)
        {
            let digest = files.digest(p, &metadata)?;
            let name = format!("{digest}.bin");
            files.stage(p, format!("AssetObjects/{name}"), digest.clone())?;
            asset.object = Some(name);
            asset.sha256 = Some(digest);
        }
    }
    Ok(assets.into_values().collect())
}

fn collect_refs(value: &Value, re: &Regex, assets: &mut BTreeMap<String, Asset>) {
    match value {
        Value::String(text) => {
            for m in re.captures_iter(text) {
                let original = m[1].replace('/', "\\");
                assets
                    .entry(original.to_ascii_lowercase())
                    .or_insert(Asset {
                        original,
                        object: None,
                        sha256: None,
                    });
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_refs(item, re, assets);
            }
        }
        Value::Object(fields) => {
            for item in fields.values() {
                collect_refs(item, re, assets);
            }
        }
        _ => {}
    }
}

pub fn publish(
    home: &Path,
    store: &Path,
    objects: Option<&Path>,
    source: &str,
    project_files: Vec<Entry>,
    progress: &impl Fn(&str),
) -> Result<Offer> {
    let app = codex::App::locate()?;
    let now = time::OffsetDateTime::now_utc().format(&time::macros::format_description!(
        "[year][month][day]-[hour][minute][second]"
    ))?;
    let tag = format!("{now}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let root = store.join("Snapshots").join(&tag);
    let data = root.join("VerifiedChatData");
    let catalog_path = objects.map(|dir| dir.join("chat-file-index-v1.json"));
    let catalog = catalog_path
        .as_deref()
        .and_then(|path| read(path).ok())
        .unwrap_or_default();
    let mut staged = PublishFiles {
        root: &root,
        objects,
        catalog,
        next_catalog: PublishCatalog::default(),
        entries: BTreeMap::new(),
    };
    progress("Checking local chats for changes");
    let guard = LiveGuard::capture(home)?;
    let raw = raw_files(home)?;
    copy_databases(home, &data)?;
    for record in &raw {
        let digest = staged.digest(&record.path, &fs::symlink_metadata(&record.path)?)?;
        staged.stage(
            &record.path,
            format!("VerifiedChatData/{}", record.relative),
            digest,
        )?;
    }
    remap_raw(&data, &raw)?;
    guard.check(home, &codex::is_open)?;
    let layout = sidebar::export(Some(home), &data.join(codex::DATABASES[0]))?;
    guard.check(home, &codex::is_open)?;
    drop(guard);
    let tokens = codex::read_profiles(&app, &data, None, progress)?;
    write(&root.join(sidebar::MANIFEST), &layout)?;
    write(&root.join("HISTORY.json"), &tokens)?;
    progress("Checking chat attachments");
    let assets = collect_assets(&raw, &mut staged)?;
    write(&root.join("ASSETS.json"), &assets)?;
    let count = project_files.len();
    let projects = Projects {
        files: project_files,
        ..Projects::default()
    };
    write(&root.join("PROJECTS.json"), &projects)?;
    let mut files = Vec::new();
    for item in WalkDir::new(&root).sort_by_file_name() {
        let item = item?;
        let name = item.file_name().to_string_lossy();
        if item.file_type().is_file()
            && !name.ends_with("-wal")
            && !name.ends_with("-shm")
            && item.path().strip_prefix(&data).ok().is_none_or(|p| {
                p.components()
                    .next()
                    .is_none_or(|c| !c.as_os_str().to_string_lossy().starts_with('.'))
            })
        {
            let under_data = item.path().strip_prefix(&data).ok();
            if let Some(p) = under_data {
                if !p.starts_with("sessions")
                    && !p.starts_with("archived_sessions")
                    && !codex::DATABASES.iter().any(|f| p == Path::new(f))
                {
                    continue;
                }
            }
            let relative = item
                .path()
                .strip_prefix(&root)?
                .to_string_lossy()
                .replace('\\', "/");
            files.push(match staged.entries.remove(&relative) {
                Some(staged) => staged,
                None => entry(&root, item.path())?,
            });
        }
    }
    let index = Index {
        snapshot: tag.clone(),
        source: source.into(),
        app_version: app.version,
        files,
        chats: tokens.len(),
        project_files: count,
    };
    write(&root.join("TRANSFER.json"), &index)?;
    let mut files = index
        .files
        .iter()
        .map(|r| Entry {
            path: format!("Snapshots/{tag}/{}", r.path),
            ..r.clone()
        })
        .collect::<Vec<_>>();
    files.push(entry(store, &root.join("TRANSFER.json"))?);
    let offer = Offer {
        source: source.into(),
        snapshot: tag,
        files,
    };
    offer.validate(source)?;
    if let Some(path) = catalog_path {
        write(&path, &staged.next_catalog)?;
    }
    Ok(offer)
}

fn scope(raw: &[Raw], ids: &HashSet<String>) -> HashSet<String> {
    let mut scope = ids.clone();
    loop {
        let size = scope.len();
        for row in raw {
            if scope.contains(&row.physical) || scope.contains(&row.owner) {
                scope.extend([row.physical.clone(), row.owner.clone()]);
                scope.extend(row.parent.clone());
            }
        }
        if scope.len() == size {
            return scope;
        }
    }
}

fn contains_history(haystack: &[String], needle: &[String]) -> bool {
    let mut remaining = needle.iter();
    let mut next = remaining.next();
    for token in haystack {
        if next == Some(token) {
            next = remaining.next();
        }
    }
    next.is_none()
}

fn select_incoming(existing: &[String], incoming: &[String], match_sender: bool) -> bool {
    if match_sender {
        return existing != incoming;
    }
    !contains_history(existing, incoming) && contains_history(incoming, existing)
}

fn chat_changes(
    source: &BTreeMap<String, Vec<String>>,
    local: &BTreeMap<String, Vec<String>>,
    source_raw: &[Raw],
    local_raw: &[Raw],
    unchanged_ancestors: &HashSet<String>,
    match_sender: bool,
) -> (HashSet<String>, HashSet<String>) {
    let mut selected = HashSet::new();
    let mut review = HashSet::new();
    for (id, incoming) in source {
        match local.get(id) {
            None => {
                selected.insert(id.clone());
            }
            Some(existing) if select_incoming(existing, incoming, match_sender) => {
                selected.insert(id.clone());
            }
            Some(existing) if contains_history(existing, incoming) => {}
            Some(_) => {
                review.insert(id.clone());
            }
        }
    }
    if match_sender {
        // Verify shared forks with their sender's parent whenever a parent changes.
        loop {
            let lineage = scope(source_raw, &selected);
            let related: Vec<_> = source
                .keys()
                .filter(|id| {
                    !selected.contains(*id)
                        && !unchanged_ancestors.contains(*id)
                        && (lineage.contains(*id)
                            || !scope(source_raw, &HashSet::from([(*id).clone()]))
                                .difference(unchanged_ancestors)
                                .all(|part| !lineage.contains(part)))
                })
                .cloned()
                .collect();
            if related.is_empty() {
                break;
            }
            selected.extend(related);
        }
    }
    loop {
        let preserved: HashSet<_> = local
            .keys()
            .filter(|id| !selected.contains(*id))
            .cloned()
            .collect();
        let blocked = selected
            .iter()
            .filter(|id| {
                let mut lineage = scope(source_raw, &HashSet::from([(*id).clone()]));
                lineage.retain(|id| !unchanged_ancestors.contains(id));
                !lineage.is_disjoint(&preserved)
                    || local_raw.iter().any(|r| {
                        preserved.contains(&r.owner)
                            && r.parent.as_ref().is_some_and(|p| lineage.contains(p))
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        if blocked.is_empty() {
            return (selected, review);
        }
        for id in blocked {
            selected.remove(&id);
            review.insert(id);
        }
    }
}

fn unchanged_ancestors(
    source: &BTreeMap<String, Vec<String>>,
    local: &BTreeMap<String, Vec<String>>,
    source_raw: &[Raw],
    local_raw: &[Raw],
    match_sender: bool,
) -> Result<HashSet<String>> {
    let changes = source
        .iter()
        .filter(|(id, incoming)| {
            local
                .get(*id)
                .is_none_or(|old| select_incoming(old, incoming, match_sender))
        })
        .map(|(id, _)| id.clone())
        .collect::<HashSet<_>>();
    let relevant = scope(source_raw, &changes);
    let mut unchanged = HashSet::new();
    for id in relevant.intersection(&local.keys().cloned().collect()) {
        if source.get(id) != local.get(id) {
            continue;
        }
        let files = |raw: &[Raw]| {
            raw.iter()
                .filter(|r| &r.owner == id || &r.physical == id)
                .cloned()
                .collect::<Vec<_>>()
        };
        let incoming = files(source_raw);
        let existing = files(local_raw);
        if incoming.is_empty() || incoming.len() != existing.len() {
            continue;
        }
        let mut identical = true;
        for (a, b) in incoming.iter().zip(&existing) {
            if a.relative != b.relative || hash(&a.path)? != hash(&b.path)? {
                identical = false;
                break;
            }
        }
        if identical {
            unchanged.insert(id.clone());
            unchanged.extend(
                incoming
                    .into_iter()
                    .filter(|r| !changes.contains(&r.physical))
                    .map(|r| r.physical),
            );
        }
    }
    Ok(unchanged)
}

fn columns(c: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut cmd = c.prepare(&format!("PRAGMA {schema}.table_info({table})"))?;
    Ok(cmd
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?)
}

fn ensure_schema(c: &Connection, left: &str, right: &str, table: &str) -> Result<Vec<String>> {
    let a = columns(c, left, table)?;
    let b = columns(c, right, table)?;
    ensure!(
        !a.is_empty() && a == b,
        "The two ChatGPT profiles use different {table} layouts. Update ChatGPT on both computers."
    );
    ensure!(
        a.iter()
            .all(|x| x.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')),
        "Unexpected ChatGPT database column"
    );
    Ok(a)
}

fn merge(
    candidate: &Path,
    incoming: &Path,
    remote_raw: &[Raw],
    remote_ids: &HashSet<String>,
    all_ids: &HashSet<String>,
    workspaces: &Path,
    projects: &Path,
) -> Result<()> {
    let mut connection = codex::connect(&candidate.join(codex::DATABASES[0]))?;
    connection.execute(
        "ATTACH DATABASE ? AS incoming",
        [windows_path(&incoming.join(codex::DATABASES[0]))],
    )?;
    connection.execute(
        "ATTACH DATABASE ? AS hist",
        [windows_path(&candidate.join(codex::DATABASES[1]))],
    )?;
    connection.execute(
        "ATTACH DATABASE ? AS incoming_hist",
        [windows_path(&incoming.join(codex::DATABASES[1]))],
    )?;
    let mut preserved = HashMap::new();
    let cols = ensure_schema(&connection, "main", "incoming", "threads")?;
    let pinned = [
        "id",
        "cwd",
        "title",
        "name",
        "archived",
        "archived_at",
        "project_id",
        "thread_section_id",
        "is_pinned",
        "section_position",
    ];
    let fields = cols
        .iter()
        .filter(|col| !pinned.contains(&col.as_str()))
        .map(|col| format!("{col}=excluded.{col}"))
        .collect::<Vec<_>>()
        .join(",");
    for (id, _, _) in codex::threads(&connection)? {
        if remote_ids.contains(&id) {
            let cwd: String =
                connection.query_row("SELECT cwd FROM threads WHERE id=?", [&id], |r| r.get(0))?;
            preserved.insert(id, cwd);
        }
    }
    let local_roots = project_roots(&candidate.join(codex::DATABASES[0]))?;
    let incoming_roots = project_roots(&incoming.join(codex::DATABASES[0]))?;
    let tx = connection.transaction()?;
    tx.execute_batch("CREATE TEMP TABLE ids(id TEXT PRIMARY KEY); CREATE TEMP TABLE physical(id TEXT PRIMARY KEY);")?;
    for id in remote_ids {
        tx.execute("INSERT INTO ids VALUES(?)", [id])?;
    }
    for id in all_ids {
        tx.execute("INSERT INTO physical VALUES(?)", [id])?;
    }
    for table in ["projects", "project_roots", "thread_sections"] {
        ensure_schema(&tx, "main", "incoming", table)?;
        if table != "project_roots" {
            tx.execute(
                &format!("INSERT OR IGNORE INTO {table} SELECT * FROM incoming.{table}"),
                [],
            )?;
        }
    }
    let mut roots = HashMap::new();
    for (id, _, path) in local_roots {
        roots.entry(id).or_insert(path);
    }
    let existing = roots.keys().cloned().collect::<HashSet<_>>();
    for (id, position, path) in incoming_roots {
        if existing.contains(&id) {
            continue;
        }
        let folder = scoped(projects, &project_name(Path::new(&path))?)?;
        fs::create_dir_all(&folder)?;
        let path = windows_path(&folder);
        tx.execute(
            "INSERT OR IGNORE INTO project_roots(project_id,position,path) VALUES(?,?,?)",
            params![id, position, path],
        )?;
        roots.entry(id).or_insert(path);
    }
    tx.execute(&format!("INSERT INTO threads SELECT * FROM incoming.threads WHERE id IN (SELECT id FROM ids) ON CONFLICT(id) DO UPDATE SET {fields}"),[])?;
    for (id, _, previous) in codex::threads(&tx)?
        .into_iter()
        .filter(|r| remote_ids.contains(&r.0))
    {
        let key = remote_raw
            .iter()
            .find(|row| {
                codex::logical_path(&previous)
                    .ends_with(&format!("/{}", codex::logical_path(&row.relative)))
            })
            .with_context(|| format!("Incoming transcript is missing for chat {id}"))?;
        let project: Option<String> =
            tx.query_row("SELECT project_id FROM threads WHERE id=?", [&id], |row| {
                row.get(0)
            })?;
        let cwd = preserved
            .get(&id)
            .cloned()
            .or_else(|| project.as_ref().and_then(|p| roots.get(p)).cloned())
            .unwrap_or_else(|| windows_path(&workspaces.join(&id)));
        fs::create_dir_all(scoped(workspaces, &id)?)?;
        let name = windows_path(&candidate.join(key.relative.trim_end_matches(".zst")));
        tx.execute(
            "UPDATE threads SET cwd=?,rollout_path=? WHERE id=?",
            params![cwd, name, id],
        )?;
    }
    for table in HISTORY {
        ensure_schema(&tx, "hist", "incoming_hist", table)?;
        tx.execute(
            &format!("DELETE FROM hist.{table} WHERE thread_id IN (SELECT id FROM physical)"),
            [],
        )?;
        tx.execute(&format!("INSERT INTO hist.{table} SELECT * FROM incoming_hist.{table} WHERE thread_id IN (SELECT id FROM physical)"),[])?;
    }
    for (table, key) in EXTRA {
        ensure_schema(&tx, "main", "incoming", table)?;
        tx.execute(&format!("INSERT OR IGNORE INTO {table} SELECT * FROM incoming.{table} WHERE {key} IN (SELECT id FROM ids)"),[])?;
    }
    tx.commit()?;
    for sql in ["PRAGMA main.quick_check", "PRAGMA hist.quick_check"] {
        let check: String = connection.query_row(sql, [], |r| r.get(0))?;
        ensure!(
            check == "ok",
            "Prepared ChatGPT database failed its integrity check"
        );
    }
    Ok(())
}

pub fn project_destination(home: &Path) -> Result<PathBuf> {
    Ok(home
        .parent()
        .context("No user folder")?
        .join("Documents/Desktop-Projects-Initial"))
}

type ProjectFetch<'a> = &'a (dyn Fn(&Path, Option<&Entry>) -> Result<()> + Sync);

struct ProjectFolders {
    fallback: PathBuf,
    registered: HashMap<String, PathBuf>,
}

impl ProjectFolders {
    fn for_profile(home: &Path, destination: Option<&Path>) -> Result<Self> {
        let fallback = match destination {
            Some(path) => path.to_path_buf(),
            None => project_destination(home)?,
        };
        ensure!(
            fallback.is_absolute() && fallback.parent().is_some(),
            "Choose a project folder inside a drive"
        );
        if fallback.exists() {
            let resolved = fallback.canonicalize()?;
            let profile = home.canonicalize()?;
            ensure!(
                resolved.is_dir()
                    && !resolved.starts_with(&profile)
                    && !profile.starts_with(&resolved),
                "Choose a folder apart from the Codex profile"
            );
        }
        let mut registered = HashMap::new();
        for (_, _, path) in project_roots(&home.join(codex::DATABASES[0]))? {
            let folder = PathBuf::from(path);
            if !folder.is_dir() {
                continue;
            }
            let name = project_name(&folder)?.to_ascii_lowercase();
            let resolved = folder.canonicalize()?;
            if resolved.parent().is_none()
                || home.starts_with(&resolved)
                || resolved.starts_with(home)
            {
                continue;
            }
            registered.entry(name).or_insert(resolved);
        }
        Ok(Self {
            fallback,
            registered,
        })
    }
}

fn project_target(
    row: &Entry,
    folders: &ProjectFolders,
    received: &Path,
) -> Result<Option<(PathBuf, bool)>> {
    let relative = row.path.replace('\\', "/");
    let (project, file) = relative
        .split_once('/')
        .context("Project file has no project folder")?;
    let (root, path) = match folders.registered.get(&project.to_ascii_lowercase()) {
        Some(root) => (root, scoped(root, file)?),
        None => (&folders.fallback, scoped(&folders.fallback, &relative)?),
    };
    if path.is_file() && path.metadata()?.len() == row.bytes && hash(&path)? == row.sha256 {
        return Ok(None);
    }
    if path.exists()
        || path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(root))
            .any(Path::is_file)
    {
        ensure!(is_hash(&row.sha256), "Invalid project checksum");
        let version = scoped(
            &received.join("Project conflicts").join(&row.sha256),
            &row.path,
        )?;
        if version.is_file()
            && version.metadata()?.len() == row.bytes
            && hash(&version)? == row.sha256
        {
            return Ok(None);
        }
        return Ok(Some((version, true)));
    }
    Ok(Some((path, false)))
}

fn add_project_files(
    root: &Path,
    tag: &str,
    data: &Path,
    target: &ProjectFolders,
    received: &Path,
    fetch: ProjectFetch<'_>,
    progress: &(impl Fn(&str) + Sync),
) -> Result<(usize, usize)> {
    let projects: Projects = read(&data.join("PROJECTS.json"))?;
    let mut conflicts = 0;
    let mut added = 0;
    for archive in projects.archives {
        progress("Copying project files · preserving local edits");
        let name = scoped(&root.join("ProjectArchives").join(tag), &archive.name)?;
        let mut zip = None;
        for row in archive.files {
            let Some((path, conflict)) = project_target(&row, target, received)? else {
                continue;
            };
            if zip.is_none() {
                fetch(&name, None)?;
                zip = Some(zip::ZipArchive::new(fs::File::open(&name)?)?);
            }
            let mut file = zip.as_mut().unwrap().by_name(&row.path)?;
            ensure!(
                file.size() == row.bytes,
                "A project file does not match its backup index"
            );
            verified_stream(&mut file, &path, row.bytes, &row.sha256)?;
            conflicts += usize::from(conflict);
            added += usize::from(!conflict);
        }
    }
    let mut paths = HashSet::new();
    ensure!(
        projects
            .files
            .iter()
            .all(|row| paths.insert(row.path.replace('\\', "/").to_ascii_lowercase())),
        "Duplicate project file path in backup"
    );
    let mut planned = Vec::new();
    let total = projects.files.len();
    for (index, row) in projects.files.iter().enumerate() {
        if let Some((path, conflict)) = project_target(row, target, received)? {
            planned.push((row, path, conflict));
        }
        if index == 0 || (index + 1) % 25 == 0 || index + 1 == total {
            progress(&format!(
                "Checking existing project files · {}/{}",
                index + 1,
                total
            ));
        }
    }
    let total = planned.len();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let stopped = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|workers| -> Result<()> {
        let mut handles = Vec::new();
        for _ in 0..total.min(6) {
            let files = &planned;
            let (next, done, stopped) = (&next, &done, &stopped);
            handles.push(workers.spawn(move || -> Result<(usize, usize)> {
                let process = || -> Result<(usize, usize)> {
                    let mut added = 0;
                    let mut conflicts = 0;
                    while !stopped.load(Ordering::Relaxed) {
                        let Some((row, path, conflict)) =
                            files.get(next.fetch_add(1, Ordering::Relaxed))
                        else {
                            break;
                        };
                        if project_target(row, target, received)?.as_ref()
                            == Some(&(path.clone(), *conflict))
                        {
                            fetch(path, Some(row))?;
                            ensure!(
                                path.is_file()
                                    && path.metadata()?.len() == row.bytes
                                    && hash(&path)? == row.sha256,
                                "Project file did not pass verification"
                            );
                            conflicts += usize::from(*conflict);
                            added += usize::from(!*conflict);
                        } else {
                            anyhow::bail!(
                                "Project files changed during transfer. Start sync again."
                            );
                        }
                        let checked = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if checked == 1 || checked % 25 == 0 || checked == total {
                            progress(&format!("Copying needed project files · {checked}/{total}"));
                        }
                    }
                    Ok((added, conflicts))
                };
                let result = process();
                if result.is_err() {
                    stopped.store(true, Ordering::Relaxed);
                }
                result
            }));
        }
        for handle in handles {
            let (new, different) = handle
                .join()
                .map_err(|_| anyhow::anyhow!("A project file worker stopped unexpectedly"))??;
            added += new;
            conflicts += different;
        }
        Ok(())
    })?;
    Ok((added, conflicts))
}

fn add_assets(data: &Path, received: &Path, fetch: ProjectFetch<'_>) -> Result<usize> {
    let assets: Vec<Asset> = read(&data.join("ASSETS.json"))?;
    let mut count = 0;
    for asset in assets {
        let (Some(object), Some(digest)) = (asset.object, asset.sha256) else {
            continue;
        };
        ensure!(is_hash(&digest), "Invalid attachment checksum");
        let source = scoped(&data.join("AssetObjects"), &object)?;
        let original = PathBuf::from(asset.original);
        let name = original
            .file_name()
            .and_then(|p| p.to_str())
            .unwrap_or("attachment");
        let out = scoped(&received.join("Attachments").join(&digest), name)?;
        if out.is_file() && hash(&out)? == digest {
            continue;
        }
        fetch(&source, None)?;
        let mut input = fs::File::open(&source)?;
        verified_stream(&mut input, &out, source.metadata()?.len(), &digest)?;
        count += 1;
    }
    Ok(count)
}

#[derive(Clone, Copy, Default)]
pub struct RestoreOptions<'a> {
    pub match_sender: bool,
    pub projects_folder: Option<&'a Path>,
}

pub fn apply(
    download: &Path,
    home: &Path,
    backup_base: &Path,
    source: &str,
    options: RestoreOptions<'_>,
    fetch: ProjectFetch<'_>,
    progress: &(impl Fn(&str) + Sync),
) -> Result<String> {
    apply_checked(
        download,
        home,
        backup_base,
        source,
        options,
        fetch,
        progress,
        codex::is_open,
    )
}

pub fn confirmed(
    download: &Path,
    home: &Path,
    backup_base: &Path,
    snapshot: &str,
    options: RestoreOptions<'_>,
) -> Result<Option<String>> {
    Ok(
        cached_receipt(download, home, backup_base, snapshot, "DONE.json", options)?
            .and_then(|receipt| receipt["summary"].as_str().map(str::to_owned)),
    )
}

fn cached_receipt(
    download: &Path,
    home: &Path,
    backup_base: &Path,
    snapshot: &str,
    file: &str,
    options: RestoreOptions<'_>,
) -> Result<Option<Value>> {
    let receipt = verified_receipt(download, home, backup_base, snapshot, file)?;
    let Some(receipt) = receipt else {
        return Ok(None);
    };
    let destination = options.projects_folder.map(windows_path);
    if receipt["match_sender"].as_bool() != Some(options.match_sender)
        || receipt["projects_folder"].as_str() != destination.as_deref()
        || receipt["installed_revision"].as_str() != Some(revision(home)?.as_str())
    {
        return Ok(None);
    }
    Ok(Some(receipt))
}

pub fn outcome(
    download: &Path,
    home: &Path,
    backup_base: &Path,
    snapshot: &str,
) -> Result<Outcome> {
    let receipt = verified_receipt(download, home, backup_base, snapshot, "DONE.json")?
        .context("The receiving ChatGPT profile did not verify")?;
    Ok(Outcome {
        chat_conflicts: receipt["chat_conflicts"].as_u64().unwrap_or_default() as usize,
        project_conflicts: receipt["project_conflicts"].as_u64().unwrap_or_default() as usize,
        sidebar_verified: receipt["sidebar_verified"].as_bool().unwrap_or(false),
    })
}

fn verified_receipt(
    download: &Path,
    home: &Path,
    backup_base: &Path,
    snapshot: &str,
    file: &str,
) -> Result<Option<Value>> {
    let ready = scoped(&backup_base.join("Received"), snapshot)?.join(file);
    let root = scoped(&download.join("Snapshots"), snapshot)?;
    let data = root.join("VerifiedChatData").join(codex::DATABASES[0]);
    if !ready.is_file() || !data.is_file() {
        return Ok(None);
    }
    let receipt: Value = read(&ready)?;
    if receipt["profile"].as_str() != Some(windows_path(home).as_str()) {
        return Ok(None);
    }
    let known = codex::threads(&codex::ro(&home.join(codex::DATABASES[0]))?)?
        .into_iter()
        .map(|row| row.0)
        .collect::<HashSet<_>>();
    let required = codex::threads(&codex::ro(&data)?)?;
    if !required.iter().all(|row| {
        known.contains(&row.0)
            || receipt["reviewed_ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(&row.0)))
    }) {
        return Ok(None);
    }
    if receipt["sidebar_verified"] == true {
        let manifest = sidebar::MANIFESTS
            .into_iter()
            .find(|name| root.join(name).is_file());
        let Some(manifest) = manifest else {
            return Ok(None);
        };
        let layout = read(&root.join(manifest))?;
        let ids = required.into_iter().map(|r| r.0).collect();
        if sidebar::verify(&layout, home, &home.join(codex::DATABASES[0]), &ids).is_err() {
            return Ok(None);
        }
    }
    Ok(Some(receipt))
}

fn finish_files(
    download: &Path,
    home: &Path,
    backup_base: &Path,
    root: &Path,
    snapshot: &str,
    mut receipt: Value,
    options: RestoreOptions<'_>,
    fetch: ProjectFetch<'_>,
    progress: &(impl Fn(&str) + Sync),
) -> Result<String> {
    progress("Chats and sidebar verified · copying project files");
    let (added, conflicts) = add_project_files(
        download,
        snapshot,
        root,
        &ProjectFolders::for_profile(home, options.projects_folder)?,
        &backup_base.join("Received"),
        fetch,
        progress,
    )?;
    progress("Chats and sidebar verified · copying attachments");
    let assets = add_assets(root, &backup_base.join("Received"), fetch)?;
    let mut summary = receipt["summary"]
        .as_str()
        .unwrap_or("Chats and sidebar verified")
        .to_owned();
    if added != 0 {
        summary.push_str(&format!(" · {added} project files added"));
    }
    if conflicts != 0 {
        summary.push_str(&format!(
            " · {conflicts} differing project files saved separately"
        ));
    }
    receipt["summary"] = json!(summary);
    receipt["attachments"] = json!(assets);
    receipt["project_files_added"] = json!(added);
    receipt["project_conflicts"] = json!(conflicts);
    write(
        &scoped(&backup_base.join("Received"), snapshot)?.join("DONE.json"),
        &receipt,
    )?;
    Ok(summary)
}

fn checkpoint(directory: &Path) -> Result<()> {
    for name in profile_databases(directory) {
        checkpoint_database(&directory.join(name))?;
    }
    Ok(())
}

fn checkpoint_database(path: &Path) -> Result<()> {
    let db = codex::connect(path)?;
    let (busy, _, _): (i64, i64, i64) =
        db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    ensure!(busy == 0, "ChatGPT is still using its database");
    let mode: String = db.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
    ensure!(
        mode.eq_ignore_ascii_case("delete"),
        "ChatGPT's database could not be closed safely"
    );
    Ok(())
}

fn apply_checked(
    download: &Path,
    home: &Path,
    backup_base: &Path,
    source: &str,
    options: RestoreOptions<'_>,
    fetch: ProjectFetch<'_>,
    progress: &(impl Fn(&str) + Sync),
    is_open: impl Fn() -> Result<bool>,
) -> Result<String> {
    let ptr: Pointer = read(&download.join("CURRENT.json"))?;
    let root = scoped(&download.join("Snapshots"), &ptr.snapshot)?;
    ensure!(
        hash(&root.join("TRANSFER.json"))? == ptr.manifest_sha256,
        "Backup index did not pass verification"
    );
    let index: Index = read(&root.join("TRANSFER.json"))?;
    ensure!(
        index.source == source && index.snapshot == ptr.snapshot,
        "This backup is for the other computer"
    );
    let received = backup_base.join("Received").join(&index.snapshot);
    if let Some(summary) = confirmed(download, home, backup_base, &index.snapshot, options)? {
        return Ok(summary);
    }
    if let Some(receipt) = cached_receipt(
        download,
        home,
        backup_base,
        &index.snapshot,
        "CHATS-DONE.json",
        options,
    )? {
        return finish_files(
            download,
            home,
            backup_base,
            &root,
            &index.snapshot,
            receipt,
            options,
            fetch,
            progress,
        );
    }
    ensure!(
        !is_open()?,
        "Close ChatGPT completely, including its tray icon, then select Sync now."
    );
    let journal = backup_base.join("INSTALL-IN-PROGRESS.json");
    ensure!(
        !journal.exists(),
        "A previous restore was interrupted. The original files are recorded in {}",
        journal.display()
    );
    let app = codex::App::locate()?;
    let version = |name: &str| {
        name.split('.')
            .filter_map(|n| n.parse::<u32>().ok())
            .collect::<Vec<_>>()
    };
    ensure!(
        version(&app.version) >= version(&index.app_version),
        "Update ChatGPT on this computer before receiving this backup"
    );
    for row in index
        .files
        .iter()
        .filter(|row| !row.path.starts_with("AssetObjects/"))
    {
        let file = scoped(&root, &row.path)?;
        ensure!(
            file.metadata()?.len() == row.bytes && hash(&file)? == row.sha256,
            "A backup file did not pass verification: {}",
            row.path
        );
    }
    let data = root.join("VerifiedChatData");
    let guard = LiveGuard::capture(home)?;
    let local_raw = raw_files(home)?;
    let remote_raw = raw_files(&data)?;
    let recovery = backup_base.join("Recovery");
    fs::create_dir_all(&recovery)?;
    let recovery_dir = tempfile::Builder::new()
        .prefix("sync-")
        .tempdir_in(recovery)?;
    let base = recovery_dir.path().to_path_buf();
    let original = base.join("original");
    let work = tempfile::tempdir_in(&base)?;
    let candidate = work.path().join("candidate");
    let incoming = work.path().join("incoming");
    progress("Comparing incoming and local chats");
    copy_databases(&data, &incoming)?;
    let catalog = root.join("HISTORY.json");
    let source_tokens: BTreeMap<String, Vec<String>> =
        if index.files.iter().any(|row| row.path == "HISTORY.json") {
            read(&catalog)?
        } else {
            link_raw(&remote_raw, &incoming)?;
            remap_raw(&incoming, &remote_raw)?;
            codex::read_profiles(&app, &incoming, None, progress)?
        };
    ensure!(
        source_tokens.len() == index.chats,
        "An incoming chat is missing"
    );
    let registered = codex::threads(&codex::ro(&home.join(codex::DATABASES[0]))?)?;
    let shared = registered
        .iter()
        .map(|r| r.0.clone())
        .filter(|id| source_tokens.contains_key(id))
        .collect::<HashSet<_>>();
    profile(home, &local_raw, &original)?;
    let local_tokens = codex::read_profiles(&app, &original, Some(&shared), progress)?;
    let mut known = local_tokens.clone();
    for (id, _, _) in &registered {
        known.entry(id.clone()).or_default();
    }
    let unchanged = unchanged_ancestors(
        &source_tokens,
        &known,
        &remote_raw,
        &local_raw,
        options.match_sender,
    )?;
    let (remote_ids, review) = chat_changes(
        &source_tokens,
        &known,
        &remote_raw,
        &local_raw,
        &unchanged,
        options.match_sender,
    );
    let project_folders = ProjectFolders::for_profile(home, options.projects_folder)?;
    let manifest = sidebar::MANIFESTS
        .into_iter()
        .find(|name| index.files.iter().any(|row| row.path == *name));
    let has_sidebar = manifest.is_some();
    let layout = if let Some(manifest) = manifest {
        read(&root.join(manifest))?
    } else {
        sidebar::export(None, &incoming.join(codex::DATABASES[0]))?
    };
    if !review.is_empty() {
        write(
            &received.join("Chats to review.json"),
            &json!({
                "incoming_backup": root, "chat_ids": review
            }),
        )?;
    }
    let previous_versions: BTreeMap<_, _> = remote_ids
        .iter()
        .filter_map(|id| {
            let old = local_tokens.get(id)?;
            (!contains_history(&source_tokens[id], old)).then_some((id, old))
        })
        .collect();
    let mut summary = format!(
        "{}{}",
        if remote_ids.is_empty() && !review.is_empty() {
            "No chats updated".into()
        } else if remote_ids.is_empty() {
            "Chats are up to date".into()
        } else {
            format!("{} chats updated", remote_ids.len())
        },
        if review.is_empty() {
            String::new()
        } else {
            format!(" · {} chats kept separately", review.len())
        }
    );
    if !previous_versions.is_empty() {
        summary.push_str(&format!(
            " · {} previous chat versions backed up",
            previous_versions.len()
        ));
    }
    let mut receipt = json!({"chats":remote_ids.len(),"chat_conflicts":review.len(),"reviewed_ids":review,"backup":base,"profile":windows_path(home),"summary":summary,"sidebar_verified":has_sidebar,"match_sender":options.match_sender,"projects_folder":options.projects_folder.map(windows_path)});
    if remote_ids.is_empty() && layout.is_empty() {
        guard.check(home, &is_open)?;
        receipt["backup"] = Value::Null;
        receipt["installed_revision"] = json!(revision(home)?);
        write(&received.join("CHATS-DONE.json"), &receipt)?;
        return finish_files(
            download,
            home,
            backup_base,
            &root,
            &index.snapshot,
            receipt,
            options,
            fetch,
            progress,
        );
    }
    let mut all_ids = scope(&remote_raw, &remote_ids);
    all_ids.retain(|id| !unchanged.contains(id));
    let remote_raw = remote_raw
        .into_iter()
        .filter(|row| all_ids.contains(&row.physical) || all_ids.contains(&row.owner))
        .collect::<Vec<_>>();
    let local_only: HashSet<_> = known
        .keys()
        .filter(|id| !remote_ids.contains(*id))
        .cloned()
        .collect();
    ensure!(
        local_only.is_disjoint(&all_ids),
        "A separate local chat shares a protected history file. Both copies are preserved."
    );
    ensure!(
        !local_raw.iter().any(|r| local_only.contains(&r.owner)
            && r.parent.as_ref().is_some_and(|p| all_ids.contains(p))),
        "A separate local chat was forked from an incoming history file. Both copies are preserved."
    );
    let kept = local_raw
        .iter()
        .filter(|r| !all_ids.contains(&r.physical) && !all_ids.contains(&r.owner))
        .cloned()
        .map(|mut row| {
            row.path = scoped(&original, &row.relative)?;
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    copy_databases(&original, &candidate)?;
    link_raw(&kept, &candidate)?;
    remap_raw(&candidate, &kept)?;
    link_raw(&remote_raw, &candidate)?;
    merge(
        &candidate,
        &incoming,
        &remote_raw,
        &remote_ids,
        &all_ids,
        &received.join("Workspaces"),
        &project_folders.fallback,
    )?;
    let before_sidebar = sidebar::state(home)?;
    let source_ids = source_tokens.keys().cloned().collect::<HashSet<_>>();
    let merged_sidebar = sidebar::apply(
        &layout,
        home,
        &mut codex::connect(&candidate.join(codex::DATABASES[0]))?,
        before_sidebar.clone(),
        &source_ids,
    )?;
    let mut protected = local_tokens
        .iter()
        .filter(|(id, _)| local_only.contains(*id))
        .map(|(id, history)| (id.clone(), history.clone()))
        .collect::<BTreeMap<_, _>>();
    let missing = local_only
        .iter()
        .filter(|id| !protected.contains_key(*id))
        .cloned()
        .collect::<HashSet<_>>();
    if !missing.is_empty() {
        protected.extend(codex::read_profiles(
            &app,
            &original,
            Some(&missing),
            progress,
        )?);
    }
    let candidate_tokens = codex::read_profiles(&app, &candidate, None, progress)?;
    ensure!(
        candidate_tokens.len() == remote_ids.len() + local_only.len(),
        "Prepared chat list did not match the verified backups"
    );
    for (id, expected) in source_tokens
        .iter()
        .filter(|(id, _)| remote_ids.contains(*id) || options.match_sender && !review.contains(*id))
        .chain(protected.iter())
    {
        ensure!(
            candidate_tokens.get(id) == Some(expected),
            "Prepared chats did not match their original histories. No chats were changed."
        );
    }
    let mut verified_previous = HashSet::new();
    if !previous_versions.is_empty() {
        let ids = previous_versions
            .keys()
            .map(|id| (*id).clone())
            .collect::<HashSet<_>>();
        let recovered = codex::read_profiles(&app, &original, Some(&ids), progress)?;
        ensure!(
            previous_versions
                .iter()
                .all(|(id, history)| recovered.get(*id) == Some(*history)),
            "The previous chat versions could not be verified in recovery"
        );
        verified_previous = ids;
    }
    guard.check(home, &is_open)?;
    // Original SQLite files are preserved with the live paths for rollback.
    let rollback = base.join("rollback");
    for file in codex::DATABASES {
        codex::snapshot(&home.join(file), &rollback.join(file))?;
    }
    let sqlite_sidebar = home.join(sidebar::SQLITE).is_file();
    let sidebar_name = if sqlite_sidebar {
        sidebar::SQLITE
    } else {
        sidebar::GLOBAL
    };
    let sidebar_file = home.join(sidebar_name);
    let sidebar_backup = rollback.join(sidebar_name);
    let sidebar_candidate = candidate.join(sidebar_name);
    if merged_sidebar != before_sidebar {
        if sqlite_sidebar {
            codex::snapshot(&sidebar_file, &sidebar_backup)?;
            codex::snapshot(&sidebar_backup, &sidebar_candidate)?;
            sidebar::write_settings(&sidebar_candidate, &before_sidebar, &merged_sidebar)?;
        } else {
            if sidebar_file.is_file() {
                fs::copy(&sidebar_file, &sidebar_backup)?;
            }
            write(&sidebar_candidate, &merged_sidebar)?;
        }
    }
    let touched = local_raw
        .iter()
        .filter(|r| all_ids.contains(&r.physical) || all_ids.contains(&r.owner))
        .cloned()
        .collect::<Vec<_>>();
    let old_paths = touched
        .iter()
        .map(|r| r.relative.clone())
        .collect::<HashSet<_>>();
    let new_paths = remote_raw
        .iter()
        .map(|r| r.relative.clone())
        .collect::<HashSet<_>>();
    ensure!(
        new_paths
            .iter()
            .all(|p| old_paths.contains(p) || !home.join(p).exists()),
        "An existing unrelated transcript is in the way"
    );
    let mut database = codex::connect(&candidate.join(codex::DATABASES[0]))?;
    let batch = database.transaction()?;
    for (id, _, path) in codex::threads(&batch)? {
        let location = PathBuf::from(path);
        let relative = location
            .strip_prefix(windows_path(&candidate))
            .or_else(|_| location.strip_prefix(&candidate))
            .context("Cannot map a verified conversation to this computer")?;
        batch.execute(
            "UPDATE threads SET rollout_path=? WHERE id=?",
            params![windows_path(&home.join(relative)), id],
        )?;
    }
    batch.commit()?;
    drop(database);
    for directory in [&rollback, &candidate] {
        checkpoint(directory)?;
    }
    guard.check(home, &is_open)?;
    drop(guard);
    checkpoint(home)?;
    let closed_revision = revision(home)?;
    // Keep the recovery folder before any operation can change the live profile.
    let _ = recovery_dir.keep();
    if !verified_previous.is_empty() {
        write(
            &received.join("Previous chat versions.json"),
            &json!({"profile":original,"chat_ids":verified_previous}),
        )?;
    }
    write(
        &journal,
        &json!({"original":original,"database_backup":rollback,"old_paths":old_paths,"new_paths":new_paths,"sidebar_backup":sidebar_backup,"sidebar_file":sidebar_file}),
    )?;
    progress("Applying verified chats");
    let mut changed_raw = Vec::new();
    let mut changed_db = Vec::new();
    let mut changed_sidebar = false;
    let install = (|| -> Result<()> {
        ensure!(
            !is_open()? && revision(home)? == closed_revision,
            "ChatGPT opened before installation"
        );
        for row in &remote_raw {
            let destination = scoped(home, &row.relative)?;
            if old_paths.contains(&row.relative) {
                copy_replace(&scoped(&candidate, &row.relative)?, &destination)?;
            } else {
                let checked = scoped(&candidate, &row.relative)?;
                verified_stream(
                    &mut fs::File::open(&checked)?,
                    &destination,
                    checked.metadata()?.len(),
                    &hash(&checked)?,
                )?;
            }
            changed_raw.push(row.relative.clone());
        }
        for row in &touched {
            if !new_paths.contains(&row.relative) {
                fs::remove_file(&row.path)?;
                changed_raw.push(row.relative.clone());
            }
        }
        for file in codex::DATABASES {
            ensure!(!is_open()?, "ChatGPT opened during installation");
            copy_replace(&candidate.join(file), &home.join(file))?;
            changed_db.push(file);
        }
        if merged_sidebar != before_sidebar {
            ensure!(!is_open()?, "Codex opened before the sidebar was installed");
            copy_replace(&sidebar_candidate, &sidebar_file)?;
            changed_sidebar = true;
        }
        receipt["installed_revision"] = json!(revision(home)?);
        write(&received.join("CHATS-DONE.json"), &receipt)?;
        ensure!(
            verified_receipt(
                download,
                home,
                backup_base,
                &index.snapshot,
                "CHATS-DONE.json"
            )?
            .is_some(),
            "The installed chats could not be verified."
        );
        Ok(())
    })();
    if let Err(error) = install {
        if !changed_raw.is_empty() || !changed_db.is_empty() || changed_sidebar {
            ensure!(
                !is_open()?,
                "ChatGPT opened during restore. Recovery copies are saved in {}",
                rollback.display()
            );
            for relative in &changed_raw {
                let target = scoped(home, relative)?;
                if old_paths.contains(relative) {
                    copy_replace(&scoped(&original, relative)?, &target)?;
                } else {
                    fs::remove_file(target)?;
                }
            }
            for file in changed_db {
                copy_replace(&rollback.join(file), &home.join(file))?;
            }
            if changed_sidebar {
                if sidebar_backup.is_file() {
                    copy_replace(&sidebar_backup, &sidebar_file)?;
                } else {
                    fs::remove_file(&sidebar_file)?;
                }
            }
        }
        if received.join("CHATS-DONE.json").is_file() {
            fs::remove_file(received.join("CHATS-DONE.json"))?;
        }
        fs::remove_file(&journal)?;
        return Err(error);
    }
    fs::remove_file(journal)?;
    drop(work);
    finish_files(
        download,
        home,
        backup_base,
        &root,
        &index.snapshot,
        receipt,
        options,
        fetch,
        progress,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_reuses_verified_objects_and_preserves_old_versions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = temp.path().join("store");
        let source = temp.path().join("chat.jsonl");
        let stage = |root: &Path, catalog: PublishCatalog| -> Result<PublishCatalog> {
            let mut files = PublishFiles {
                root,
                objects: Some(&store),
                catalog,
                next_catalog: PublishCatalog::default(),
                entries: BTreeMap::new(),
            };
            let digest = files.digest(&source, &fs::symlink_metadata(&source)?)?;
            files.stage(&source, "chat.jsonl".into(), digest)?;
            Ok(files.next_catalog)
        };
        let original = b"original chat";
        fs::write(&source, original)?;
        let old_digest = checksum(original);
        let saved = object(&store, &old_digest)?;
        let first = stage(&temp.path().join("first"), PublishCatalog::default())?;
        let stored_at = saved.metadata()?.modified()?;
        let second = stage(&temp.path().join("second"), first)?;
        assert_eq!(saved.metadata()?.modified()?, stored_at);
        assert_eq!(fs::read(temp.path().join("second/chat.jsonl"))?, original);

        let updated = b"original chat with a newer message";
        fs::write(&source, updated)?;
        stage(&temp.path().join("third"), second)?;
        assert_eq!(fs::read(saved)?, original);
        assert_eq!(fs::read(temp.path().join("first/chat.jsonl"))?, original);
        assert_eq!(fs::read(temp.path().join("third/chat.jsonl"))?, updated);
        Ok(())
    }

    #[test]
    fn project_source_rechecks_contents_and_does_not_copy() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("project-file.txt");
        let bytes = b"source project";
        fs::write(&path, bytes)?;
        let stamp = ProjectStamp {
            bytes: bytes.len() as u64,
            modified: fs::metadata(&path)?.modified()?,
            hash: checksum(bytes),
        };
        write(
            &temp.path().join("project-stamps.json"),
            &HashMap::from([(path.clone(), stamp)]),
        )?;
        assert_eq!(
            project_source(temp.path(), &checksum(bytes), Some(bytes.len() as u64))?,
            Some(path.clone())
        );
        assert!(!temp.path().join("objects").exists());
        fs::write(&path, b"changed value!")?;
        assert!(project_source(temp.path(), &checksum(bytes), Some(bytes.len() as u64))?.is_none());
        Ok(())
    }
    use std::io::Write;

    #[test]
    fn live_guard_distinguishes_wal_maintenance_from_real_edits() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let home = temp.path();
        for name in codex::DATABASES {
            let db = codex::connect(&home.join(name))?;
            db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE fixture(id INTEGER);")?;
        }
        let guard = LiveGuard::capture(home)?;
        let db = codex::connect(&home.join(codex::DATABASES[0]))?;
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        guard.check(home, &|| Ok(false))?;
        assert!(guard.check(home, &|| Ok(true)).is_err());
        db.execute("INSERT INTO fixture VALUES (1)", [])?;
        assert!(guard.check(home, &|| Ok(false)).is_err());
        Ok(())
    }

    #[test]
    fn project_transfer_keeps_local_edits_and_saves_conflict() -> Result<()> {
        let work = tempfile::tempdir()?;
        let tag = "20261123-111111-112233aa";
        let data = work.path().join("metadata");
        let target = work.path().join("projects");
        let folders = ProjectFolders {
            fallback: target.clone(),
            registered: HashMap::new(),
        };
        let received = work.path().join("received");
        let archive = work.path().join("ProjectArchives").join(tag).join("1.zip");
        mkdir_parent(&archive)?;
        let mut zip = zip::ZipWriter::new(fs::File::create_new(&archive)?);
        let rows = [
            ("sample/edited.txt", b"peer edit".as_slice()),
            ("sample/new.txt", b"new file".as_slice()),
        ];
        let mut files = Vec::new();
        for (relative, bytes) in rows {
            zip.start_file(relative, zip::write::SimpleFileOptions::default())?;
            zip.write_all(bytes)?;
            files.push(Entry {
                path: relative.into(),
                sha256: checksum(bytes),
                bytes: bytes.len() as u64,
            });
        }
        zip.finish()?;
        write(
            &data.join("PROJECTS.json"),
            &Projects {
                archives: vec![Archive {
                    name: "1.zip".into(),
                    files,
                }],
                ..Projects::default()
            },
        )?;
        let local = target.join("sample/edited.txt");
        mkdir_parent(&local)?;
        fs::write(&local, b"local edit")?;
        assert_eq!(
            add_project_files(
                work.path(),
                tag,
                &data,
                &folders,
                &received,
                &|_, _| Ok(()),
                &|_| {}
            )?,
            (1, 1)
        );
        assert_eq!(fs::read(local)?, b"local edit");
        assert_eq!(fs::read(target.join("sample/new.txt"))?, b"new file");
        assert_eq!(
            fs::read(
                received
                    .join("Project conflicts")
                    .join(checksum(b"peer edit"))
                    .join("sample/edited.txt")
            )?,
            b"peer edit"
        );
        assert_eq!(
            add_project_files(
                work.path(),
                tag,
                &data,
                &folders,
                &received,
                &|_, _| anyhow::bail!("Unchanged files should not download the archive"),
                &|_| {}
            )?,
            (0, 0)
        );
        Ok(())
    }

    #[test]
    fn only_missing_or_append_only_chats_are_selected() {
        let tokens = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let local = BTreeMap::from([
            ("same".into(), tokens(&["a"])),
            ("older".into(), tokens(&["a", "b"])),
            ("append".into(), tokens(&["a"])),
            ("different".into(), tokens(&["a", "local"])),
            ("inserted".into(), tokens(&["a", "c"])),
        ]);
        let incoming = BTreeMap::from([
            ("same".into(), tokens(&["a"])),
            ("older".into(), tokens(&["a"])),
            ("append".into(), tokens(&["a", "b"])),
            ("different".into(), tokens(&["a", "peer"])),
            ("inserted".into(), tokens(&["a", "b", "c"])),
            ("new".into(), tokens(&["new"])),
        ]);
        let (changes, review) = chat_changes(&incoming, &local, &[], &[], &HashSet::new(), false);
        assert_eq!(
            changes,
            HashSet::from(["append".into(), "inserted".into(), "new".into()])
        );
        assert_eq!(review, HashSet::from(["different".into()]));
        let (changes, review) = chat_changes(&incoming, &local, &[], &[], &HashSet::new(), true);
        assert_eq!(
            changes,
            HashSet::from([
                "append".into(),
                "inserted".into(),
                "new".into(),
                "older".into(),
                "different".into()
            ])
        );
        assert!(review.is_empty());
    }

    #[test]
    fn matching_sender_updates_shared_forks_and_protects_local_only_forks() {
        let source = BTreeMap::from([
            ("parent".into(), vec!["sender".into()]),
            ("shared".into(), vec![]),
        ]);
        let mut local = BTreeMap::from([
            ("parent".into(), vec!["receiver".into()]),
            ("shared".into(), vec![]),
        ]);
        let fork = |id: &str| Raw {
            physical: id.into(),
            owner: id.into(),
            parent: Some("parent".into()),
            relative: format!("{id}.jsonl"),
            path: PathBuf::new(),
        };
        let shared = fork("shared");
        let raw = std::slice::from_ref(&shared);
        let (selected, review) = chat_changes(&source, &local, raw, raw, &HashSet::new(), true);
        assert_eq!(selected, HashSet::from(["parent".into(), "shared".into()]));
        assert!(review.is_empty());
        local.insert("only-here".into(), vec![]);
        let (selected, review) = chat_changes(
            &source,
            &local,
            raw,
            &[shared.clone(), fork("only-here")],
            &HashSet::new(),
            true,
        );
        assert!(selected.is_empty());
        assert_eq!(review, HashSet::from(["parent".into(), "shared".into()]));
    }

    #[test]
    fn direct_project_transfer_requests_only_needed_objects() -> Result<()> {
        use std::sync::Mutex;
        let work = tempfile::tempdir()?;
        let data = work.path().join("metadata");
        let target = work.path().join("projects");
        let folders = ProjectFolders {
            fallback: work.path().join("unused"),
            registered: HashMap::from([("sample".into(), target.join("sample"))]),
        };
        let received = work.path().join("received");
        let rows = [
            ("sample/existing.txt", b"identical".as_slice()),
            ("sample/new.txt", b"new file".as_slice()),
        ];
        let files = rows
            .iter()
            .map(|(path, bytes)| Entry {
                path: (*path).into(),
                sha256: checksum(bytes),
                bytes: bytes.len() as u64,
            })
            .collect();
        write(
            &data.join("PROJECTS.json"),
            &Projects {
                files,
                ..Projects::default()
            },
        )?;
        let existing = target.join(rows[0].0);
        mkdir_parent(&existing)?;
        fs::write(&existing, rows[0].1)?;
        let original = existing.metadata()?.modified()?;
        let requests = Mutex::new(Vec::new());
        let fetch = |destination: &Path, row: Option<&Entry>| {
            let row = row.context("Missing project record")?;
            requests.lock().unwrap().push(row.path.clone());
            let (_, contents) = rows.iter().find(|(path, _)| *path == row.path).unwrap();
            let mut contents = *contents;
            verified_stream(&mut contents, destination, row.bytes, &row.sha256)
        };
        add_project_files(
            work.path(),
            "unused",
            &data,
            &folders,
            &received,
            &fetch,
            &|_| {},
        )?;
        add_project_files(
            work.path(),
            "unused",
            &data,
            &folders,
            &received,
            &fetch,
            &|_| {},
        )?;
        assert_eq!(requests.into_inner().unwrap(), [rows[1].0]);
        assert_eq!(existing.metadata()?.modified()?, original);
        assert_eq!(fs::read(target.join(rows[1].0))?, rows[1].1);
        Ok(())
    }

    #[test]
    #[ignore = "Requires the installed ChatGPT app and the previously verified backup fixtures"]
    fn native_restore_preserves_histories() -> Result<()> {
        let source = PathBuf::from(
            std::env::var_os("BETWEEN_RESTORE_SOURCE").context("Set the source fixture")?,
        );
        let tag = "20261123-020153-d5104f9a";
        let work = tempfile::tempdir()?;
        let home = work.path().join("profile");
        let download = work.path().join("download");
        let data = download.join("Snapshots").join(tag);
        let incoming = data.join("VerifiedChatData");
        let raw = raw_files(&source)?;
        profile(&source, &raw, &incoming)?;
        profile(&source, &raw, &home)?;
        let settings = codex::connect(&home.join(sidebar::SQLITE))?;
        settings.execute_batch(
            "CREATE TABLE settings(scope TEXT, key TEXT, value TEXT, PRIMARY KEY(scope,key));
             INSERT INTO settings VALUES ('global','receiver-only-setting','true');
             INSERT INTO settings VALUES ('electron-owner-atoms:test','sidebar-width','400');",
        )?;
        drop(settings);
        write(
            &home.join(sidebar::GLOBAL),
            &json!({"project-order":["stale-json"]}),
        )?;
        write(
            &incoming.join(sidebar::GLOBAL),
            &json!({"electron-owner-atoms:test":{
                "sidebar-width":290,
                "sidebar-project-list-expanded-v1":true
            }}),
        )?;
        let app = codex::App::locate()?;
        let progress = |text: &str| eprintln!("{text}");
        let incoming_db = codex::connect(&incoming.join(codex::DATABASES[0]))?;
        let mut foreign = Vec::new();
        for (id, _, _) in codex::threads(&incoming_db)? {
            let found = raw.iter().filter(|row| row.owner == id).collect::<Vec<_>>();
            if found.len() == 1
                && found[0].parent.is_none()
                && raw.iter().all(|r| r.parent.as_ref() != Some(&id))
            {
                incoming_db.execute("DELETE FROM threads WHERE id=?", [&id])?;
                fs::remove_file(scoped(&incoming, &found[0].relative)?)?;
                foreign.push(id);
                if foreign.len() == 4 {
                    break;
                }
            }
        }
        drop(incoming_db);
        assert_eq!(foreign.len(), 4);
        let shared: String = codex::ro(&incoming.join(codex::DATABASES[0]))?.query_row(
            "SELECT id FROM threads LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let shared_file = scoped(
            &home,
            &raw.iter().find(|r| r.owner == shared).unwrap().relative,
        )?;
        let modified = shared_file.metadata()?.modified()?;
        let expected_remote = codex::read_profiles(&app, &incoming, None, progress)?;
        let local = codex::connect(&home.join(codex::DATABASES[0]))?;
        for id in expected_remote.keys() {
            if id != &shared {
                local.execute("DELETE FROM threads WHERE id=?", [id])?;
            }
        }
        drop(local);
        let expected_local = codex::read_profiles(&app, &home, None, progress)?;
        assert!(!expected_remote.is_empty());
        assert!(!expected_local.is_empty());
        write(&data.join("ASSETS.json"), &Vec::<Asset>::new())?;
        let project = b"project file synced after verified chats";
        let project_file = Entry {
            path: "test-fixture/readme.txt".into(),
            sha256: checksum(project),
            bytes: project.len() as u64,
        };
        write(
            &data.join("PROJECTS.json"),
            &Projects {
                files: vec![project_file],
                ..Projects::default()
            },
        )?;
        write(&data.join("HISTORY.json"), &expected_remote)?;
        write(
            &data.join(sidebar::MANIFEST),
            &sidebar::export(Some(&incoming), &incoming.join(codex::DATABASES[0]))?,
        )?;
        let manifest = Index {
            snapshot: tag.into(),
            source: crate::lan::PC.into(),
            app_version: app.version.clone(),
            files: [
                "ASSETS.json",
                "PROJECTS.json",
                "HISTORY.json",
                sidebar::MANIFEST,
            ]
            .iter()
            .map(|name| entry(&data, &data.join(name)))
            .collect::<Result<_>>()?,
            chats: expected_remote.len(),
            project_files: 0,
        };
        write(&data.join("TRANSFER.json"), &manifest)?;
        write(
            &download.join("CURRENT.json"),
            &Pointer {
                snapshot: tag.into(),
                manifest_sha256: hash(&data.join("TRANSFER.json"))?,
            },
        )?;
        let backup = work.path().join("backups");
        let projects_folder = work.path().join("custom projects");
        let options = RestoreOptions {
            match_sender: true,
            projects_folder: Some(&projects_folder),
        };
        let early = apply_checked(
            &download,
            &home,
            &backup,
            crate::lan::PC,
            options,
            &|_, _| anyhow::bail!("Simulated Wi-Fi interruption while copying project files"),
            &progress,
            || Ok(false),
        )
        .unwrap_err();
        assert!(early.to_string().contains("Simulated Wi-Fi interruption"));
        assert!(confirmed(&download, &home, &backup, tag, options)?.is_none());
        assert!(verified_receipt(&download, &home, &backup, tag, "CHATS-DONE.json")?.is_some());
        let ui = sidebar::state(&home)?;
        assert_eq!(ui["receiver-only-setting"], true);
        assert_eq!(ui["electron-owner-atoms:test"]["sidebar-width"], 290);
        assert_eq!(
            ui["electron-owner-atoms:test"]["sidebar-project-list-expanded-v1"],
            true
        );
        assert_eq!(
            read::<Value>(&home.join(sidebar::GLOBAL))?["project-order"],
            json!(["stale-json"])
        );
        assert!(
            ui["project-order"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        );
        assert!(
            ui["thread-project-assignments"]
                .as_object()
                .is_some_and(|rows| !rows.is_empty())
        );
        let result = apply_checked(
            &download,
            &home,
            &backup,
            crate::lan::PC,
            options,
            &|target, row| {
                let row = row.context("Missing project file row")?;
                verified_stream(&mut project.as_slice(), target, row.bytes, &row.sha256)
            },
            &progress,
            || Ok(false),
        )?;
        assert!(result.contains("1 project files added"));
        assert_eq!(
            fs::read(projects_folder.join("test-fixture/readme.txt"))?,
            project
        );
        assert_eq!(
            shared_file.metadata()?.modified()?,
            modified,
            "Unchanged chat transcript was rewritten"
        );
        let after = codex::read_profiles(&app, &home, None, progress)?;
        for (id, history) in expected_remote.iter().chain(
            expected_local
                .iter()
                .filter(|(id, _)| !expected_remote.contains_key(*id)),
        ) {
            assert_eq!(
                Some(history),
                after.get(id),
                "Incorrect restored history for {id}"
            );
        }
        assert!(!backup.join("INSTALL-IN-PROGRESS.json").exists());
        assert_eq!(
            confirmed(&download, &home, &backup, tag, options)?,
            Some(result)
        );
        assert!(confirmed(&download, &home, &backup, tag, RestoreOptions::default())?.is_none());
        let conn = codex::connect(&home.join(codex::DATABASES[0]))?;
        conn.execute(
            "DELETE FROM threads WHERE id=?",
            [expected_remote.keys().next().unwrap()],
        )?;
        assert!(confirmed(&download, &home, &backup, tag, options)?.is_none());
        Ok(())
    }
}
