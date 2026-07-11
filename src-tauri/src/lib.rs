mod codex;

use serde::Serialize;
use serde_json::{json, Value};
use std::env;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

// ── App 狀態 ────────────────────────────────────────

struct AppState {
    codex_process: Arc<Mutex<codex::CodexProcess>>,
    working_dir: Arc<Mutex<String>>,
    model: Arc<Mutex<Option<String>>>,
    discord_bot: Arc<Mutex<Option<tokio::process::Child>>>,
    watcher_generation: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Serialize)]
struct DiscordBotStatus {
    running: bool,
    owned_by_gui: bool,
    pid: Option<u32>,
    config_dir: Option<String>,
    env_path: Option<String>,
    asset_dir: Option<String>,
    source_label: Option<String>,
    launch_target: Option<String>,
}

#[derive(Debug, Clone)]
enum DiscordBotLaunchTarget {
    PythonScript(PathBuf),
    Executable(PathBuf),
}

#[derive(Debug, Clone)]
struct DiscordBotRuntime {
    launch_target: DiscordBotLaunchTarget,
    asset_dir: PathBuf,
    config_dir: PathBuf,
    source_label: String,
}

#[derive(Debug, Clone, Default)]
struct SessionCatalogEntry {
    id: String,
    cwd: String,
    originator: String,
    modified: String,
    title: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct SessionListEntry {
    #[serde(rename = "sessionId")]
    session_id: String,
    title: String,
    modified: String,
    #[serde(rename = "displayLabel")]
    display_label: String,
}

#[cfg(windows)]
fn hide_tokio_console(cmd: &mut tokio::process::Command) {
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_tokio_console(_cmd: &mut tokio::process::Command) {}

fn discord_bot_candidates(app: &AppHandle) -> Vec<(PathBuf, &'static str)> {
    let mut candidates: Vec<(PathBuf, &'static str)> = Vec::new();

    if let Ok(override_dir) = env::var("XIAOKUI_BOT_ASSET_DIR") {
        candidates.push((PathBuf::from(override_dir), "env override"));
    }

    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push((
            resource_dir.join("resources").join("discord-bot"),
            "release resource dir/resources",
        ));
        candidates.push((resource_dir.join("discord-bot"), "legacy release bot dir"));
    }

    let repo_root = repo_root();
    candidates.push((
        repo_root.join("src-tauri").join("resources").join("discord-bot"),
        "repo resources dir",
    ));
    candidates.push((
        repo_root.join("runtime").join("discord-bot-src"),
        "repo source dir",
    ));

    candidates
}

fn app_discord_bot_config_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let app_data_dir = app.path()
        .app_data_dir()
        .map_err(|e| format!("找不到 app data dir: {}", e))?;
    Ok(app_data_dir.join("discord-bot"))
}

fn runtime_launch_target_string(runtime: &DiscordBotRuntime) -> String {
    match &runtime.launch_target {
        DiscordBotLaunchTarget::PythonScript(path) => path.display().to_string(),
        DiscordBotLaunchTarget::Executable(path) => path.display().to_string(),
    }
}

fn packaged_bot_name() -> &'static str {
    if cfg!(windows) { "xiaokui_bot.exe" } else { "xiaokui_bot" }
}

fn inspect_discord_bot_runtime(app: &AppHandle) -> DiscordBotStatus {
    let config_dir = app_discord_bot_config_dir(app).ok();
    let env_path = config_dir.as_ref().map(|dir| dir.join(".env.xiaokui"));

    for (dir, label) in discord_bot_candidates(app) {
        if dir_has_packaged_bot(&dir) {
            return DiscordBotStatus {
                running: false,
                owned_by_gui: false,
                pid: None,
                config_dir: config_dir.as_ref().map(|p| p.display().to_string()),
                env_path: env_path.as_ref().map(|p| p.display().to_string()),
                asset_dir: Some(dir.display().to_string()),
                source_label: Some(label.to_string()),
                launch_target: Some(dir.join(packaged_bot_name()).display().to_string()),
            };
        }

        if dir_has_source_bot(&dir) {
            return DiscordBotStatus {
                running: false,
                owned_by_gui: false,
                pid: None,
                config_dir: config_dir.as_ref().map(|p| p.display().to_string()),
                env_path: env_path.as_ref().map(|p| p.display().to_string()),
                asset_dir: Some(dir.display().to_string()),
                source_label: Some(label.to_string()),
                launch_target: Some(dir.join("bot_xiaokui.py").display().to_string()),
            };
        }
    }

    DiscordBotStatus {
        running: false,
        owned_by_gui: false,
        pid: None,
        config_dir: config_dir.as_ref().map(|p| p.display().to_string()),
        env_path: env_path.as_ref().map(|p| p.display().to_string()),
        asset_dir: None,
        source_label: None,
        launch_target: None,
    }
}

