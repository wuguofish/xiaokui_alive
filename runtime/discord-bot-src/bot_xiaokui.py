"""小葵 Discord Listener Bot — Codex app-server 方案

監聽 Discord @mention / reply / DM，透過 Codex app-server 維持
per-channel thread，讓小葵可持續對話並保留 thread 級別上下文。
"""
import asyncio
import contextlib
import json
import logging
import os
import shutil
import subprocess
import sys
from abc import ABC, abstractmethod
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
import discord
import websockets
from dotenv import load_dotenv

# ── 設定 ──────────────────────────────────────────────

BOT_DIR = Path(os.getenv("XIAOKUI_BOT_DIR", Path(__file__).resolve().parent)).resolve()
TEMP_DIR = BOT_DIR / "temp"
TEMP_DIR.mkdir(exist_ok=True)

load_dotenv(BOT_DIR / ".env.xiaokui")

TOKEN = os.getenv("DISCORD_TOKEN_XIAOKUI")
BOT_USER_ID = int(os.getenv("BOT_USER_ID_XIAOKUI", "0"))
CODEX_PATH = os.getenv("CODEX_PATH", "codex")
CODEX_MODEL = (os.getenv("CODEX_MODEL") or "").strip() or None
CODEX_CWD = Path(os.getenv("CODEX_CWD", ".")).resolve()
CODEX_TURN_TIMEOUT_SEC = int(os.getenv("CODEX_TURN_TIMEOUT_SEC", "0") or "0")
CODEX_TRANSPORT = os.getenv("CODEX_TRANSPORT", "stdio").strip().lower() or "stdio"
CODEX_WS_URL = os.getenv("CODEX_WS_URL", "ws://127.0.0.1:45888").strip()
CODEX_CONNECT_RETRIES = max(1, int(os.getenv("CODEX_CONNECT_RETRIES", "4") or "4"))
CODEX_RETRY_BASE_SEC = max(1, int(os.getenv("CODEX_RETRY_BASE_SEC", "2") or "2"))
THREAD_MAP_PATH = TEMP_DIR / "xiaokui_threads.json"

with open(BOT_DIR / "config_xiaokui.json", encoding="utf-8") as f:
    CONFIG = json.load(f)

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%H:%M:%S",
)
log = logging.getLogger("xiaokui-bot")

# ── Access Control ────────────────────────────────────

recent_bot_messages: set[int] = set()
MAX_RECENT = 200


def remember_bot_message(message_id: int) -> None:
    recent_bot_messages.add(message_id)
    while len(recent_bot_messages) > MAX_RECENT:
        recent_bot_messages.pop()


def should_respond(message: discord.Message) -> bool:
    """判斷是否應該觸發。"""
    if message.author.id == BOT_USER_ID:
        return False

    if message.author.bot:
        channel_id = str(getattr(message.channel, "id", 0))
        ch_config = CONFIG.get("channels", {}).get(channel_id, {})
        if not ch_config.get("allowBotMention", False):
            return False
        if any(u.id == BOT_USER_ID for u in message.mentions):
            return True
        return False

    if isinstance(message.channel, discord.DMChannel):
        if CONFIG.get("dmPolicy") != "allowlist":
            return False
        return str(message.author.id) in CONFIG.get("allowFrom", [])

    channel_id = str(message.channel.id)
    ch_config = CONFIG.get("channels", {}).get(channel_id)
    if not ch_config:
        return False

    ch_allow = ch_config.get("allowFrom", [])
    if ch_allow and str(message.author.id) not in ch_allow:
        return False

    if ch_config.get("requireMention", True):
        if any(u.id == BOT_USER_ID for u in message.mentions):
            return True
        if message.reference and message.reference.message_id:
            if message.reference.message_id in recent_bot_messages:
                return True
            if message.reference.resolved and hasattr(message.reference.resolved, "author"):
                if message.reference.resolved.author.id == BOT_USER_ID:
                    return True
        return False

    return True


# ── Prompt 組裝 ───────────────────────────────────────

