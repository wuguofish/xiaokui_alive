use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::process::Command;
use tokio::sync::{Mutex, oneshot};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

// ── CodexEvent（發送到前端）──────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event_type")]
pub enum CodexEvent {
    Connected,
    ThreadReady {
        thread_id: String,
    },
    TextDelta {
        delta: String,
    },
    TextComplete {
        text: String,
    },
    ItemStarted {
        item_type: String,
        item_id: String,
        details: Value,
    },
    ItemCompleted {
        item_type: String,
        item_id: String,
        text: Option<String>,
        details: Value,
    },
    TurnComplete {
        turn_id: String,
    },
    Error {
        message: String,
    },
    ProcessExited,
}

// ── WebSocket writer type ───────────────────────────

type WsWriter = Arc<Mutex<
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>
        >,
        WsMessage,
    >
>>;

// ── CodexProcess 狀態管理 ───────────────────────────

pub struct CodexProcess {
    pub child: Option<tokio::process::Child>,
    ws_writer: Option<WsWriter>,
    next_request_id: u64,
    pending_requests: HashMap<u64, oneshot::Sender<Result<Value, String>>>,
    pub current_thread_id: Option<String>,
    pub port: u16,
}

impl Default for CodexProcess {
    fn default() -> Self {
        Self {
            child: None,
            ws_writer: None,
            next_request_id: 0,
            pending_requests: HashMap::new(),
            current_thread_id: None,
            port: 45888,
        }
    }
}

// ── 找到 codex 執行檔 ──────────────────────────────

fn resolve_codex_path() -> Result<String, String> {
    if let Ok(path) = which::which("codex.cmd") {
        return Ok(path.to_string_lossy().to_string());
    }
    if let Ok(path) = which::which("codex.exe") {
        return Ok(path.to_string_lossy().to_string());
    }
    if let Ok(path) = which::which("codex") {
        return Ok(path.to_string_lossy().to_string());
    }

    // Fallback: Codex CLI ships its own binary into `~/.codex/.sandbox-bin/`
    // when installed via the official installer. That directory is *not* on
    // PATH by default, so `which` misses it even though a fully-working CLI
    // is right there. Probe it explicitly before giving up.
    if let Some(home) = dirs::home_dir() {
        let candidates = if cfg!(windows) {
            vec!["codex.exe", "codex.cmd", "codex"]
        } else {
            vec!["codex"]
        };
        for name in candidates {
            let p = home.join(".codex").join(".sandbox-bin").join(name);
            if p.is_file() {
                return Ok(p.to_string_lossy().to_string());
            }
        }
    }

    Err("找不到 codex 執行檔，請確認已安裝 Codex CLI".to_string())
}

// ── Windows: 隱藏子程序 console ────────────────────

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}

// ── JSON-RPC 送出（透過 WebSocket）─────────────────

async fn send_request(
    process: &Arc<Mutex<CodexProcess>>,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let (tx, rx) = oneshot::channel();
    let request_id;

    {
        let mut proc = process.lock().await;
        proc.next_request_id += 1;
        request_id = proc.next_request_id;
        proc.pending_requests.insert(request_id, tx);

        let msg = json!({
            "id": request_id,
            "method": method,
            "params": params,
        });

        let writer = proc.ws_writer.as_ref()
            .ok_or("WebSocket 尚未連線")?;
        let text = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
        writer.lock().await.send(WsMessage::Text(text.into()))
            .await
            .map_err(|e| format!("WebSocket 送出失敗: {}", e))?;
    }

    eprintln!("[Codex] → request id={} method={}", request_id, method);

    rx.await.map_err(|_| "request channel 已關閉".to_string())?
}

/// 送出 notification（無 id，不等回應）
async fn send_notification(
    process: &Arc<Mutex<CodexProcess>>,
    method: &str,
    params: Value,
) -> Result<(), String> {
    let proc = process.lock().await;
    let msg = json!({
        "method": method,
        "params": params,
    });

    let writer = proc.ws_writer.as_ref()
        .ok_or("WebSocket 尚未連線")?;
    let text = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    writer.lock().await.send(WsMessage::Text(text.into()))
        .await
        .map_err(|e| format!("WebSocket 送出失敗: {}", e))?;

    eprintln!("[Codex] → notification method={}", method);
    Ok(())
}

