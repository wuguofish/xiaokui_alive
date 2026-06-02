/**
 * Codex 事件處理邏輯
 * 將 CodexEvent 映射到 UI 狀態變更
 */
import type { CodexEvent, Message, ToolUseItem, AvatarState } from '../types/codex'

// ── 可變狀態參考（由 App.vue 傳入）────────────────

export interface AppStateRefs {
  messages: Message[]
  streamingText: string
  currentToolUses: ToolUseItem[]
  isLoading: boolean
  isProcessAlive: boolean
  threadId: string | null
  avatarState: AvatarState
}

// ── 事件處理結果 ────────────────────────────────────

export interface StateUpdates {
  messages?: Message[]
  streamingText?: string
  currentToolUses?: ToolUseItem[]
  isLoading?: boolean
  isProcessAlive?: boolean
  threadId?: string | null
  avatarState?: AvatarState
}

export type EventAction =
  | { type: 'scrollToBottom' }
  | { type: 'startCompleteTimer' }

export interface EventResult {
  stateUpdates: StateUpdates
  actions: EventAction[]
}

// ── 主要事件處理器 ──────────────────────────────────

export function handleCodexEvent(
  event: CodexEvent,
  state: AppStateRefs,
): EventResult {
  switch (event.event_type) {
    case 'Connected':
      return handleConnected()
    case 'ThreadReady':
      return handleThreadReady(event)
    case 'TextDelta':
      return handleTextDelta(event, state)
    case 'TextComplete':
      return handleTextComplete(event, state)
    case 'ItemStarted':
      return handleItemStarted(event, state)
    case 'ItemCompleted':
      return handleItemCompleted(event, state)
    case 'TurnComplete':
      return handleTurnComplete()
    case 'Error':
      return handleError(event)
    case 'ProcessExited':
      return handleProcessExited()
    default:
      return { stateUpdates: {}, actions: [] }
  }
}

// ── 各事件處理函數 ──────────────────────────────────

function handleConnected(): EventResult {
  return {
    stateUpdates: { isProcessAlive: true },
    actions: [],
  }
}

function handleThreadReady(event: CodexEvent): EventResult {
  return {
    stateUpdates: { threadId: event.thread_id ?? null },
    actions: [],
  }
}

function handleTextDelta(event: CodexEvent, state: AppStateRefs): EventResult {
  const delta = event.delta ?? ''
  const newStreamingText = state.streamingText + delta

  // 確保最後一條 message 是 assistant，且有 text item
  const messages = [...state.messages]
  let lastMsg = messages[messages.length - 1]

  if (!lastMsg || lastMsg.role !== 'assistant') {
    lastMsg = { role: 'assistant', items: [{ type: 'text', content: '' }] }
    messages.push(lastMsg)
  }

  // 找到最後一個 text item 並更新
  const lastTextItem = [...lastMsg.items].reverse().find(i => i.type === 'text')
  if (lastTextItem && lastTextItem.type === 'text') {
    lastTextItem.content = newStreamingText
  } else {
    lastMsg.items.push({ type: 'text', content: newStreamingText })
  }

  return {
    stateUpdates: {
      messages,
      streamingText: newStreamingText,
      avatarState: 'thinking',
    },
    actions: [{ type: 'scrollToBottom' }],
  }
}

function handleTextComplete(event: CodexEvent, state: AppStateRefs): EventResult {
  const finalText = event.text ?? state.streamingText

  // 更新最後的 assistant message 的 text
  const messages = [...state.messages]
  const lastMsg = messages[messages.length - 1]
  if (lastMsg && lastMsg.role === 'assistant') {
    const lastTextItem = [...lastMsg.items].reverse().find(i => i.type === 'text')
    if (lastTextItem && lastTextItem.type === 'text') {
      lastTextItem.content = finalText
    }
  }

  return {
    stateUpdates: {
      messages,
      streamingText: '',
    },
    actions: [{ type: 'scrollToBottom' }],
  }
}

function handleItemStarted(event: CodexEvent, state: AppStateRefs): EventResult {
  const tool: ToolUseItem = {
    id: event.item_id ?? '',
    type: event.item_type ?? 'unknown',
    name: extractToolName(event),
    input: (event.details as Record<string, unknown>) ?? {},
    isRunning: true,
  }

  const currentToolUses = [...state.currentToolUses, tool]

  // 加到 messages
  const messages = [...state.messages]
  let lastMsg = messages[messages.length - 1]
  if (!lastMsg || lastMsg.role !== 'assistant') {
    lastMsg = { role: 'assistant', items: [] }
    messages.push(lastMsg)
  }
  // 如果有 streaming text 正在進行，先結束它
  if (state.streamingText) {
    // streamingText 已經在 text item 裡了，不用額外處理
  }
  lastMsg.items.push({ type: 'tool', tool })

  return {
    stateUpdates: {
      messages,
      currentToolUses,
      streamingText: '',
      avatarState: 'working',
    },
    actions: [{ type: 'scrollToBottom' }],
  }
}

function handleItemCompleted(event: CodexEvent, state: AppStateRefs): EventResult {
  const itemId = event.item_id ?? ''
  const resultText = event.text ?? ''

  // 更新 currentToolUses
  const currentToolUses = state.currentToolUses.map(t =>
    t.id === itemId
      ? { ...t, result: resultText, isRunning: false }
      : t,
  )

  // 更新 messages 中對應的 tool
  const messages = state.messages.map(msg => ({
    ...msg,
    items: msg.items.map(item => {
      if (item.type === 'tool' && item.tool.id === itemId) {
        return {
          type: 'tool' as const,
          tool: { ...item.tool, result: resultText, isRunning: false },
        }
      }
      return item
    }),
  }))

  return {
    stateUpdates: { messages, currentToolUses },
    actions: [{ type: 'scrollToBottom' }],
  }
}

function handleTurnComplete(): EventResult {
  return {
    stateUpdates: {
      isLoading: false,
      currentToolUses: [],
      avatarState: 'complete',
    },
    actions: [{ type: 'startCompleteTimer' }, { type: 'scrollToBottom' }],
  }
}

function handleError(_event: CodexEvent): EventResult {
  return {
    stateUpdates: {
      isLoading: false,
      avatarState: 'error',
    },
    actions: [],
  }
}

function handleProcessExited(): EventResult {
  return {
    stateUpdates: {
      isProcessAlive: false,
      isLoading: false,
      avatarState: 'idle',
    },
    actions: [],
  }
}

// ── 工具名稱提取 ────────────────────────────────────

function extractToolName(event: CodexEvent): string {
  const details = event.details ?? {}
  // 嘗試從 details 中提取名稱
  if (typeof details.name === 'string') return details.name
  if (typeof details.call_id === 'string') return details.call_id
  // fallback 到 item_type
  return event.item_type ?? 'unknown'
}