TW = timezone(timedelta(hours=8))


def build_prompt(message: discord.Message) -> str:
    """組裝給 Codex 的 prompt。"""
    now = datetime.now(TW).strftime("%Y-%m-%d %H:%M")

    if isinstance(message.channel, discord.DMChannel):
        channel_info = "私訊 (DM)"
    else:
        ch_name = message.channel.name if hasattr(message.channel, "name") else "unknown"
        channel_info = f"#{ch_name} ({message.channel.id})"

    display_name = message.author.display_name or message.author.name

    return f"""📩 Discord 通知
頻道：{channel_info}
來自：{display_name} (user_id: {message.author.id})
訊息 ID：{message.id}
時間：{now}

訊息內容：
{message.content or "(無文字)"}

請用 discord_read 工具讀取頻道 {message.channel.id} 了解上下文，
完成你的任務和相關操作後，
如有回覆的必要再用 discord_reply 工具回覆訊息 {message.id} 或按reaction。
已經回覆過的不用再回覆。
回覆時請保持小葵（夏葵心）的語氣和角色。"""


# ── Thread Map ────────────────────────────────────────

class ThreadMapStore:
    """持久化 Discord channel 與 Codex thread 的對應。"""

    def __init__(self, path: Path):
        self.path = path
        self._lock = asyncio.Lock()
        self._data = self._load()

    def _load(self) -> dict[str, str]:
        if not self.path.exists():
            return {}
        try:
            raw = json.loads(self.path.read_text(encoding="utf-8"))
            if isinstance(raw, dict):
                return {str(k): str(v) for k, v in raw.items()}
        except Exception as exc:
            log.warning(f"讀取 thread map 失敗，改用空白狀態: {exc}")
        return {}

    async def _save(self) -> None:
        temp_path = self.path.with_suffix(".tmp")
        temp_path.write_text(
            json.dumps(self._data, ensure_ascii=False, indent=2),
            encoding="utf-8",
        )
        temp_path.replace(self.path)

    def get(self, channel_id: int | str) -> str | None:
        return self._data.get(str(channel_id))

    async def set(self, channel_id: int | str, thread_id: str) -> None:
        async with self._lock:
            self._data[str(channel_id)] = thread_id
            await self._save()

    async def delete(self, channel_id: int | str) -> None:
        async with self._lock:
            if str(channel_id) in self._data:
                self._data.pop(str(channel_id), None)
                await self._save()


class CodexAppServerError(RuntimeError):
    """Codex app-server 溝通錯誤。"""


@dataclass
class CodexRunResult:
    """一次 Codex turn 執行結果。"""

    ok: bool
    thread_id: str | None = None
    turn_id: str | None = None
    attempts: int = 0
    error_message: str | None = None


def find_command_with_where(candidates: list[str]) -> str | None:
    """Windows 上用 where.exe 補抓 PowerShell 可見但 CreateProcess 不可見的指令。"""
    for candidate in candidates:
        try:
            result = subprocess.run(
                ["where.exe", candidate],
                check=False,
                capture_output=True,
                text=True,
            )
        except Exception:
            continue

        for line in result.stdout.splitlines():
            path = line.strip()
            if path:
                return path
    return None


def resolve_codex_command(codex_path: str) -> list[str]:
    """把 CODEX_PATH 解析成 Windows / Unix 都可直接 exec 的命令列。"""
    target = (codex_path or "codex").strip().strip('"')
    if not target:
        target = "codex"

    candidate_names = [target]
    target_path = Path(os.path.expandvars(os.path.expanduser(target)))
    if os.name == "nt" and target_path.suffix == "":
        candidate_names.extend(
            [
                f"{target}.cmd",
                f"{target}.exe",
                f"{target}.ps1",
                f"{target}.bat",
            ]
        )

    resolved: str | None = None
    for candidate in candidate_names:
        expanded = Path(os.path.expandvars(os.path.expanduser(candidate)))
        if expanded.exists():
            resolved = str(expanded.resolve())
            break

        found = shutil.which(str(expanded))
        if found:
            resolved = found
            break

    if resolved is None and os.name == "nt":
        resolved = find_command_with_where(candidate_names)

    if resolved is None:
        return [target]

    if Path(resolved).suffix.lower() == ".ps1":
        powershell_cmd = (
            shutil.which("pwsh")
            or shutil.which("powershell")
            or shutil.which("powershell.exe")
        )
        if not powershell_cmd:
            raise CodexAppServerError(
                "找到 codex.ps1，但系統上找不到 pwsh / powershell 可執行檔。"
            )
        return [powershell_cmd, "-ExecutionPolicy", "Bypass", "-File", resolved]

    return [resolved]