fn apply_model(params: &mut Value, model: Option<&str>) {
    let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        return;
    };

    if let Some(obj) = params.as_object_mut() {
        obj.insert("model".to_string(), json!(model));
    }
}

// ── WebSocket reader 背景任務 ───────────────────────

fn spawn_ws_reader(
    mut reader: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>
        >
    >,
    process: Arc<Mutex<CodexProcess>>,
    app: AppHandle,
) {
    tokio::spawn(async move {
        while let Some(msg_result) = reader.next().await {
            match msg_result {
                Ok(WsMessage::Text(text)) => {
                    let text_str: &str = &text;
                    if text_str.trim().is_empty() {
                        continue;
                    }
                    handle_message(text_str, &process, &app).await;
                }
                Ok(WsMessage::Close(_)) => {
                    eprintln!("[Codex] WebSocket 已關閉");
                    let _ = app.emit("codex-event", CodexEvent::ProcessExited);
                    break;
                }
                Ok(_) => {} // Ping/Pong/Binary — ignore
                Err(e) => {
                    eprintln!("[Codex] WebSocket 錯誤: {}", e);
                    let _ = app.emit("codex-event", CodexEvent::ProcessExited);
                    break;
                }
            }
        }
    });
}

/// 背景 stderr reader（只做 log）
fn spawn_stderr_reader(stderr: tokio::process::ChildStderr) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[Codex stderr] {}", line);
        }
    });
}

// ── 訊息路由 ────────────────────────────────────────

async fn handle_message(
    text: &str,
    process: &Arc<Mutex<CodexProcess>>,
    app: &AppHandle,
) {
    let json: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[Codex] 非 JSON 資料: {} ({})", &text[..text.len().min(200)], e);
            return;
        }
    };

    // 有 id 的是 response，比對 pending_requests
    if let Some(id) = json.get("id").and_then(|v| v.as_u64()) {
        let mut proc = process.lock().await;
        if let Some(tx) = proc.pending_requests.remove(&id) {
            if let Some(error) = json.get("error") {
                let msg = error.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                eprintln!("[Codex] ← error response id={}: {}", id, msg);
                let _ = tx.send(Err(msg));
            } else {
                let result = json.get("result").cloned().unwrap_or(Value::Null);
                eprintln!("[Codex] ← response id={}", id);
                let _ = tx.send(Ok(result));
            }
        }
        return;
    }

    // 無 id 的是 notification
    let method = match json.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => return,
    };
    let params = json.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "thread/started" => {
            if let Some(thread_id) = params.get("thread")
                .and_then(|t| t.get("id"))
                .and_then(|id| id.as_str())
            {
                eprintln!("[Codex] thread/started: {}", thread_id);
                let _ = app.emit("codex-event", CodexEvent::ThreadReady {
                    thread_id: thread_id.to_string(),
                });
            }
        }
        "item/agentMessage/delta" => {
            if let Some(delta) = params.get("delta").and_then(|d| d.as_str()) {
                let _ = app.emit("codex-event", CodexEvent::TextDelta {
                    delta: delta.to_string(),
                });
            }
        }
        "item/started" => {
            let item_type = params.get("item")
                .and_then(|i| i.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            let item_id = params.get("item")
                .and_then(|i| i.get("id"))
                .and_then(|id| id.as_str())
                .unwrap_or("")
                .to_string();

            if item_type != "agentMessage" {
                eprintln!("[Codex] item/started: type={} id={}", item_type, item_id);
                let _ = app.emit("codex-event", CodexEvent::ItemStarted {
                    item_type,
                    item_id,
                    details: params.get("item").cloned().unwrap_or(Value::Null),
                });
            }
        }
        "item/completed" => {
            let item_type = params.get("item")
                .and_then(|i| i.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            let item_id = params.get("item")
                .and_then(|i| i.get("id"))
                .and_then(|id| id.as_str())
                .unwrap_or("")
                .to_string();
            let text = params.get("item")
                .and_then(|i| i.get("text"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());

            if item_type == "agentMessage" {
                let _ = app.emit("codex-event", CodexEvent::TextComplete {
                    text: text.unwrap_or_default(),
                });
            } else {
                eprintln!("[Codex] item/completed: type={} id={}", item_type, item_id);
                let _ = app.emit("codex-event", CodexEvent::ItemCompleted {
                    item_type,
                    item_id,
                    text,
                    details: params.get("item").cloned().unwrap_or(Value::Null),
                });
            }
        }
        "turn/completed" => {
            let turn_id = params.get("turn")
                .and_then(|t| t.get("id"))
                .and_then(|id| id.as_str())
                .unwrap_or("")
                .to_string();
            eprintln!("[Codex] turn/completed: {}", turn_id);
            let _ = app.emit("codex-event", CodexEvent::TurnComplete { turn_id });
        }
        "error" => {
            let msg = serde_json::to_string(&params).unwrap_or_default();
            eprintln!("[Codex] error notification: {}", msg);
            let _ = app.emit("codex-event", CodexEvent::Error { message: msg });
        }
        "mcpServer/startupStatus/updated" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("?");
            let status = params.get("status").and_then(|s| s.as_str()).unwrap_or("?");
            eprintln!("[Codex] MCP server {}: {}", name, status);
        }
        _ => {
            eprintln!("[Codex] unhandled notification: {}", method);
        }
    }
}

