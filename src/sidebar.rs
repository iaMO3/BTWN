use crate::{
    codex,
    storage::{read, windows_path},
};
use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

pub const GLOBAL: &str = ".codex-global-state.json";
pub const SQLITE: &str = ".codex-global-state.sqlite";
pub const MANIFEST: &str = "SIDEBAR-V4.json";
pub const PREVIOUS_MANIFEST: &str = "SIDEBAR-V3.json";
pub const LEGACY_MANIFEST: &str = "SIDEBAR.json";
pub const MANIFESTS: [&str; 4] = [
    MANIFEST,
    PREVIOUS_MANIFEST,
    "SIDEBAR-V2.json",
    LEGACY_MANIFEST,
];
const MAPPING: &str = "app-server-project-id-by-legacy-project-id-by-host";
const ATOMS: &str = "electron-persisted-atom-state";
const OWNER: &str = "electron-owner-atoms:";
const VISIBLE: &str = "unified-sidebar-project-order-v1";
const EXPANDED: &str = "sidebar-project-expanded-v1-";
const PRESENTATION: &[&str] = &[
    "sidebar-width",
    "sidebar-collapsed-sections-v1",
    "sidebar-project-list-expanded-v1",
    "flat-project-sidebar-preferences-v1",
    "codex-sidebar-sort-mode-v1",
    "sidebar-organize-mode-v1",
    "sidebar-keep-projects-in-recent-v1",
    "projectless-sidebar-chats-first-v1",
];