# ── Transport ─────────────────────────────────────────

OnMessage = Callable[[dict], Awaitable[None]]
OnDisconnect = Callable[[Exception], Awaitable[None]]


class CodexTransport(ABC):
    """Codex app-server transport abstraction."""

    def __init__(self):
        self._on_message: OnMessage | None = None
        self._on_disconnect: OnDisconnect | None = None
        self._closing = False

    async def connect(self, on_message: OnMessage, on_disconnect: OnDisconnect) -> None:
        self._on_message = on_message
        self._on_disconnect = on_disconnect
        self._closing = False
        await self._connect()

    @abstractmethod
    async def _connect(self) -> None:
        raise NotImplementedError

    @abstractmethod
    async def send(self, payload: dict) -> None:
        raise NotImplementedError

    @abstractmethod
    async def close(self) -> None:
        raise NotImplementedError

    @abstractmethod
    def is_connected(self) -> bool:
        raise NotImplementedError

    @property
    @abstractmethod
    def name(self) -> str:
        raise NotImplementedError

    async def _dispatch_message(self, data: dict) -> None:
        if self._on_message:
            await self._on_message(data)

    async def _dispatch_disconnect(self, exc: Exception) -> None:
        if self._closing:
            return
        if self._on_disconnect:
            await self._on_disconnect(exc)


class StdioTransport(CodexTransport):
    """透過 subprocess stdio 與 app-server 溝通。"""

    def __init__(self, codex_path: str, cwd: Path):
        super().__init__()
        self.codex_path = codex_path
        self.cwd = cwd
        self._process: asyncio.subprocess.Process | None = None
        self._stdout_task: asyncio.Task | None = None
        self._stderr_task: asyncio.Task | None = None
        self._write_lock = asyncio.Lock()

    @property
    def name(self) -> str:
        return "stdio"

    async def _connect(self) -> None:
        launch_cmd = resolve_codex_command(self.codex_path)
        log.info(f"啟動 codex app-server（stdio 常駐）: {launch_cmd[0]}")
        try:
            self._process = await asyncio.create_subprocess_exec(
                *launch_cmd,
                "app-server",
                "--listen",
                "stdio://",
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                cwd=str(self.cwd),
            )
        except FileNotFoundError as exc:
            raise CodexAppServerError(
                "找不到 codex CLI。請確認 `CODEX_PATH` 指向可執行的 codex、codex.cmd、"
                "codex.exe 或 codex.ps1。"
            ) from exc
        self._stdout_task = asyncio.create_task(self._stdout_loop(self._process))
        self._stderr_task = asyncio.create_task(self._stderr_loop(self._process))

    def is_connected(self) -> bool:
        return self._process is not None and self._process.returncode is None

    async def _stdout_loop(self, proc: asyncio.subprocess.Process) -> None:
        assert proc.stdout is not None
        try:
            while True:
                raw = await proc.stdout.readline()
                if not raw:
                    break

                line = raw.decode("utf-8", errors="replace").strip()
                if not line:
                    continue

                try:
                    data = json.loads(line)
                except json.JSONDecodeError:
                    log.warning(f"app-server 傳回非 JSON 資料: {line[:200]}")
                    continue

                await self._dispatch_message(data)
        except asyncio.CancelledError:
            return
        finally:
            if proc is self._process:
                await self._dispatch_disconnect(CodexAppServerError("Codex app-server stdio 連線已中斷"))

    async def _stderr_loop(self, proc: asyncio.subprocess.Process) -> None:
        assert proc.stderr is not None
        try:
            while True:
                raw = await proc.stderr.readline()
                if not raw:
                    break
                line = raw.decode("utf-8", errors="replace").rstrip()
                if line:
                    log.info(f"[codex] {line}")
        except asyncio.CancelledError:
            return

    async def send(self, payload: dict) -> None:
        if not self._process or not self._process.stdin:
            raise CodexAppServerError("Codex app-server stdio 尚未啟動")

        body = (json.dumps(payload, ensure_ascii=False) + "\n").encode("utf-8")
        async with self._write_lock:
            self._process.stdin.write(body)
            await self._process.stdin.drain()

    async def close(self) -> None:
        self._closing = True
        proc = self._process
        self._process = None

        for task in (self._stdout_task, self._stderr_task):
            if task:
                task.cancel()
        self._stdout_task = None
        self._stderr_task = None

        if not proc:
            return

        if proc.stdin:
            proc.stdin.close()
            with contextlib.suppress(Exception):
                await proc.stdin.wait_closed()

        if proc.returncode is None:
            proc.terminate()
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(proc.wait(), timeout=5)
        if proc.returncode is None:
            proc.kill()
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(proc.wait(), timeout=5)


