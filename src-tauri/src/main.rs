// Claude Accounts — a Tauri launcher that runs multiple isolated copies of the
// *native* Claude Desktop app side by side.
//
// Claude Desktop on Windows is a Microsoft Store (MSIX) package: it's
// single-instance, its exe can't be launched directly with args, and Store
// activation won't forward a custom data dir. To get isolated instances we:
//   1. Mirror the installed app's files to a writable folder (and RE-mirror
//      automatically whenever the Store version changes — so updates just work).
//   2. Launch that mirrored claude.exe with a per-account `--user-data-dir`
//      (separate login/data) plus `--no-sandbox` (required — a relocated,
//      unpackaged Electron app's child processes crash otherwise).
//
// The real login at %APPDATA%\Claude is never used, so it stays untouched.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write as _;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Clone, Serialize, Deserialize)]
struct Account {
    id: String,
    name: String,
    gradient: u8,
    #[serde(default)]
    created_at: u64,
    #[serde(default)]
    last_used: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct MirrorMeta {
    version: String,
}

#[derive(Serialize)]
struct Status {
    installed: bool,
    version: String,
    mirror_ready: bool,
}

/// Child Claude processes we launched, keyed by account id.
#[derive(Default)]
struct OpenState(Mutex<HashMap<String, Child>>);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------- paths (everything under LOCALAPPDATA to keep the 540MB mirror out of roaming) ----------
fn local_root(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_local_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    fs::create_dir_all(&dir).ok();
    dir
}
fn store_path(app: &tauri::AppHandle) -> PathBuf {
    local_root(app).join("accounts.json")
}
fn mirror_dir(app: &tauri::AppHandle) -> PathBuf {
    local_root(app).join("claude-app")
}
fn mirror_meta_path(app: &tauri::AppHandle) -> PathBuf {
    local_root(app).join("mirror.json")
}
fn profile_dir(app: &tauri::AppHandle, id: &str) -> PathBuf {
    let dir = local_root(app).join("profiles").join(id);
    fs::create_dir_all(&dir).ok();
    dir
}

// ---------- account store (durable + self-healing) ----------
fn backup_path(app: &tauri::AppHandle) -> PathBuf {
    local_root(app).join("accounts.bak.json")
}
fn meta_path(app: &tauri::AppHandle, id: &str) -> PathBuf {
    local_root(app).join("profiles").join(id).join(".ca-meta.json")
}

#[derive(Serialize, Deserialize)]
struct AccountMeta {
    name: String,
    gradient: u8,
}

fn parse_accounts(text: &str) -> Option<Vec<Account>> {
    let t = text.trim();
    if t.is_empty() {
        return None; // an empty/truncated file is suspicious — treat as unreadable
    }
    serde_json::from_str::<Vec<Account>>(t).ok()
}

/// `Some(list)` when the store is trustworthy (a *missing* file legitimately means
/// "no accounts yet"); `None` when the file exists but is unreadable/corrupt/empty
/// — in which case callers must NOT overwrite it with a blank list.
fn load_store(app: &tauri::AppHandle) -> Option<Vec<Account>> {
    match fs::read_to_string(store_path(app)) {
        Ok(s) => parse_accounts(&s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Vec::new()),
        Err(_) => None,
    }
}

fn write_profile_meta(app: &tauri::AppHandle, id: &str, name: &str, gradient: u8) {
    let p = meta_path(app, id);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent).ok();
    }
    if let Ok(s) = serde_json::to_string(&AccountMeta {
        name: name.to_string(),
        gradient,
    }) {
        fs::write(p, s).ok();
    }
}
fn read_profile_meta(app: &tauri::AppHandle, id: &str) -> Option<AccountMeta> {
    fs::read_to_string(meta_path(app, id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Re-attach any profile folder that holds a real session but is missing from the
/// list (self-heal after an accounts.json loss). Returns true if it changed.
fn reconcile_orphans(app: &tauri::AppHandle, list: &mut Vec<Account>) -> bool {
    let profiles = local_root(app).join("profiles");
    let known: HashSet<String> = list.iter().map(|a| a.id.clone()).collect();
    let mut changed = false;
    if let Ok(entries) = fs::read_dir(&profiles) {
        for e in entries.flatten() {
            let path = e.path();
            if !path.is_dir() {
                continue;
            }
            let id = e.file_name().to_string_lossy().to_string();
            if known.contains(&id) {
                continue;
            }
            // only recover folders that actually contain a Claude session
            let real = path.join("config.json").exists()
                || path.join("Network").exists()
                || path.join("Local Storage").exists();
            if !real {
                continue;
            }
            let (name, gradient) = match read_profile_meta(app, &id) {
                Some(m) => (m.name, m.gradient),
                None => (
                    format!("Recovered {}", &id[..id.len().min(6)]),
                    (list.len() as u8) % 6,
                ),
            };
            list.push(Account {
                id,
                name,
                gradient,
                created_at: now(),
                last_used: 0,
            });
            changed = true;
        }
    }
    changed
}

fn read_accounts(app: &tauri::AppHandle) -> Vec<Account> {
    match load_store(app) {
        Some(l) => l,
        // main store unreadable — fall back to the backup rather than "empty"
        None => fs::read_to_string(backup_path(app))
            .ok()
            .and_then(|s| parse_accounts(&s))
            .unwrap_or_default(),
    }
}

fn write_accounts(app: &tauri::AppHandle, list: &[Account]) {
    let Ok(s) = serde_json::to_string_pretty(list) else {
        return;
    };
    let path = store_path(app);
    // preserve the previous good copy as a backup before replacing
    if let Ok(prev) = fs::read_to_string(&path) {
        if parse_accounts(&prev).is_some() {
            fs::write(backup_path(app), &prev).ok();
        }
    }
    // durable atomic write: temp file -> fsync -> rename
    let tmp = local_root(app).join("accounts.json.tmp");
    if let Ok(mut f) = fs::File::create(&tmp) {
        if f.write_all(s.as_bytes()).is_ok() {
            let _ = f.flush();
            let _ = f.sync_all();
            drop(f);
            if fs::rename(&tmp, &path).is_ok() {
                return;
            }
        }
    }
    fs::write(&path, &s).ok(); // fallback
}

// ---------- Claude Desktop detection + mirroring ----------
/// (InstallLocation, Version) of the installed Store Claude, via Get-AppxPackage.
fn claude_info() -> Option<(PathBuf, String)> {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$p = Get-AppxPackage -Name Claude | Select-Object -First 1; if ($p) { \"$($p.InstallLocation)|$($p.Version)\" }",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        return None;
    }
    let mut parts = s.splitn(2, '|');
    let loc = parts.next()?.trim().to_string();
    let ver = parts.next().unwrap_or("").trim().to_string();
    if loc.is_empty() {
        return None;
    }
    Some((PathBuf::from(loc), ver))
}

fn stored_mirror_version(app: &tauri::AppHandle) -> Option<String> {
    fs::read_to_string(mirror_meta_path(app))
        .ok()
        .and_then(|s| serde_json::from_str::<MirrorMeta>(&s).ok())
        .map(|m| m.version)
}

/// Ensure the mirrored app matches the currently-installed version, copying
/// (or re-copying on update) if needed. Returns the mirrored claude.exe path.
fn ensure_mirror(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let (loc, ver) = claude_info()
        .ok_or_else(|| "Claude Desktop isn't installed (Microsoft Store).".to_string())?;
    let src_app = loc.join("app");
    if !src_app.join("claude.exe").exists() {
        return Err("Couldn't find Claude's app files — unexpected install layout.".into());
    }

    let mirror = mirror_dir(app);
    let mirror_exe = mirror.join("claude.exe");
    let up_to_date =
        mirror_exe.exists() && stored_mirror_version(app).as_deref() == Some(ver.as_str());

    if !up_to_date {
        fs::create_dir_all(&mirror).ok();
        // /MIR makes the mirror exactly match the current install (handles updates,
        // removes stale files from a previous version).
        let status = Command::new("robocopy")
            .arg(&src_app)
            .arg(&mirror)
            .args(["/MIR", "/R:1", "/W:1", "/NFL", "/NDL", "/NP", "/NJH", "/NJS"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map_err(|e| format!("Couldn't run robocopy: {e}"))?;
        // robocopy exit codes: <8 = success, >=8 = failure.
        let code = status.code().unwrap_or(16);
        if code >= 8 {
            return Err(format!("Copying the Claude app failed (robocopy code {code})."));
        }
        let meta = MirrorMeta { version: ver };
        if let Ok(s) = serde_json::to_string_pretty(&meta) {
            fs::write(mirror_meta_path(app), s).ok();
        }
    }
    Ok(mirror_exe)
}

// ---------- commands ----------
#[tauri::command]
fn get_status(app: tauri::AppHandle) -> Status {
    match claude_info() {
        Some((_, ver)) => {
            let ready = mirror_dir(&app).join("claude.exe").exists()
                && stored_mirror_version(&app).as_deref() == Some(ver.as_str());
            Status {
                installed: true,
                version: ver,
                mirror_ready: ready,
            }
        }
        None => Status {
            installed: false,
            version: String::new(),
            mirror_ready: false,
        },
    }
}

#[tauri::command]
fn sync_mirror(app: tauri::AppHandle) -> Result<String, String> {
    ensure_mirror(&app)?;
    Ok(claude_info().map(|(_, v)| v).unwrap_or_default())
}

#[tauri::command]
fn list_accounts(app: tauri::AppHandle) -> Vec<Account> {
    let mut list = read_accounts(&app);
    if reconcile_orphans(&app, &mut list) {
        write_accounts(&app, &list); // persist any recovered accounts
    }
    list
}

#[tauri::command]
fn add_account(app: tauri::AppHandle, name: String) -> Account {
    let mut list = read_accounts(&app);
    let acc = Account {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.trim().to_string(),
        gradient: (list.len() as u8) % 6,
        created_at: now(),
        last_used: 0,
    };
    list.push(acc.clone());
    write_accounts(&app, &list);
    // stamp the profile so the account can be recovered if accounts.json is ever lost
    write_profile_meta(&app, &acc.id, &acc.name, acc.gradient);
    acc
}

#[tauri::command]
fn rename_account(app: tauri::AppHandle, id: String, name: String) {
    let mut list = read_accounts(&app);
    if let Some(a) = list.iter_mut().find(|a| a.id == id) {
        a.name = name.trim().to_string();
        write_profile_meta(&app, &id, &a.name, a.gradient);
    }
    write_accounts(&app, &list);
}

#[tauri::command]
fn delete_account(app: tauri::AppHandle, state: State<OpenState>, id: String) {
    if let Some(mut child) = state.0.lock().unwrap().remove(&id) {
        let _ = child.kill();
    }
    let mut list = read_accounts(&app);
    list.retain(|a| a.id != id);
    write_accounts(&app, &list);
    fs::remove_dir_all(local_root(&app).join("profiles").join(&id)).ok();
}

#[tauri::command]
fn open_account(app: tauri::AppHandle, state: State<OpenState>, id: String) -> Result<(), String> {
    // Already open? Leave it alone.
    {
        let mut map = state.0.lock().unwrap();
        if let Some(child) = map.get_mut(&id) {
            match child.try_wait() {
                Ok(None) => return Ok(()),
                _ => {
                    map.remove(&id);
                }
            }
        }
    }

    // Ensure the mirror is present & current (this is what makes updates seamless).
    let exe = ensure_mirror(&app)?;
    let dir = profile_dir(&app, &id);

    // Only persist if the account is actually present — never rewrite the store
    // from a read that might have momentarily failed (that was the wipe bug).
    let mut list = read_accounts(&app);
    if let Some(a) = list.iter_mut().find(|a| a.id == id) {
        a.last_used = now();
        let (nm, g) = (a.name.clone(), a.gradient);
        write_accounts(&app, &list);
        write_profile_meta(&app, &id, &nm, g);
    }

    // CLAUDE_USER_DATA_DIR is Claude's own env var for a custom profile. Setting
    // it is essential: without it the app forces userData onto a single shared
    // folder (LOCALAPPDATA\Claude-3p), collapsing every instance into one via the
    // single-instance lock. With it, each account is a truly separate instance
    // that can run alongside the others (and the main app).
    //
    // --no-sandbox is required: the relocated/unpackaged Electron app's child
    // processes crash (STATUS_BREAKPOINT) without it.
    let child = Command::new(&exe)
        .env("CLAUDE_USER_DATA_DIR", &dir)
        .arg(format!("--user-data-dir={}", dir.display()))
        .arg("--no-sandbox")
        .spawn()
        .map_err(|e| format!("Couldn't launch Claude: {e}"))?;

    state.0.lock().unwrap().insert(id, child);
    Ok(())
}

#[tauri::command]
fn close_account(state: State<OpenState>, id: String) {
    if let Some(mut child) = state.0.lock().unwrap().remove(&id) {
        let _ = child.kill();
    }
}

#[tauri::command]
fn open_state(state: State<OpenState>) -> Vec<String> {
    let mut map = state.0.lock().unwrap();
    map.retain(|_, child| matches!(child.try_wait(), Ok(None)));
    map.keys().cloned().collect()
}

// ---------- config sync (MCP servers) ----------
// `id` is either "main" (the real %APPDATA%\Claude account, read-only) or an
// account id (an isolated profile).
fn desktop_config_path(app: &tauri::AppHandle, id: &str) -> Option<PathBuf> {
    if id == "main" {
        std::env::var("APPDATA")
            .ok()
            .map(|a| PathBuf::from(a).join("Claude").join("claude_desktop_config.json"))
    } else {
        Some(profile_dir(app, id).join("claude_desktop_config.json"))
    }
}

fn read_config(path: &PathBuf) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}))
}

/// The `mcpServers` object for a source ("main" or an account id).
#[tauri::command]
fn get_mcp_servers(app: tauri::AppHandle, id: String) -> Value {
    match desktop_config_path(&app, &id) {
        Some(p) => read_config(&p)
            .get("mcpServers")
            .cloned()
            .unwrap_or_else(|| json!({})),
        None => json!({}),
    }
}

/// Overwrite an account's `mcpServers` (other config keys are preserved).
/// Refuses to modify the real "main" account.
#[tauri::command]
fn set_mcp_servers(app: tauri::AppHandle, id: String, servers: Value) -> Result<(), String> {
    if id == "main" {
        return Err("Refusing to modify the main Claude account.".into());
    }
    let path = desktop_config_path(&app, &id).ok_or("no config path")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    let mut root = read_config(&path);
    if !root.is_object() {
        root = json!({});
    }
    root["mcpServers"] = servers;
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(OpenState::default())
        .invoke_handler(tauri::generate_handler![
            get_status,
            sync_mirror,
            list_accounts,
            add_account,
            rename_account,
            delete_account,
            open_account,
            close_account,
            open_state,
            get_mcp_servers,
            set_mcp_servers
        ])
        .setup(|app| {
            WebviewWindowBuilder::new(app, "launcher", WebviewUrl::App("index.html".into()))
                .title("Claude Accounts")
                .inner_size(960.0, 720.0)
                .min_inner_size(720.0, 560.0)
                .center()
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Claude Accounts");
}