#[derive(Clone, Serialize, Deserialize)]
struct Project {
    native: String,
    alias: String,
    name: String,
    #[serde(default)]
    timestamps: (i64, i64),
    #[serde(default)]
    appearance: Option<Value>,
    #[serde(default)]
    order: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Sidebar {
    projects: Vec<Project>,
    assignments: BTreeMap<String, String>,
    #[serde(default)]
    projectless: Vec<String>,
    #[serde(default)]
    visible_order: Vec<String>,
    #[serde(default)]
    presentation: BTreeMap<String, Map<String, Value>>,
}

impl Sidebar {
    pub fn is_empty(&self) -> bool {
        self.projects.is_empty() && self.projectless.is_empty() && self.presentation.is_empty()
    }
}

pub fn state(home: &Path) -> Result<Value> {
    let db = home.join(SQLITE);
    if db.is_file() {
        let connection = codex::ro(&db)?;
        let mut query =
            connection.prepare("SELECT scope,key,value FROM settings ORDER BY scope,key")?;
        let mut rows = query.query([])?;
        let mut state = json!({});
        while let Some(row) = rows.next()? {
            let scope: String = row.get(0)?;
            let key: String = row.get(1)?;
            let value: String = row.get(2)?;
            if scope == "global" {
                if !atom_scope(&key) {
                    object(&mut state).insert(key, serde_json::from_str(&value)?);
                }
            } else {
                let scope = if scope == "atom" { ATOMS } else { &scope };
                if atom_scope(scope) {
                    object(&mut state[scope]).insert(key, serde_json::from_str(&value)?);
                }
            }
        }
        return Ok(state);
    }
    let file = home.join(GLOBAL);
    let value = if file.is_file() {
        read(&file)?
    } else {
        json!({})
    };
    ensure!(value.is_object(), "Invalid Codex project settings");
    Ok(value)
}

fn atom_scope(key: &str) -> bool {
    key == ATOMS || key.starts_with(OWNER)
}

pub fn write_settings(path: &Path, before: &Value, after: &Value) -> Result<()> {
    let mut database = codex::connect(path)?;
    let tx = database.transaction()?;
    {
        let mut put = tx.prepare(
            "INSERT INTO settings(scope,key,value) VALUES(?,?,?) \
            ON CONFLICT(scope,key) DO UPDATE SET value=excluded.value",
        )?;
        let mut remove = tx.prepare("DELETE FROM settings WHERE scope=? AND key=?")?;
        for (key, value) in after.as_object().into_iter().flatten() {
            if before.get(key) == Some(value) {
                continue;
            }
            if !atom_scope(key) {
                put.execute(params!["global", key, value.to_string()])?;
                continue;
            }
            let scope = if key == ATOMS { "atom" } else { key };
            if before.get(key).is_none() {
                put.execute(params!["global", key, "{}"])?;
            }
            for (atom, value) in value.as_object().into_iter().flatten() {
                if before[key].get(atom) != Some(value) {
                    put.execute(params![scope, atom, value.to_string()])?;
                }
            }
            for atom in before[key].as_object().into_iter().flat_map(|m| m.keys()) {
                if value.get(atom).is_none() {
                    remove.execute(params![scope, atom])?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    value.as_object_mut().expect("JSON object")
}

fn mappings<'a>(state: &'a Value, home: &Path) -> Option<&'a Map<String, Value>> {
    let host = format!("local:{}", windows_path(home));
    state[MAPPING]
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(&host))?
        .1
        .as_object()
}

fn appearance(color: &Value, marker: &Value) -> Option<Value> {
    let color = color.as_str()?;
    let marker = match marker.as_str() {
        Some(encoded) => {
            serde_json::from_str(encoded).unwrap_or_else(|_| json!({"kind":"icon","icon":encoded}))
        }
        None => marker.clone(),
    };
    let field = match marker["kind"].as_str()? {
        "icon" => "icon",
        "emoji" => "emoji",
        _ => return None,
    };
    let symbol = marker[field].as_str()?;
    Some(json!({"color":color,"marker":{"kind":marker["kind"],(field):symbol}}))
}

fn presentation_key(key: &str) -> bool {
    PRESENTATION.contains(&key) || key == VISIBLE || key.starts_with(EXPANDED)
}

pub fn export(home: Option<&Path>, db: &Path) -> Result<Sidebar> {
    let ui = home.map(state).transpose()?.unwrap_or_else(|| json!({}));
    let host_map = home.and_then(|home| mappings(&ui, home));
    let order = strings(&ui["project-order"]);
    let c = codex::ro(db)?;
    let mut stmt = c.prepare(
        "SELECT id,name,metadata,created_at_ms,updated_at_ms FROM projects ORDER BY position,id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            (r.get::<_, i64>(3)?, r.get::<_, i64>(4)?),
        ))
    })?;
    let mut projects = Vec::new();
    for row in rows {
        let (native, name, metadata, timestamps) = row?;
        let alias = host_map
            .and_then(|m| {
                m.iter()
                    .filter(|(_, id)| id.as_str() == Some(&native))
                    .min_by_key(|(alias, _)| {
                        order.iter().position(|p| p == *alias).unwrap_or(usize::MAX)
                    })
                    .map(|(alias, _)| alias.clone())
            })
            .unwrap_or_else(|| native.clone());
        let native_metadata: Value = metadata
            .as_deref()
            .and_then(|entry| serde_json::from_str(entry).ok())
            .unwrap_or_default();
        let custom = ui["project-appearances"]
            .get(&alias)
            .or_else(|| ui["project-appearances"].get(&native))
            .unwrap_or(&Value::Null);
        let appearance = appearance(&custom["color"], &custom["marker"]).or_else(|| {
            appearance(
                &native_metadata["appearance.color"],
                &native_metadata["appearance.marker"],
            )
        });
        projects.push(Project {
            native,
            timestamps,
            name: ui["local-projects"][&alias]["name"]
                .as_str()
                .unwrap_or(&name)
                .to_owned(),
            order: strings(&ui["sidebar-project-thread-orders"][&alias]["threadIds"]),
            alias,
            appearance,
        });
    }
    projects.sort_by_key(|p| {
        order
            .iter()
            .position(|id| id == &p.alias)
            .unwrap_or(usize::MAX)
    });
    let aliases: HashMap<_, _> = projects
        .iter()
        .flat_map(|p| {
            [
                (p.alias.as_str(), p.native.as_str()),
                (p.native.as_str(), p.native.as_str()),
            ]
        })
        .collect();
    let explicit_projectless = strings(&ui["projectless-thread-ids"])
        .into_iter()
        .collect::<HashSet<_>>();
    let mut stmt = c.prepare("SELECT id,project_id FROM threads")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut assignments = BTreeMap::new();
    let mut projectless = Vec::new();
    for row in rows {
        let (thread, native) = row?;
        let assigned = ui["thread-project-assignments"][&thread]["projectId"]
            .as_str()
            .and_then(|alias| aliases.get(alias).map(|p| (*p).to_owned()));
        if let Some(project) = assigned {
            assignments.insert(thread, project);
        } else if explicit_projectless.contains(&thread) {
            projectless.push(thread);
        } else if let Some(project) = native {
            assignments.insert(thread, project);
        }
    }
    let presentation = ui
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| atom_scope(key))
        .filter_map(|(key, atoms)| {
            Some((
                key.clone(),
                atoms
                    .as_object()?
                    .iter()
                    .filter(|(key, _)| presentation_key(key))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            ))
        })
        .collect();
    Ok(Sidebar {
        projects,
        assignments,
        projectless,
        visible_order: Vec::new(),
        presentation,
    })
}