class WebSocketTransport(CodexTransport):
    """透過 WebSocket 與 app-server 溝通。"""

    def __init__(self, url: str):
        super().__init__()
        self.url = url
        self._ws = None
        self._reader_task: asyncio.Task | None = None
        self._write_lock = asyncio.Lock()

    @property
    def name(self) -> str:
        return "ws"

    async def _connect(self) -> None:
        log.info(f"連線 codex app-server（WebSocket）: {self.url}")
        self._ws = await websockets.connect(
            self.url,
            max_size=10 * 1024 * 1024,
            ping_interval=20,
            ping_timeout=20,
        )
        self._reader_task = asyncio.create_task(self._reader_loop())

    def is_connected(self) -> bool:
        return self._ws is not None

    async def _reader_loop(self) -> None:
        if not self._ws:
            return
        try:
            while True:
                raw = await self._ws.recv()
                if isinstance(raw, bytes):
                    raw = raw.decode("utf-8", errors="replace")
                data = json.loads(raw)
                await self._dispatch_message(data)
        except asyncio.CancelledError:
            return
        except Exception as exc:
            await self._dispatch_disconnect(CodexAppServerError(f"Codex app-server ws 連線已中斷: {exc}"))
        finally:
            self._ws = None

    async def send(self, payload: dict) -> None:
        if not self._ws:
            raise CodexAppServerError("Codex app-server ws 尚未連線")

        async with self._write_lock:
            await self._ws.send(json.dumps(payload, ensure_ascii=False))

    async def close(self) -> None:
        self._closing = True
        reader_task = self._reader_task
        self._reader_task = None
        ws = self._ws
        self._ws = None

        if reader_task:
            reader_task.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await reader_task

        if ws:
            with contextlib.suppress(Exception):
                await ws.close()


def create_transport(mode: str, codex_path: str, cwd: Path, ws_url: str) -> CodexTransport:
    if mode == "ws":
        return WebSocketTransport(ws_url)
    if mode != "stdio":
        log.warning(f"未知 transport={mode}，回退到 stdio")
    return StdioTransport(codex_path, cwd)


# ── Codex RPC Client ──────────────────────────────────