fn runtime_status(runtime: &DiscordBotRuntime, running: bool, owned_by_gui: bool, pid: Option<u32>) -> DiscordBotStatus {
    DiscordBotStatus {
        running,
        owned_by_gui,
        pid,
        config_dir: Some(runtime.config_dir.display().to_string()),
        env_path: Some(runtime.config_dir.join(".env.xiaokui").display().to_string()),
        asset_dir: Some(runtime.asset_dir.display().to_string()),
        source_label: Some(runtime.source_label.clone()),
        launch_target: Some(runtime_launch_target_string(runtime)),
    }
}

// ── Tauri Commands: Codex 生命週期 ──────────────────

#[tauri::command]
async fn start_codex(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    working_dir: String,
    model: Option<String>,
    port: Option<u16>,
) -> Result<(), String> {
    let model_str = model.and_then(|m| {
        let trimmed = m.trim().to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    });
    let ws_port = port.unwrap_or(45888);

    {
        let mut wd = state.working_dir.lock().await;
        *wd = working_dir.clone();
        let mut m = state.model.lock().await;
        *m = model_str;
    }

    codex::start(app, state.codex_process.clone(), &working_dir, ws_port).await
}

#[tauri::command]
async fn stop_codex(
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    codex::stop(state.codex_process.clone()).await
}

#[tauri::command]
async fn create_thread(
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let cwd = state.working_dir.lock().await.clone();
    let model = state.model.lock().await.clone();
    codex::create_thread(&state.codex_process, &cwd, model.as_deref()).await
}

#[tauri::command]
async fn resume_thread(
    state: tauri::State<'_, AppState>,
    thread_id: String,
) -> Result<(), String> {
    let cwd = state.working_dir.lock().await.clone();
    let model = state.model.lock().await.clone();
    codex::resume_thread(&state.codex_process, &thread_id, &cwd, model.as_deref()).await
}

#[tauri::command]
async fn send_prompt(
    state: tauri::State<'_, AppState>,
    thread_id: String,
    text: String,
    image_paths: Option<Vec<String>>,
) -> Result<String, String> {
    let cwd = state.working_dir.lock().await.clone();
    let model = state.model.lock().await.clone();
    codex::send_prompt(
        &state.codex_process,
        &thread_id,
        &text,
        &image_paths.unwrap_or_default(),
        &cwd,
        model.as_deref(),
    ).await
}

// ── Tauri Commands: Discord Bot ─────────────────────

#[cfg(windows)]
async fn find_external_discord_bot_pid() -> Option<u32> {
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Where-Object { ((($_.Name -ieq 'python.exe') -or ($_.Name -ieq 'pythonw.exe')) -and ($_.CommandLine -like '*bot_xiaokui.py*')) -or ($_.Name -ieq 'xiaokui_bot.exe') } | Select-Object -First 1 -ExpandProperty ProcessId",
        ]);
    hide_tokio_console(&mut cmd);

    let output = cmd.output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.trim().parse::<u32>().ok()
}

#[cfg(not(windows))]
async fn find_external_discord_bot_pid() -> Option<u32> {
    None
}

async fn get_discord_bot_status_internal(app: &AppHandle, state: &AppState) -> DiscordBotStatus {
    let inspected = inspect_discord_bot_runtime(app);

    let mut bot = state.discord_bot.lock().await;
    if let Some(ref mut child) = *bot {
        match child.try_wait() {
            Ok(None) => {
                let pid = child.id();
                if let Ok(runtime) = resolve_discord_bot_runtime(app) {
                    return runtime_status(&runtime, true, true, pid);
                }
                let mut status = inspected.clone();
                status.running = true;
                status.owned_by_gui = true;
                status.pid = pid;
                return status;
            }
            _ => {
                *bot = None;
            }
        }
    }
    drop(bot);

    let external_pid = find_external_discord_bot_pid().await;
    let mut status = inspected;
    status.running = external_pid.is_some();
    status.owned_by_gui = false;
    status.pid = external_pid;
    status
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri 應有上層 repo root")
        .to_path_buf()
}