// ── 公開 API ────────────────────────────────────────

/// 啟動 codex app-server（WebSocket）+ 初始化握手
pub async fn start(
    app: AppHandle,
    process: Arc<Mutex<CodexProcess>>,
    working_dir: &str,
    port: u16,
) -> Result<(), String> {
    // 先停止舊的
    stop(process.clone()).await?;

    let codex_path = resolve_codex_path()?;
    let ws_url = format!("ws://127.0.0.1:{}", port);
    eprintln!("[Codex] 使用: {} ({})", codex_path, ws_url);

    // 1. Spawn app-server with WebSocket listener
    let mut cmd = Command::new(&codex_path);
    cmd.args(["app-server", "--listen", &format!("ws://0.0.0.0:{}", port)]);
    cmd.current_dir(working_dir);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());
    hide_console(&mut cmd);

    let mut child = cmd.spawn()
        .map_err(|e| format!("啟動 codex app-server 失敗: {}", e))?;

    let stderr = child.stderr.take();
    if let Some(stderr) = stderr {
        spawn_stderr_reader(stderr);
    }

    // 儲存 child process
    {
        let mut proc = process.lock().await;
        proc.child = Some(child);
        proc.port = port;
        proc.next_request_id = 0;
        proc.pending_requests.clear();
        proc.current_thread_id = None;
    }

    // 2. 等待 app-server 啟動，然後連接 WebSocket（重試）
    let mut connected = false;
    for attempt in 1..=10 {
        tokio::time::sleep(tokio::time::Duration::from_millis(500 * attempt)).await;
        eprintln!("[Codex] WebSocket 連線嘗試 {}/10...", attempt);

        match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((ws_stream, _)) => {
                eprintln!("[Codex] ✅ WebSocket 已連線");
                let (writer, reader) = ws_stream.split();
                let ws_writer = Arc::new(Mutex::new(writer));

                {
                    let mut proc = process.lock().await;
                    proc.ws_writer = Some(ws_writer);
                }

                // 啟動 reader 背景任務
                spawn_ws_reader(reader, process.clone(), app.clone());
                connected = true;
                break;
            }
            Err(e) => {
                eprintln!("[Codex] WebSocket 連線失敗: {} (attempt {})", e, attempt);
            }
        }
    }

    if !connected {
        stop(process.clone()).await?;
        return Err("無法連線到 codex app-server WebSocket（10 次重試失敗）".to_string());
    }

    // 3. 初始化握手
    let init_result = send_request(&process, "initialize", json!({
        "clientInfo": {
            "name": "xiaokui_alive",
            "version": "0.2.0"
        }
    })).await?;
    eprintln!("[Codex] initialize 成功: {}", serde_json::to_string(&init_result).unwrap_or_default());

    send_notification(&process, "initialized", json!({})).await?;

    let _ = app.emit("codex-event", CodexEvent::Connected);
    eprintln!("[Codex] ✅ app-server 已連線（WebSocket port {}）", port);

    Ok(())
}