class CodexAppServerClient:
    """共用 JSON-RPC 與 thread/turn 管理，底下 transport 可切 stdio 或 ws。"""

    def __init__(
        self,
        transport: CodexTransport,
        cwd: Path,
        model: str | None,
        turn_timeout_sec: int = 0,
        connect_retries: int = 4,
        retry_base_sec: int = 2,
    ):
        self.transport = transport
        self.cwd = cwd
        self.model = model
        self.turn_timeout_sec = turn_timeout_sec
        self.connect_retries = max(1, connect_retries)
        self.retry_base_sec = max(1, retry_base_sec)

        self._start_lock = asyncio.Lock()
        self._request_lock = asyncio.Lock()
        self._next_request_id = 0
        self._pending_requests: dict[int, asyncio.Future] = {}
        self._turn_waiters: dict[str, asyncio.Future] = {}
        self._completed_turns: dict[str, dict] = {}
        self._loaded_threads: set[str] = set()

    def _apply_model(self, params: dict) -> dict:
        if self.model:
            params["model"] = self.model
        return params

    async def ensure_started(self) -> None:
        async with self._start_lock:
            if self.transport.is_connected():
                return

            self._fail_pending(CodexAppServerError("Codex app-server 已重啟，上一輪等待中止"))
            self._loaded_threads.clear()
            self._completed_turns.clear()
            last_error: Exception | None = None

            for attempt in range(1, self.connect_retries + 1):
                try:
                    await self.transport.connect(self._handle_message, self._handle_transport_disconnect)
                    await self._initialize_connection()
                    await self.healthcheck()
                    log.info(
                        f"Codex app-server 已初始化（transport={self.transport.name}, attempt={attempt}）"
                    )
                    return
                except Exception as exc:
                    last_error = exc
                    await self.transport.close()
                    wait_sec = self.retry_base_sec * (2 ** (attempt - 1))
                    if attempt >= self.connect_retries:
                        break
                    log.warning(
                        "Codex app-server 連線失敗 "
                        f"(transport={self.transport.name}, attempt={attempt}/{self.connect_retries})：{exc}；"
                        f" {wait_sec} 秒後重試"
                    )
                    await asyncio.sleep(wait_sec)

            raise CodexAppServerError(
                f"Codex app-server 初始化失敗（transport={self.transport.name}）：{last_error}"
            )

    async def _initialize_connection(self) -> None:
        async with self._request_lock:
            self._next_request_id += 1
            request_id = self._next_request_id

        loop = asyncio.get_running_loop()
        future = loop.create_future()
        self._pending_requests[request_id] = future

        await self.transport.send(
            {
                "id": request_id,
                "method": "initialize",
                "params": {"clientInfo": {"name": "xiaokui_discord", "version": "3.0.0"}},
            }
        )
        await future
        await self.transport.send({"method": "initialized", "params": {}})

    async def _handle_transport_disconnect(self, exc: Exception) -> None:
        self._fail_pending(exc)
        self._loaded_threads.clear()
        self._completed_turns.clear()
        log.warning(str(exc))

    async def healthcheck(self) -> None:
        """用一個輕量 request 確認 transport 與 app-server 都真的活著。"""
        result = await self._send_request_internal("model/list", {}, skip_ensure=True)
        models = result.get("models", []) if isinstance(result, dict) else []
        if not isinstance(models, list):
            raise CodexAppServerError("Codex app-server 健康檢查失敗：model/list 回傳格式異常")
        log.info(
            "Codex app-server 健康檢查通過 "
            f"(transport={self.transport.name}, models={len(models)})"
        )

    async def _handle_message(self, data: dict) -> None:
        if "id" in data:
            request_id = data["id"]
            future = self._pending_requests.pop(request_id, None)
            if future and not future.done():
                if "error" in data:
                    future.set_exception(CodexAppServerError(json.dumps(data["error"], ensure_ascii=False)))
                else:
                    future.set_result(data.get("result"))
            return

        method = data.get("method")
        params = data.get("params", {})
        if not method:
            return

        if method == "thread/started":
            thread = params.get("thread", {})
            thread_id = thread.get("id")
            if thread_id:
                self._loaded_threads.add(thread_id)
            return

        if method == "mcpServer/startupStatus/updated":
            name = params.get("name", "unknown")
            status = params.get("status", "unknown")
            error = params.get("error")
            if error:
                log.warning(f"MCP {name} 狀態：{status} ({error})")
            else:
                log.info(f"MCP {name} 狀態：{status}")
            return

        if method == "item/agentMessage/delta":
            delta = params.get("delta", "")
            if delta:
                log.debug(f"Codex delta: {delta}")
            return

        if method == "item/completed":
            item = params.get("item", {})
            if item.get("type") == "agentMessage":
                phase = item.get("phase") or "unknown"
                text = item.get("text", "").strip()
                if text:
                    preview = text.replace("\n", " ")[:120]
                    log.info(f"Codex {phase}: {preview}")
            return

        if method == "turn/completed":
            turn = params.get("turn", {})
            turn_id = turn.get("id")
            if turn_id:
                waiter = self._turn_waiters.pop(turn_id, None)
                if waiter and not waiter.done():
                    waiter.set_result(turn)
                else:
                    self._completed_turns[turn_id] = turn
            return

        if method == "thread/status/changed":
            thread_id = params.get("threadId")
            status = params.get("status", {})
            log.info(f"thread {thread_id} 狀態：{status}")
            return

        if method == "error":
            log.warning(f"app-server error 通知: {params}")

    def _fail_pending(self, exc: Exception) -> None:
        for future in list(self._pending_requests.values()):
            if not future.done():
                future.set_exception(exc)
        self._pending_requests.clear()

        for future in list(self._turn_waiters.values()):
            if not future.done():
                future.set_exception(exc)
        self._turn_waiters.clear()

    async def _send_request_internal(self, method: str, params: dict, skip_ensure: bool = False) -> dict:
        if not skip_ensure:
            await self.ensure_started()
        async with self._request_lock:
            self._next_request_id += 1
            request_id = self._next_request_id

        loop = asyncio.get_running_loop()
        future = loop.create_future()
        self._pending_requests[request_id] = future

        await self.transport.send({"id": request_id, "method": method, "params": params})
        return await future

    async def _send_request(self, method: str, params: dict) -> dict:
        return await self._send_request_internal(method, params, skip_ensure=False)

    async def create_thread(self) -> str:
        result = await self._send_request(
            "thread/start",
            self._apply_model({
                "cwd": str(self.cwd),
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
            }),
        )
        thread_id = result["thread"]["id"]
        self._loaded_threads.add(thread_id)
        log.info(f"建立新 thread: {thread_id}")
        return thread_id

    async def ensure_thread_loaded(self, thread_id: str) -> None:
        await self.ensure_started()
        if thread_id in self._loaded_threads:
            return

        await self._send_request(
            "thread/resume",
            self._apply_model({
                "threadId": thread_id,
                "cwd": str(self.cwd),
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
            }),
        )
        self._loaded_threads.add(thread_id)
        log.info(f"恢復既有 thread: {thread_id}")

    async def start_turn(self, thread_id: str, prompt: str) -> dict:
        result = await self._send_request(
            "turn/start",
            self._apply_model({
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt}],
                "cwd": str(self.cwd),
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "dangerFullAccess"},
            }),
        )

        turn = result["turn"]
        turn_id = turn["id"]
        if turn.get("status") != "inProgress":
            return turn

        completed = self._completed_turns.pop(turn_id, None)
        if completed is not None:
            return completed

        loop = asyncio.get_running_loop()
        waiter = loop.create_future()
        self._turn_waiters[turn_id] = waiter

        try:
            if self.turn_timeout_sec > 0:
                return await asyncio.wait_for(waiter, timeout=self.turn_timeout_sec)
            return await waiter
        except asyncio.TimeoutError as exc:
            self._turn_waiters.pop(turn_id, None)
            raise CodexAppServerError(
                f"turn 執行超時（{self.turn_timeout_sec} 秒）"
            ) from exc

    async def close(self) -> None:
        self._fail_pending(CodexAppServerError("Codex app-server 已關閉"))
        self._loaded_threads.clear()
        self._completed_turns.clear()
        await self.transport.close()