fn dir_has_packaged_bot(dir: &Path) -> bool {
    dir.join(packaged_bot_name()).is_file()
}

fn dir_has_source_bot(dir: &Path) -> bool {
    dir.join("bot_xiaokui.py").exists()
}

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("建立資料夾失敗 {}: {}", parent.display(), e))?;
    }
    Ok(())
}

fn copy_if_missing(source: &Path, target: &Path) -> Result<(), String> {
    if !source.exists() || target.exists() {
        return Ok(());
    }
    ensure_parent_dir(target)?;
    fs::copy(source, target)
        .map_err(|e| format!("複製檔案失敗 {} -> {}: {}", source.display(), target.display(), e))?;
    Ok(())
}

fn ensure_discord_bot_config_dir(
    app: &AppHandle,
    asset_dir: &Path,
) -> Result<PathBuf, String> {
    let config_dir = app_discord_bot_config_dir(app)?;
    fs::create_dir_all(&config_dir)
        .map_err(|e| format!("建立 Discord Bot 設定目錄失敗 {}: {}", config_dir.display(), e))?;

    copy_if_missing(
        &asset_dir.join("config_xiaokui.json"),
        &config_dir.join("config_xiaokui.json"),
    )?;
    copy_if_missing(
        &asset_dir.join(".env.xiaokui.example"),
        &config_dir.join(".env.xiaokui"),
    )?;
    copy_if_missing(
        &asset_dir.join(".env.xiaokui.example"),
        &config_dir.join(".env.xiaokui.example"),
    )?;

    Ok(config_dir)
}

fn resolve_discord_bot_runtime(app: &AppHandle) -> Result<DiscordBotRuntime, String> {
    for (dir, label) in discord_bot_candidates(app) {
        if dir_has_packaged_bot(&dir) {
            let config_dir = ensure_discord_bot_config_dir(app, &dir)?;
            return Ok(DiscordBotRuntime {
                launch_target: DiscordBotLaunchTarget::Executable(dir.join(packaged_bot_name())),
                asset_dir: dir,
                config_dir,
                source_label: label.to_string(),
            });
        }

        if dir_has_source_bot(&dir) {
            let config_dir = ensure_discord_bot_config_dir(app, &dir)?;
            return Ok(DiscordBotRuntime {
                launch_target: DiscordBotLaunchTarget::PythonScript(dir.join("bot_xiaokui.py")),
                asset_dir: dir,
                config_dir,
                source_label: label.to_string(),
            });
        }
    }

    Err("找不到可用的 Discord Bot runtime（既沒有 xiaokui_bot.exe，也沒有 bot_xiaokui.py）".to_string())
}