fn update_presentation(
    layout: &Sidebar,
    ui: &mut Value,
    aliases: &HashMap<String, String>,
    existing_mapping: &Map<String, Value>,
) {
    let source_ids: HashMap<_, _> = layout
        .projects
        .iter()
        .filter_map(|project| {
            let alias = aliases.get(&project.native)?.as_str();
            Some([
                (project.alias.as_str(), alias),
                (project.native.as_str(), alias),
            ])
        })
        .flatten()
        .collect();
    let synced = |id: &str| {
        aliases.contains_key(id)
            || aliases.values().any(|alias| alias == id)
            || existing_mapping
                .get(id)
                .and_then(Value::as_str)
                .is_some_and(|native| aliases.contains_key(native))
    };
    let merge_visible = |old: &Value, source: &[String]| {
        let mut visible = Vec::new();
        for item in source {
            let mapped = item
                .strip_prefix("codex:project:")
                .and_then(|id| source_ids.get(id))
                .map(|alias| format!("codex:project:{alias}"))
                .unwrap_or_else(|| item.clone());
            if !visible.contains(&mapped) {
                visible.push(mapped);
            }
        }
        for item in strings(old) {
            if !visible.contains(&item) && !item.strip_prefix("codex:project:").is_some_and(synced)
            {
                visible.push(item);
            }
        }
        json!(visible)
    };
    if layout.presentation.is_empty() && !layout.visible_order.is_empty() {
        let owner = ui
            .as_object()
            .into_iter()
            .flat_map(|map| map.keys())
            .find(|key| key.starts_with(OWNER))
            .cloned();
        if let Some(owner) = owner {
            ui[&owner][VISIBLE] = merge_visible(&ui[&owner][VISIBLE], &layout.visible_order);
        }
    }
    let has_owner = ui
        .as_object()
        .into_iter()
        .flat_map(|map| map.keys())
        .any(|key| key.starts_with(OWNER));
    for (scope, desired) in &layout.presentation {
        if !atom_scope(scope) || (scope.starts_with(OWNER) && has_owner && ui.get(scope).is_none())
        {
            continue;
        }
        let atoms = object(&mut ui[scope]);
        for key in PRESENTATION {
            if let Some(value) = desired.get(*key) {
                atoms.insert((*key).into(), value.clone());
            } else {
                atoms.remove(*key);
            }
        }
        let source_order = desired.get(VISIBLE).map(strings).unwrap_or_default();
        if let Some(old) = atoms.get(VISIBLE) {
            atoms.insert(VISIBLE.into(), merge_visible(old, &source_order));
        } else if desired.contains_key(VISIBLE) {
            atoms.insert(VISIBLE.into(), merge_visible(&Value::Null, &source_order));
        }
        let local_expansion = format!("{EXPANDED}codex:");
        atoms.retain(|key, _| !key.strip_prefix(&local_expansion).is_some_and(synced));
        for (key, value) in desired {
            let Some(id) = key.strip_prefix(EXPANDED) else {
                continue;
            };
            if !value.is_boolean() && !value.is_null() {
                continue;
            }
            let mapped = id
                .strip_prefix("codex:")
                .and_then(|id| source_ids.get(id))
                .map(|alias| format!("{EXPANDED}codex:{alias}"));
            if let Some(mapped) = mapped {
                atoms.insert(mapped, value.clone());
            } else if id.starts_with("chatgpt:g-p-") || id.starts_with("codex:g-p-") {
                atoms.insert(key.clone(), value.clone());
            }
        }
    }
}