thread_store = ThreadMapStore(THREAD_MAP_PATH)
codex_transport = create_transport(
    mode=CODEX_TRANSPORT,
    codex_path=CODEX_PATH,
    cwd=CODEX_CWD,
    ws_url=CODEX_WS_URL,
)
codex_client = CodexAppServerClient(
    transport=codex_transport,
    cwd=CODEX_CWD,
    model=CODEX_MODEL,
    turn_timeout_sec=CODEX_TURN_TIMEOUT_SEC,
    connect_retries=CODEX_CONNECT_RETRIES,
    retry_base_sec=CODEX_RETRY_BASE_SEC,
)


async def get_or_create_thread(channel_id: int) -> str:
    """取回既有 thread，失效時自動重建。"""
    thread_id = thread_store.get(channel_id)
    if thread_id:
        try:
            await codex_client.ensure_thread_loaded(thread_id)
            return thread_id
        except Exception as exc:
            log.warning(f"既有 thread 無法恢復，改建新 thread: {exc}")
            await thread_store.delete(channel_id)

    thread_id = await codex_client.create_thread()
    await thread_store.set(channel_id, thread_id)
    return thread_id


async def run_codex(channel_id: int, prompt: str) -> CodexRunResult:
    """透過常駐 app-server 在對應 thread 上執行 turn。"""
    last_error: Exception | None = None

    for attempt in range(2):
        try:
            thread_id = await get_or_create_thread(channel_id)
            turn = await codex_client.start_turn(thread_id, prompt)
            status = turn.get("status")
            turn_id = turn.get("id")

            if status == "completed":
                log.info(f"turn 完成（thread={thread_id}, turn={turn.get('id')})")
                return CodexRunResult(
                    ok=True,
                    thread_id=thread_id,
                    turn_id=turn_id,
                    attempts=attempt + 1,
                )

            error = turn.get("error") or {}
            log.warning(f"turn 未完成（status={status}）: {error}")
            return CodexRunResult(
                ok=False,
                thread_id=thread_id,
                turn_id=turn_id,
                attempts=attempt + 1,
                error_message=error.get("message") or f"turn status={status}",
            )

        except Exception as exc:
            last_error = exc
            log.warning(f"Codex app-server 執行失敗（attempt {attempt + 1}/2）: {exc}")
            if attempt == 0:
                continue

    error_text = str(last_error) if last_error else "unknown error"
    log.error(f"Codex 最終失敗: {error_text}")
    return CodexRunResult(
        ok=False,
        attempts=2,
        error_message=error_text,
    )