/// 停止 app-server
pub async fn stop(process: Arc<Mutex<CodexProcess>>) -> Result<(), String> {
    let mut proc = process.lock().await;

    // 關閉 WebSocket
    if let Some(ref writer) = proc.ws_writer {
        let _ = writer.lock().await.close().await;
    }
    proc.ws_writer = None;

    // Kill process
    if let Some(ref mut child) = proc.child {
        let _ = child.kill().await;
        eprintln!("[Codex] app-server 已停止");
    }
    proc.child = None;
    proc.current_thread_id = None;

    for (_, tx) in proc.pending_requests.drain() {
        let _ = tx.send(Err("app-server 已停止".to_string()));
    }
    Ok(())
}

/// 建立新 thread
pub async fn create_thread(
    process: &Arc<Mutex<CodexProcess>>,
    cwd: &str,
    model: Option<&str>,
) -> Result<String, String> {
    let mut params = json!({
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
    });
    apply_model(&mut params, model);

    let result = send_request(process, "thread/start", params).await?;

    let thread_id = result.get("thread")
        .and_then(|t| t.get("id"))
        .and_then(|id| id.as_str())
        .ok_or("thread/start 回應中沒有 thread.id")?
        .to_string();

    {
        let mut proc = process.lock().await;
        proc.current_thread_id = Some(thread_id.clone());
    }

    eprintln!("[Codex] ✅ thread 已建立: {}", thread_id);
    Ok(thread_id)
}

/// 恢復已有 thread
pub async fn resume_thread(
    process: &Arc<Mutex<CodexProcess>>,
    thread_id: &str,
    cwd: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let mut params = json!({
        "threadId": thread_id,
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
    });
    apply_model(&mut params, model);

    send_request(process, "thread/resume", params).await?;

    {
        let mut proc = process.lock().await;
        proc.current_thread_id = Some(thread_id.to_string());
    }

    eprintln!("[Codex] ✅ thread 已恢復: {}", thread_id);
    Ok(())
}

/// 送出 prompt（開始新 turn）
pub async fn send_prompt(
    process: &Arc<Mutex<CodexProcess>>,
    thread_id: &str,
    text: &str,
    image_paths: &[String],
    cwd: &str,
    model: Option<&str>,
) -> Result<String, String> {
    let mut input = Vec::new();
    if !text.trim().is_empty() {
        input.push(json!({
            "type": "text",
            "text": text,
        }));
    }
    for path in image_paths {
        input.push(json!({
            "type": "localImage",
            "path": path,
        }));
    }
    if input.is_empty() {
        return Err("turn/start 至少要有文字或圖片輸入".to_string());
    }

    let mut params = json!({
        "threadId": thread_id,
        "input": input,
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandboxPolicy": {
            "type": "dangerFullAccess"
        }
    });
    apply_model(&mut params, model);

    let result = send_request(process, "turn/start", params).await?;

    let turn_id = result.get("turn")
        .and_then(|t| t.get("id"))
        .and_then(|id| id.as_str())
        .unwrap_or("")
        .to_string();

    eprintln!("[Codex] ✅ turn 已開始: {}", turn_id);
    Ok(turn_id)
}

/// 列出可用 threads
pub async fn list_threads(
    process: &Arc<Mutex<CodexProcess>>,
) -> Result<Value, String> {
    send_request(process, "thread/list", json!({})).await
}
