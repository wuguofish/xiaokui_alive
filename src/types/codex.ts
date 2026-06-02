// ── Codex App-Server 事件（從 Rust 後端 emit）──────

export interface CodexEvent {
  event_type:
    | 'Connected'
    | 'ThreadReady'
    | 'TextDelta'
    | 'TextComplete'
    | 'ItemStarted'
    | 'ItemCompleted'
    | 'TurnComplete'
    | 'Error'
    | 'ProcessExited'
  thread_id?: string
  delta?: string
  text?: string
  item_type?: string // "commandExecution", "toolCall", "mcpToolCall", "fileChange"
  item_id?: string
  details?: Record<string, unknown>
  turn_id?: string
  message?: string
}

// ── 工具使用項目 ────────────────────────────────────

export interface ToolUseItem {
  id: string
  type: string
  name: string
  input: Record<string, unknown>
  result?: string
  isRunning: boolean
}

// ── 對話項目 ────────────────────────────────────────

export type ChatItem =
  | { type: 'text'; content: string }
  | { type: 'tool'; tool: ToolUseItem }

// ── 訊息 ────────────────────────────────────────────

export interface Message {
  role: 'user' | 'assistant'
  items: ChatItem[]
  source?: 'discord'
}

// ── Avatar 狀態 ─────────────────────────────────────

export type AvatarState =
  | 'idle'
  | 'working'
  | 'thinking'
  | 'complete'
  | 'error'
  | 'asking'

// ── Session 列表項目 ────────────────────────────────

export interface SessionEntry {
  sessionId: string
  title: string
  modified: string
  displayLabel?: string
}

// ── Discord 活動 ────────────────────────────────────

export interface DiscordActivity {
  activity_type: 'UserMessage' | 'BotResponse' | 'ToolCall' | 'TurnComplete'
  text?: string
  name?: string
}