# ── Discord Client ────────────────────────────────────

intents = discord.Intents.default()
intents.message_content = True
intents.dm_messages = True

client = discord.Client(intents=intents)

channel_locks: dict[int, asyncio.Lock] = {}


def get_lock(channel_id: int) -> asyncio.Lock:
    if channel_id not in channel_locks:
        channel_locks[channel_id] = asyncio.Lock()
    return channel_locks[channel_id]


async def send_backend_failure_reply(message: discord.Message, error_message: str | None) -> None:
    """當 backend 沒能完成回覆時，用 bot 自己補一則簡短說明。"""
    detail = (error_message or "backend unavailable").strip()
    if len(detail) > 120:
        detail = detail[:117] + "..."

    summary, retryable = classify_backend_error(error_message)
    reply_text = summary

    if retryable:
        reply_text += "\n你再丟我一次，我這邊重新整理後接。"
    else:
        reply_text += "\n我先把這次狀態記住，你晚點換個問法丟我也可以。"

    if detail:
        reply_text += f"\n\n附註：{detail}"

    try:
        sent = await message.reply(reply_text, mention_author=False)
        remember_bot_message(sent.id)
    except Exception as exc:
        log.warning(f"補發 fallback 回覆失敗: {exc}")


def classify_backend_error(error_message: str | None) -> tuple[str, bool]:
    """把底層錯誤整理成比較像 Discord 現場會看的說法。"""
    detail = (error_message or "").strip()
    lowered = detail.lower()

    if not detail:
        return ("我這邊剛剛沒接到後台回覆，這次先沒接上。", True)

    if "超時" in detail or "timeout" in lowered:
        return ("我這邊剛剛整理太久，被系統先收線了。", True)

    if any(token in lowered for token in ("connection refused", "10061", "failed to connect", "cannot connect")):
        return ("我這邊剛剛沒接上後台服務，像是櫃台那邊暫時沒人接電話。", True)

    if any(token in lowered for token in ("disconnect", "closed", "broken pipe", "eof", "handshake")):
        return ("我這邊剛剛跟後台斷線了一下，這次沒順利送出回覆。", True)

    if "thread" in lowered and any(token in lowered for token in ("resume", "load", "not found", "invalid")):
        return ("我剛剛那條對話脈絡掉了一下，這次先沒順利接回來。", True)

    if any(token in lowered for token in ("permission", "forbidden", "unauthorized", "denied")):
        return ("我這邊剛剛卡到權限限制，這次先沒辦法幫你送出去。", False)

    return ("我這邊剛剛跟後台協調失敗，這次先沒順利送出回覆。", True)