pub fn apply(
    layout: &Sidebar,
    home: &Path,
    database: &mut Connection,
    before: Value,
    relevant: &HashSet<String>,
) -> Result<Value> {
    if layout.is_empty() {
        return Ok(before);
    }
    let mut ui = before;
    let host = format!("local:{}", windows_path(home));
    let host = ui[MAPPING]
        .as_object()
        .and_then(|map| map.keys().find(|key| key.eq_ignore_ascii_case(&host)))
        .cloned()
        .unwrap_or(host);
    let existing_mapping = mappings(&ui, home).cloned().unwrap_or_default();
    let old_order = strings(&ui["project-order"]);
    let mut aliases = HashMap::<String, String>::new();
    let mut order = Vec::new();
    let tx = database.transaction()?;
    for (position, project) in layout.projects.iter().enumerate() {
        ensure!(
            tx.execute(
                "UPDATE projects SET name=?,position=? WHERE id=?",
                params![project.name, position, project.native],
            )? == 1,
            "A synced project is missing from the sidebar database"
        );
        let alias = existing_mapping
            .iter()
            .filter(|(_, id)| id.as_str() == Some(&project.native))
            .min_by_key(|(alias, _)| {
                old_order
                    .iter()
                    .position(|item| item == *alias)
                    .unwrap_or(usize::MAX)
            })
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| {
                if ui["local-projects"].get(&project.alias).is_none() {
                    project.alias.clone()
                } else {
                    project.native.clone()
                }
            });
        let mut q =
            tx.prepare("SELECT path FROM project_roots WHERE project_id=? ORDER BY position")?;
        let roots = q
            .query_map([&project.native], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let entry = object(
            object(&mut ui["local-projects"])
                .entry(alias.clone())
                .or_insert_with(|| json!({})),
        );
        entry.insert("id".into(), json!(&alias));
        entry.insert("name".into(), json!(&project.name));
        entry.insert("rootPaths".into(), json!(roots));
        entry
            .entry("createdAt")
            .or_insert(json!(project.timestamps.0));
        entry
            .entry("updatedAt")
            .or_insert(json!(project.timestamps.1));
        object(&mut ui[MAPPING]);
        let map = object(&mut ui[MAPPING][&host]);
        map.insert(alias.clone(), json!(&project.native));
        if let Some(style) = project
            .appearance
            .as_ref()
            .and_then(|v| appearance(&v["color"], &v["marker"]))
        {
            let raw: Option<String> = tx.query_row(
                "SELECT metadata FROM projects WHERE id=?",
                [&project.native],
                |row| row.get(0),
            )?;
            let mut metadata: Value = raw
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?
                .unwrap_or_else(|| json!({}));
            let entries = metadata
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("Invalid project metadata"))?;
            entries.insert("appearance.color".into(), style["color"].clone());
            entries.insert(
                "appearance.marker".into(),
                json!(style["marker"].to_string()),
            );
            tx.execute(
                "UPDATE projects SET metadata=? WHERE id=?",
                params![metadata.to_string(), project.native],
            )?;
            object(&mut ui["project-appearances"]).insert(alias.clone(), style.clone());
            if alias != project.native {
                object(&mut ui["project-appearances"]).insert(project.native.clone(), style);
            }
        }
        order.push(alias.clone());
        aliases.insert(project.native.clone(), alias);
    }
    let local_order = tx
        .prepare("SELECT id FROM projects ORDER BY position,id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (offset, id) in local_order
        .iter()
        .filter(|id| !aliases.contains_key(*id))
        .enumerate()
    {
        tx.execute(
            "UPDATE projects SET position=? WHERE id=?",
            params![layout.projects.len() + offset, id],
        )?;
    }
    let extra = old_order
        .into_iter()
        .filter(|id| {
            !order.contains(id)
                && !aliases.contains_key(id)
                && !existing_mapping
                    .get(id)
                    .and_then(Value::as_str)
                    .is_some_and(|native| aliases.contains_key(native))
        })
        .collect::<Vec<_>>();
    order.extend(extra);
    ui["project-order"] = json!(order);
    update_presentation(layout, &mut ui, &aliases, &existing_mapping);
    let mut assigned = HashSet::new();
    for (thread, native) in &layout.assignments {
        if !relevant.contains(thread) {
            continue;
        }
        let Some(alias) = aliases.get(native) else {
            continue;
        };
        if tx.execute(
            "UPDATE threads SET project_id=? WHERE id=?",
            params![native, thread],
        )? == 0
        {
            continue;
        }
        object(&mut ui["thread-project-assignments"]).insert(
            thread.clone(),
            json!({"projectKind":"local","projectId":alias}),
        );
        object(&mut ui["thread-project-membership-host-ids"])
            .insert(thread.clone(), json!("local"));
        assigned.insert(thread.clone());
    }
    let mut projectless = strings(&ui["projectless-thread-ids"])
        .into_iter()
        .filter(|id| !assigned.contains(id))
        .collect::<Vec<_>>();
    let mut detached = HashSet::new();
    for id in &layout.projectless {
        if !relevant.contains(id) || assigned.contains(id) {
            continue;
        }
        if tx.execute("UPDATE threads SET project_id=NULL WHERE id=?", [id])? == 0 {
            continue;
        }
        object(&mut ui["thread-project-assignments"]).remove(id);
        object(&mut ui["thread-project-membership-host-ids"]).remove(id);
        if !projectless.contains(id) {
            projectless.push(id.clone());
        }
        detached.insert(id.clone());
    }
    ui["projectless-thread-ids"] = json!(projectless);
    if let Some(groups) = ui["sidebar-project-thread-orders"].as_object_mut() {
        for (alias, entry) in groups {
            let valid_here = aliases
                .iter()
                .find(|(_, mapped)| *mapped == alias)
                .map(|(native, _)| native);
            if let Some(ids) = entry["threadIds"].as_array_mut() {
                ids.retain(|id| {
                    id.as_str().is_none_or(|id| {
                        !detached.contains(id)
                            && (!assigned.contains(id) || layout.assignments.get(id) == valid_here)
                    })
                });
            }
        }
    }
    for project in &layout.projects {
        let Some(alias) = aliases.get(&project.native) else {
            continue;
        };
        let mut threads = project
            .order
            .iter()
            .filter(|id| {
                assigned.contains(*id) && layout.assignments.get(*id) == Some(&project.native)
            })
            .cloned()
            .collect::<Vec<_>>();
        for id in strings(&ui["sidebar-project-thread-orders"][alias]["threadIds"]) {
            if !threads.contains(&id)
                && (!assigned.contains(&id) || layout.assignments.get(&id) == Some(&project.native))
            {
                threads.push(id);
            }
        }
        object(&mut ui["sidebar-project-thread-orders"])
            .insert(alias.clone(), json!({"threadIds":threads}));
    }
    tx.commit()?;
    Ok(ui)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::write;

    #[test]
    fn restores_project_markers_and_moves_shared_chats_without_removing_local_entries() -> Result<()>
    {
        let folder = tempfile::tempdir()?;
        let source = folder.path().join("source");
        let target = folder.path().join("target");
        let make_db = |home: &Path| -> Result<Connection> {
            std::fs::create_dir_all(home)?;
            let db = Connection::open(home.join("state.sqlite"))?;
            db.execute_batch(
                "CREATE TABLE projects(id TEXT PRIMARY KEY,name TEXT,metadata TEXT,position INTEGER,created_at_ms INTEGER,updated_at_ms INTEGER);
                 CREATE TABLE project_roots(project_id TEXT,position INTEGER,path TEXT);
                 CREATE TABLE threads(id TEXT PRIMARY KEY,project_id TEXT,name TEXT,title TEXT,rollout_path TEXT);
                 INSERT INTO projects VALUES ('a','First',NULL,0,10,11);
                 INSERT INTO projects VALUES ('b','Second',NULL,1,20,21);
                 INSERT INTO threads VALUES ('shared','b','Shared',NULL,'rollout');
                 INSERT INTO threads VALUES ('unassigned','a','Unassigned',NULL,'elsewhere');",
            )?;
            Ok(db)
        };
        let source_db = make_db(&source)?;
        let mut target_db = make_db(&target)?;
        source_db.execute("UPDATE projects SET metadata=? WHERE id='a'",
            [json!({"appearance.color":"pink","appearance.marker":"{\"kind\":\"icon\",\"icon\":\"flask\"}"}).to_string()])?;
        let b_style = json!({"color":"orange","marker":{"kind":"icon","icon":"function"}});
        write(
            &source.join(GLOBAL),
            &json!({
                "project-order":["b","a"],
                "project-appearances":{"b":b_style},
                "sidebar-project-thread-orders":{"b":{"threadIds":["shared"]}},
                "projectless-thread-ids":["unassigned"],
                "electron-owner-atoms:test":{
                    "unified-sidebar-project-order-v1":["codex:project:b","chatgpt:project:cloud","codex:project:a"],
                    "sidebar-project-expanded-v1-codex:b":true,
                    "flat-project-sidebar-preferences-v1":{"mode":"project","chatSortMode":"updated_at"},
                    "sidebar-width":290
                }
            }),
        )?;
        target_db.execute_batch(
            "UPDATE threads SET project_id='a' WHERE id='shared';
             INSERT INTO projects VALUES ('extra','Laptop only',NULL,0,30,31);
             INSERT INTO threads VALUES ('mine','extra','Local chat',NULL,'local');",
        )?;
        let host = format!("local:{}", windows_path(&target));
        let before = json!({
            (MAPPING):{(host):{"old-b":"b","new-b":"b"}},
            "project-order":["extra","old-b","new-b","a"],
            "electron-owner-atoms:test":{
                "unified-sidebar-project-order-v1":["codex:project:extra","codex:project:old-b","codex:project:new-b"],
                "sidebar-project-expanded-v1-codex:a":true,
                "sidebar-project-expanded-v1-codex:new-b":false,
                "sidebar-project-expanded-v1-codex:extra":true,
                "sidebar-project-list-expanded-v1":true,
                "flat-project-sidebar-preferences-v1":{"mode":"folder"}
            },
            "thread-project-assignments":{"shared":{"projectKind":"local","projectId":"a"}},
            "sidebar-project-thread-orders":{"a":{"threadIds":["shared","unassigned"]},"extra":{"threadIds":["mine"]}}
        });
        let layout = export(Some(&source), &source.join("state.sqlite"))?;
        assert_eq!(layout.projects[0].appearance, Some(b_style));
        assert_eq!(
            layout.projects[1].appearance.as_ref().unwrap()["marker"]["icon"],
            "flask"
        );
        let ids = ["shared".to_owned(), "unassigned".to_owned()]
            .into_iter()
            .collect();
        let installed = apply(&layout, &target, &mut target_db, before, &ids)?;
        assert_eq!(
            installed["electron-owner-atoms:test"]["unified-sidebar-project-order-v1"],
            json!([
                "codex:project:old-b",
                "chatgpt:project:cloud",
                "codex:project:a",
                "codex:project:extra"
            ])
        );
        assert_eq!(installed["project-order"], json!(["old-b", "a", "extra"]));
        let atoms = &installed["electron-owner-atoms:test"];
        assert_eq!(atoms["sidebar-project-expanded-v1-codex:old-b"], true);
        assert_eq!(atoms["sidebar-project-expanded-v1-codex:extra"], true);
        assert!(atoms.get("sidebar-project-expanded-v1-codex:a").is_none());
        assert!(
            atoms
                .get("sidebar-project-expanded-v1-codex:new-b")
                .is_none()
        );
        assert!(atoms.get("sidebar-project-list-expanded-v1").is_none());
        assert_eq!(
            atoms["flat-project-sidebar-preferences-v1"]["mode"],
            "project"
        );
        assert_eq!(atoms["sidebar-width"], 290);
        assert_eq!(
            installed["sidebar-project-thread-orders"]["a"]["threadIds"],
            json!([])
        );
        assert_eq!(
            installed["sidebar-project-thread-orders"]["extra"]["threadIds"],
            json!(["mine"])
        );
        let own: String =
            target_db.query_row("SELECT project_id FROM threads WHERE id='mine'", [], |r| {
                r.get(0)
            })?;
        assert_eq!(own, "extra");
        write(&target.join(GLOBAL), &installed)?;
        verify(&layout, &target, &target.join("state.sqlite"), &ids)?;
        let mut changed = installed;
        changed["project-appearances"]["old-b"]["color"] = json!("blue");
        write(&target.join(GLOBAL), &changed)?;
        assert!(verify(&layout, &target, &target.join("state.sqlite"), &ids).is_err());
        Ok(())
    }
}