#[tauri::command]
async fn start_discord_bot(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<DiscordBotStatus, String> {
    let current_status = get_discord_bot_status_internal(&app, &state).await;
    if current_status.running {
        return Ok(current_status);
    }

    let working_dir = state.working_dir.lock().await.clone();
    let runtime = resolve_discord_bot_runtime(&app)?;
    let mut cmd = match &runtime.launch_target {
        DiscordBotLaunchTarget::PythonScript(script_path) => {
            let mut cmd = tokio::process::Command::new("python");
            cmd.arg(script_path);
            cmd
        }
        DiscordBotLaunchTarget::Executable(exe_path) => tokio::process::Command::new(exe_path),
    };
    cmd.current_dir(&runtime.config_dir);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.env("XIAOKUI_BOT_DIR", &runtime.config_dir);
    cmd.env("CODEX_CWD", &working_dir);

    hide_tokio_console(&mut cmd);

    let child = cmd.spawn()
        .map_err(|e| format!("啟動 Discord Bot 失敗: {}", e))?;
    let pid = child.id();

    let mut bot = state.discord_bot.lock().await;

    eprintln!(
        "[Discord Bot] ✅ 已啟動 (pid: {:?}, source={}, asset_dir={}, config_dir={})",
        pid,
        runtime.source_label,
        runtime.asset_dir.display(),
        runtime.config_dir.display()
    );
    *bot = Some(child);
    Ok(runtime_status(&runtime, true, true, pid))
}

#[tauri::command]
async fn stop_discord_bot(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<DiscordBotStatus, String> {
    let mut bot = state.discord_bot.lock().await;
    if let Some(ref mut child) = *bot {
        let _ = child.kill().await;
        eprintln!("[Discord Bot] 🛑 bot_xiaokui.py 已停止");
        *bot = None;
        let mut status = inspect_discord_bot_runtime(&app);
        status.running = false;
        status.owned_by_gui = false;
        status.pid = None;
        return Ok(status);
    }
    *bot = None;
    drop(bot);

    let external_pid = find_external_discord_bot_pid().await;
    if external_pid.is_some() {
        return Err("Discord Bot 目前是外部啟動，GUI 不會直接幫你關掉它。".to_string());
    }

    let mut status = inspect_discord_bot_runtime(&app);
    status.running = false;
    status.owned_by_gui = false;
    status.pid = None;
    Ok(status)
}

#[tauri::command]
async fn get_discord_bot_status(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<DiscordBotStatus, String> {
    Ok(get_discord_bot_status_internal(&app, &state).await)
}

// ── Tauri Commands: Discord 活動 JSONL Watcher ─────

#[tauri::command]
async fn start_discord_watcher(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let generation = state.watcher_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let generation_counter = state.watcher_generation.clone();
    let app_clone = app.clone();

    tokio::spawn(async move {
        // watched: 檔案路徑 → 已讀到的 offset
        let mut watched: std::collections::HashMap<std::path::PathBuf, u64> = std::collections::HashMap::new();
        // 啟動前已存在的檔案（這些跳過舊內容）
        let mut preexisting: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
        let mut in_discord_turn = false;

        // 記錄啟動前已存在的檔案
        if let Some(home) = dirs::home_dir() {
            let sessions_dir = home.join(".codex").join("sessions");
            let mut all: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
            collect_jsonl_files(&sessions_dir, &mut all);
            for (path, _) in &all {
                preexisting.insert(path.clone());
            }
        }
        eprintln!("[Discord Watcher] 啟動，已知 {} 個既有檔案", preexisting.len());

        loop {
            if generation_counter.load(Ordering::Relaxed) != generation {
                eprintln!("[Discord Watcher] 已停止（generation {}）", generation);
                break;
            }

            if let Some(home) = dirs::home_dir() {
                let sessions_dir = home.join(".codex").join("sessions");
                if sessions_dir.exists() {
                    let mut all_files: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
                    collect_jsonl_files(&sessions_dir, &mut all_files);
                    all_files.sort_by(|a, b| b.1.cmp(&a.1));

                    for (path, _) in all_files.into_iter().take(3) {
                        let current_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

                        if !watched.contains_key(&path) {
                            if preexisting.contains(&path) {
                                // 啟動前就有的 → 跳過舊內容
                                watched.insert(path.clone(), current_size);
                            } else {
                                // 啟動後才產生的 → 從頭讀
                                eprintln!("[Discord Watcher] 新檔案（從頭讀）: {:?}",
                                    path.file_name().unwrap_or_default());
                                watched.insert(path.clone(), 0);
                            }
                        }

                        let last_offset = *watched.get(&path).unwrap();
                        if current_size <= last_offset {
                            continue;
                        }

                        eprintln!("[Discord Watcher] 讀取: {:?} offset {} → {}",
                            path.file_name().unwrap_or_default(), last_offset, current_size);

                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Some(new_content) = content.get(last_offset as usize..) {
                                for line in new_content.lines() {
                                    if line.trim().is_empty() { continue; }
                                    process_discord_jsonl(line, &app_clone, &mut in_discord_turn);
                                }
                            }
                        }

                        watched.insert(path, current_size);
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        }
    });

    Ok(())
}

/// 遞迴收集所有 JSONL 檔案路徑和修改時間
fn collect_jsonl_files(
    dir: &std::path::Path,
    out: &mut Vec<(std::path::PathBuf, std::time::SystemTime)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let modified = entry.metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((path, modified));
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "activity_type")]
enum DiscordActivity {
    UserMessage { text: String },
    BotResponse { text: String },
    ToolCall { name: String },
    TurnComplete,
}

/// 用「內容」判斷是否為 Discord 活動，而非 originator。
/// 追蹤 in_discord_turn 狀態：看到 📩 Discord → 進入 discord turn，
/// 看到 task_complete → 結束 discord turn。
fn process_discord_jsonl(line: &str, app: &AppHandle, in_discord_turn: &mut bool) {
    let json: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return,
    };

    let record_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match record_type {
        "response_item" => {
            let payload = json.get("payload").unwrap_or(&Value::Null);
            let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match (role, ptype) {
                ("user", "message") => {
                    let text = extract_text_content(payload);
                    // 偵測 Discord 訊息的起始標記
                    if text.contains("Discord") && text.contains("📩") {
                        eprintln!("[Discord Watcher] ✅ 偵測到 Discord 訊息");
                        *in_discord_turn = true;
                        let _ = app.emit("discord-activity", DiscordActivity::UserMessage {
                            text: text.chars().take(500).collect(),
                        });
                    }
                }
                ("assistant", "message") if *in_discord_turn => {
                    let text = extract_text_content(payload);
                    if !text.is_empty() {
                        let _ = app.emit("discord-activity", DiscordActivity::BotResponse {
                            text: text.chars().take(500).collect(),
                        });
                    }
                }
                (_, "function_call") if *in_discord_turn => {
                    let name = payload.get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let _ = app.emit("discord-activity", DiscordActivity::ToolCall { name });
                }
                _ => {}
            }
        }
        "event_msg" => {
            let event_type = json.get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            if event_type == "task_complete" && *in_discord_turn {
                let _ = app.emit("discord-activity", DiscordActivity::TurnComplete);
                *in_discord_turn = false;
            }
        }
        _ => {}
    }
}

fn extract_text_content(payload: &Value) -> String {
    payload.get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let t = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if t == "input_text" || t == "output_text" {
                        item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn is_meta_user_message(text: &str) -> bool {
    let trimmed = text.trim_start();
    [
        "# AGENTS.md instructions",
        "# Current Workspace Directory",
        "<environment_context>",
        "<INSTRUCTIONS>",
        "<SYSTEM>",
    ].iter().any(|prefix| trimmed.starts_with(prefix))
}

fn extract_title_from_user_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() || is_meta_user_message(trimmed) {
        return String::new();
    }

    if trimmed.contains("📩 Discord 通知") {
        let mut in_message_body = false;
        for line in trimmed.lines() {
            let current = line.trim();
            if current == "訊息內容：" {
                in_message_body = true;
                continue;
            }
            if !in_message_body {
                continue;
            }
            if current.is_empty() {
                continue;
            }
            if current.starts_with("請用 ") || current.starts_with("已經回覆過") || current.starts_with("回覆時請保持") {
                break;
            }
            return current.chars().take(60).collect();
        }
    }

    trimmed.chars().take(60).collect()
}

fn collect_session_catalog(sessions_dir: &std::path::Path) -> HashMap<String, SessionCatalogEntry> {
    let mut catalog = HashMap::new();
    if !sessions_dir.exists() {
        return catalog;
    }

    let Ok(years) = fs::read_dir(sessions_dir) else { return catalog };
    for year in years.flatten() {
        if !year.path().is_dir() { continue; }
        let Ok(months) = fs::read_dir(year.path()) else { continue };
        for month in months.flatten() {
            if !month.path().is_dir() { continue; }
            let Ok(days) = fs::read_dir(month.path()) else { continue };
            for day in days.flatten() {
                if !day.path().is_dir() { continue; }
                let Ok(files) = fs::read_dir(day.path()) else { continue };
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }

                    let Ok(content) = fs::read_to_string(&path) else { continue };
                    let mut entry = SessionCatalogEntry::default();

                    for line in content.lines().take(80) {
                        let Ok(obj) = serde_json::from_str::<Value>(line) else { continue };
                        let rec_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

                        if rec_type == "session_meta" {
                            if let Some(payload) = obj.get("payload") {
                                entry.id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                entry.cwd = payload.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                entry.originator = payload.get("originator").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                entry.modified = payload.get("timestamp").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            }
                            continue;
                        }

                        if rec_type != "response_item" || !entry.title.is_empty() {
                            continue;
                        }

                        let Some(payload) = obj.get("payload") else { continue };
                        let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
                        let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if role != "user" || ptype != "message" {
                            continue;
                        }

                        let text = extract_text_content(payload);
                        let title = extract_title_from_user_text(&text);
                        if !title.is_empty() {
                            entry.title = title;
                        }
                    }

                    if entry.id.is_empty() {
                        continue;
                    }

                    if entry.modified.is_empty() {
                        entry.modified = file.metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .map(|t| {
                                let dt: chrono::DateTime<chrono::Utc> = t.into();
                                dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                            })
                            .unwrap_or_default();
                    }

                    catalog.insert(entry.id.clone(), entry);
                }
            }
        }
    }

    catalog
}

// ── Tauri Commands: Thread 列表 ─────────────────────

#[tauri::command]
async fn list_threads(
    state: tauri::State<'_, AppState>,
) -> Result<Value, String> {
    let mut catalog: HashMap<String, SessionCatalogEntry> = HashMap::new();
    let mut session_map: HashMap<String, SessionListEntry> = HashMap::new();

    if let Some(home) = dirs::home_dir() {
        let sessions_dir = home.join(".codex").join("sessions");
        catalog = collect_session_catalog(&sessions_dir);
    }

    for (id, entry) in &catalog {
        session_map.insert(
            id.clone(),
            SessionListEntry {
                session_id: id.clone(),
                title: if entry.title.is_empty() {
                    "(unnamed)".to_string()
                } else {
                    entry.title.clone()
                },
                modified: entry.modified.clone(),
                display_label: id.clone(),
            },
        );
    }

    // 1. 先從 app-server 取目前 loaded 的 threads
    if let Ok(result) = codex::list_threads(&state.codex_process).await {
        if let Some(threads) = result.get("threads").and_then(|t| t.as_array()) {
            for t in threads {
                if let Some(id) = t.get("id").and_then(|i| i.as_str()) {
                    let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let updated = t.get("updatedAt").and_then(|u| u.as_str()).unwrap_or("");
                    let entry = session_map.entry(id.to_string()).or_insert_with(|| {
                        let catalog_entry = catalog.get(id);
                        SessionListEntry {
                            session_id: id.to_string(),
                            title: catalog_entry
                                .and_then(|c| (!c.title.is_empty()).then(|| c.title.clone()))
                                .unwrap_or_else(|| "(unnamed)".to_string()),
                            modified: catalog_entry
                                .map(|c| c.modified.clone())
                                .unwrap_or_default(),
                            display_label: id.to_string(),
                        }
                    });
                    if !name.trim().is_empty() {
                        entry.title = name.to_string();
                    }
                    if !updated.trim().is_empty() {
                        entry.modified = updated.to_string();
                    }
                }
            }
        }
    }

    // 2. 從 session_index.jsonl 補齊
    if let Some(home) = dirs::home_dir() {
        let index_path = home.join(".codex").join("session_index.jsonl");
        if let Ok(content) = fs::read_to_string(&index_path) {
            for line in content.lines() {
                if line.trim().is_empty() { continue; }
                if let Ok(entry) = serde_json::from_str::<Value>(line) {
                    let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if id.is_empty() { continue; }

                    let thread_name = entry.get("thread_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let updated_at = entry.get("updated_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let session = session_map.entry(id.clone()).or_insert_with(|| {
                        let catalog_entry = catalog.get(&id);
                        SessionListEntry {
                            session_id: id.clone(),
                            title: catalog_entry
                                .and_then(|c| (!c.title.is_empty()).then(|| c.title.clone()))
                                .unwrap_or_else(|| "(unnamed)".to_string()),
                            modified: catalog_entry
                                .map(|c| c.modified.clone())
                                .unwrap_or_default(),
                            display_label: id.clone(),
                        }
                    });
                    if session.title == "(unnamed)" && !thread_name.trim().is_empty() {
                        session.title = thread_name;
                    }
                    if session.modified.trim().is_empty() && !updated_at.trim().is_empty() {
                        session.modified = updated_at;
                    }
                }
            }
        }
    }

    let mut sessions: Vec<SessionListEntry> = session_map.into_values().collect();

    // 依修改時間倒序
    sessions.sort_by(|a, b| {
        let modified_cmp = b.modified.cmp(&a.modified);
        if modified_cmp == std::cmp::Ordering::Equal {
            a.title.cmp(&b.title)
        } else {
            modified_cmp
        }
    });

    Ok(json!({ "sessions": sessions }))
}

// ── Tauri Commands: 載入歷史訊息 ────────────────────

#[tauri::command]
async fn load_thread_history(
    thread_id: String,
) -> Result<Value, String> {
    let home = dirs::home_dir().ok_or("找不到 home 目錄")?;
    let sessions_dir = home.join(".codex").join("sessions");

    // 找到對應的 JSONL 檔案（filename 尾巴包含 thread_id）
    let file_path = find_session_file(&sessions_dir, &thread_id)
        .ok_or_else(|| format!("找不到 thread {} 的 session 檔案", thread_id))?;

    let content = fs::read_to_string(&file_path)
        .map_err(|e| format!("讀取檔案失敗: {}", e))?;

    let mut messages: Vec<Value> = Vec::new();

    for line in content.lines() {
        if line.trim().is_empty() { continue; }
        let Ok(obj) = serde_json::from_str::<Value>(line) else { continue };

        let rec_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if rec_type != "response_item" { continue; }

        let payload = obj.get("payload").unwrap_or(&Value::Null);
        let role = payload.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

        if ptype != "message" { continue; }

        let text = extract_text_content(payload);
        if text.is_empty() { continue; }

        // 跳過系統/developer 訊息
        if role == "developer" { continue; }
        if role == "user" && is_meta_user_message(&text) { continue; }

        let is_discord = text.contains("📩 Discord");

        if role == "user" || role == "assistant" {
            messages.push(json!({
                "role": role,
                "text": text.chars().take(1000).collect::<String>(),
                "source": if is_discord { "discord" } else { "gui" },
            }));
        }
    }

    Ok(json!({ "messages": messages }))
}

fn find_session_file(sessions_dir: &std::path::Path, session_id: &str) -> Option<std::path::PathBuf> {
    if !sessions_dir.exists() { return None; }
    for year in fs::read_dir(sessions_dir).ok()?.flatten() {
        if !year.path().is_dir() { continue; }
        for month in fs::read_dir(year.path()).ok().into_iter().flatten().flatten() {
            if !month.path().is_dir() { continue; }
            for day in fs::read_dir(month.path()).ok().into_iter().flatten().flatten() {
                if !day.path().is_dir() { continue; }
                for file in fs::read_dir(day.path()).ok().into_iter().flatten().flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
                            if name.ends_with(session_id) {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

// ── Tauri Commands: 圖片暫存 ────────────────────────

#[tauri::command]
fn save_temp_image_png(png_data: Vec<u8>) -> Result<String, String> {
    use std::io::Write;

    let temp_dir = std::env::temp_dir().join("xiaokui_alive");
    if !temp_dir.exists() {
        fs::create_dir_all(&temp_dir)
            .map_err(|e| format!("Failed to create temp directory: {}", e))?;
    }

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
    let filename = format!("clipboard_{}.png", timestamp);
    let file_path = temp_dir.join(&filename);

    let mut file = fs::File::create(&file_path)
        .map_err(|e| format!("Failed to create file: {}", e))?;
    file.write_all(&png_data)
        .map_err(|e| format!("Failed to write PNG data: {}", e))?;

    Ok(file_path.to_string_lossy().to_string())
}

#[tauri::command]
fn cleanup_temp_image(file_path: String) -> Result<(), String> {
    let path = PathBuf::from(&file_path);
    let temp_dir = std::env::temp_dir().join("xiaokui_alive");
    if !path.starts_with(&temp_dir) {
        return Err("Cannot delete file outside temp directory".to_string());
    }
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete temp file: {}", e))?;
    }
    Ok(())
}

// ── Tauri App 啟動 ──────────────────────────────────

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            codex_process: Arc::new(Mutex::new(codex::CodexProcess::default())),
            working_dir: Arc::new(Mutex::new(".".to_string())),
            model: Arc::new(Mutex::new(None)),
            discord_bot: Arc::new(Mutex::new(None)),
            watcher_generation: Arc::new(AtomicU64::new(0)),
        })
        .invoke_handler(tauri::generate_handler![
            start_codex,
            stop_codex,
            create_thread,
            resume_thread,
            send_prompt,
            start_discord_bot,
            stop_discord_bot,
            get_discord_bot_status,
            start_discord_watcher,
            list_threads,
            load_thread_history,
            save_temp_image_png,
            cleanup_temp_image,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