async def safe_add_reaction(message: discord.Message, emoji: str) -> None:
    try:
        await message.add_reaction(emoji)
    except Exception as exc:
        log.debug(f"新增 reaction 失敗 ({emoji}): {exc}")


async def safe_remove_reaction(message: discord.Message, emoji: str) -> None:
    if not client.user:
        return

    try:
        await message.remove_reaction(emoji, client.user)
    except Exception as exc:
        log.debug(f"移除 reaction 失敗 ({emoji}): {exc}")


@client.event
async def on_ready():
    log.info(f"小葵 Listener Bot 已上線: {client.user} (ID: {client.user.id})")
    try:
        await codex_client.ensure_started()
    except Exception as exc:
        log.warning(f"Codex app-server 預熱失敗，收到訊息時會再重試: {exc}")


@client.event
async def on_disconnect():
    await codex_client.close()


@client.event
async def on_message(message: discord.Message):
    if message.author.id == BOT_USER_ID:
        remember_bot_message(message.id)
        return

    if not should_respond(message):
        return

    log.info(f"觸發通知: {message.author.display_name} 在 {getattr(message.channel, 'name', 'DM')}")

    lock = get_lock(message.channel.id)
    async with lock:
        prompt = build_prompt(message)
        log.info(f"prompt 預覽: {prompt[:100]}...")
        await safe_add_reaction(message, "⏳")
        result: CodexRunResult | None = None
        log.info(f"開始處理 Discord 訊息: channel={message.channel.id}, message={message.id}")

        try:
            async with message.channel.typing():
                result = await run_codex(message.channel.id, prompt)
        except Exception as exc:
            log.exception(f"run_codex 發生未預期錯誤: {exc}")
            result = CodexRunResult(
                ok=False,
                attempts=1,
                error_message=str(exc),
            )
        finally:
            await safe_remove_reaction(message, "⏳")
            log.info(f"結束處理 Discord 訊息: channel={message.channel.id}, message={message.id}")

        if not result or not result.ok:
            await safe_add_reaction(message, "⚠️")
            await send_backend_failure_reply(
                message,
                result.error_message if result else "unexpected run_codex error",
            )
            return

        await safe_add_reaction(message, "✅")


# ── 啟動 ──────────────────────────────────────────────

def main():
    if not TOKEN:
        print("錯誤：請在 .env.xiaokui 中設定 DISCORD_TOKEN_XIAOKUI", file=sys.stderr)
        sys.exit(1)
    if not BOT_USER_ID:
        print("錯誤：請在 .env.xiaokui 中設定 BOT_USER_ID_XIAOKUI", file=sys.stderr)
        sys.exit(1)

    log.info(
        "啟動小葵 Listener Bot... "
        f"(transport={CODEX_TRANSPORT}, ws_url={CODEX_WS_URL if CODEX_TRANSPORT == 'ws' else 'n/a'})"
    )
    client.run(TOKEN, log_handler=None)


if __name__ == "__main__":
    main()