pub fn verify(
    layout: &Sidebar,
    home: &Path,
    database: &Path,
    relevant: &HashSet<String>,
) -> Result<()> {
    let installed = export(Some(home), database)?;
    let imported = layout
        .projects
        .iter()
        .map(|p| p.native.as_str())
        .collect::<HashSet<_>>();
    let projected = installed
        .projects
        .iter()
        .filter(|p| imported.contains(p.native.as_str()));
    let db = codex::ro(database)?;
    let native_order = db
        .prepare("SELECT id FROM projects ORDER BY position,id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        native_order
            .iter()
            .filter(|id| imported.contains(id.as_str()))
            .eq(layout.projects.iter().map(|project| &project.native)),
        "Project database order did not verify"
    );
    let ui = state(home)?;
    let registered = codex::threads(&db)?
        .into_iter()
        .map(|row| row.0)
        .collect::<HashSet<_>>();
    let active = |id: &str| relevant.contains(id) && registered.contains(id);
    ensure!(
        projected.count() == layout.projects.len(),
        "Installed sidebar is missing projects"
    );
    let aliases = installed
        .projects
        .iter()
        .filter(|p| imported.contains(p.native.as_str()))
        .map(|p| (p.native.clone(), p.alias.clone()))
        .collect();
    let mut expected = ui.clone();
    update_presentation(
        layout,
        &mut expected,
        &aliases,
        &mappings(&ui, home).cloned().unwrap_or_default(),
    );
    ensure!(expected == ui, "Sidebar display settings did not verify");
    for (actual, expected) in installed
        .projects
        .iter()
        .filter(|p| imported.contains(p.native.as_str()))
        .zip(&layout.projects)
    {
        ensure!(
            actual.native == expected.native && actual.name == expected.name,
            "Project sidebar order or names did not verify"
        );
        if let Some(style) = expected
            .appearance
            .as_ref()
            .and_then(|v| appearance(&v["color"], &v["marker"]))
        {
            let metadata: String = db.query_row(
                "SELECT metadata FROM projects WHERE id=?",
                [&actual.native],
                |r| r.get(0),
            )?;
            let metadata: Value = serde_json::from_str(&metadata)?;
            ensure!(
                actual.appearance.as_ref() == Some(&style)
                    && appearance(
                        &metadata["appearance.color"],
                        &metadata["appearance.marker"]
                    )
                    .as_ref()
                        == Some(&style),
                "A synced project icon or color did not verify"
            );
        }
        let ordered =
            |id: &String| active(id) && layout.assignments.get(id) == Some(&expected.native);
        ensure!(
            actual
                .order
                .iter()
                .take(expected.order.iter().filter(|id| ordered(id)).count())
                .eq(expected.order.iter().filter(|id| ordered(id))),
            "Project chat order did not verify"
        );
    }
    for (id, native) in &layout.assignments {
        if active(id) && imported.contains(native.as_str()) {
            let actual: Option<String> =
                db.query_row("SELECT project_id FROM threads WHERE id=?", [id], |r| {
                    r.get(0)
                })?;
            let alias = ui["thread-project-assignments"][id]["projectId"].as_str();
            ensure!(
                actual.as_ref() == Some(native)
                    && installed.assignments.get(id) == Some(native)
                    && alias.and_then(|alias| mappings(&ui, home)?.get(alias)?.as_str())
                        == Some(native),
                "A synced chat's project membership did not verify"
            );
        }
    }
    for id in layout.projectless.iter().filter(|id| active(id)) {
        let actual: Option<String> =
            db.query_row("SELECT project_id FROM threads WHERE id=?", [id], |r| {
                r.get(0)
            })?;
        ensure!(
            actual.is_none() && installed.projectless.contains(id),
            "A synced chat's sidebar placement did not verify"
        );
    }
    Ok(())
}
