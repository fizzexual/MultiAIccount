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

use std::collections::HashMap;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
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

// ---------- account store ----------
fn read_accounts(app: &tauri::AppHandle) -> Vec<Account> {
    fs::read_to_string(store_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
fn write_accounts(app: &tauri::AppHandle, list: &[Account]) {
    if let Ok(s) = serde_json::to_string_pretty(list) {
        fs::write(store_path(app), s).ok();
    }
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
    read_accounts(&app)
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
    acc
}

#[tauri::command]
fn rename_account(app: tauri::AppHandle, id: String, name: String) {
    let mut list = read_accounts(&app);
    if let Some(a) = list.iter_mut().find(|a| a.id == id) {
        a.name = name.trim().to_string();
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

    let mut list = read_accounts(&app);
    if let Some(a) = list.iter_mut().find(|a| a.id == id) {
        a.last_used = now();
    }
    write_accounts(&app, &list);

    // --no-sandbox is required: the relocated/unpackaged Electron app's child
    // processes crash (STATUS_BREAKPOINT) without it. This is the user-chosen
    // native approach's known tradeoff.
    let child = Command::new(&exe)
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
            open_state
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
