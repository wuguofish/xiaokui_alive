<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick, computed, watch } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'
import { renderMarkdown } from './utils/markdown'
import { handleCodexEvent, type AppStateRefs } from './utils/codexEventHandler'
import type { CodexEvent, DiscordActivity, Message, ToolUseItem, AvatarState, SessionEntry } from './types/codex'
import ToolIndicator from './components/ToolIndicator.vue'
// SessionSelector 在 launch screen 和 running screen 的 header 使用
// import SessionSelector from './components/SessionSelector.vue'

// === 版本號 ===
const appVersion = __APP_VERSION__

// === 階段狀態 ===
type AppPhase = 'launch' | 'connecting' | 'running'
const phase = ref<AppPhase>('launch')

// === 啟動參數 ===
const launchMode = ref<'new' | 'resume'>('new')
const workingDir = ref('.')
const modelOptions = [
  { value: '', label: 'Codex 設定檔' },
  { value: 'gpt-5.6', label: 'GPT-5.6 Sol' },
  { value: 'gpt-5.6-terra', label: 'GPT-5.6 Terra' },
  { value: 'gpt-5.6-luna', label: 'GPT-5.6 Luna' },
  { value: 'gpt-5.5', label: 'GPT-5.5' },
  { value: 'gpt-5.4', label: 'GPT-5.4' },
  { value: 'gpt-5.4-mini', label: 'GPT-5.4 Mini' },
  { value: 'gpt-5.3-codex-spark', label: 'GPT-5.3 Codex Spark' },
  { value: 'o3', label: 'o3' },
  { value: 'o4-mini', label: 'o4-mini' },
  { value: 'gpt-4.1', label: 'GPT-4.1' },
  { value: 'gpt-4.1-mini', label: 'GPT-4.1-mini' },
]
const selectedModel = ref('')
const wsPort = ref(45888)
const resumeThreadId = ref('')

// === Discord Bot ===
const discordBotRunning = ref(false)
const discordBotOwnedByGui = ref(false)
const discordBotPid = ref<number | null>(null)
const discordBotConfigDir = ref('')
const discordBotEnvPath = ref('')
const discordBotAssetDir = ref('')
const discordBotSourceLabel = ref('')
const discordBotLaunchTarget = ref('')

interface DiscordBotStatus {
  running: boolean
  owned_by_gui: boolean
  pid?: number | null
  config_dir?: string | null
  env_path?: string | null
  asset_dir?: string | null
  source_label?: string | null
  launch_target?: string | null
}

function applyDiscordBotStatus(status: DiscordBotStatus) {
  discordBotRunning.value = status.running
  discordBotOwnedByGui.value = status.owned_by_gui
  discordBotPid.value = status.pid ?? null
  discordBotConfigDir.value = status.config_dir ?? ''
  discordBotEnvPath.value = status.env_path ?? ''
  discordBotAssetDir.value = status.asset_dir ?? ''
  discordBotSourceLabel.value = status.source_label ?? ''
  discordBotLaunchTarget.value = status.launch_target ?? ''
}

async function refreshDiscordBotStatus() {
  try {
    const status = await invoke<DiscordBotStatus>('get_discord_bot_status')
    applyDiscordBotStatus(status)
  } catch {
    discordBotRunning.value = false
    discordBotOwnedByGui.value = false
    discordBotPid.value = null
    discordBotConfigDir.value = ''
    discordBotEnvPath.value = ''
    discordBotAssetDir.value = ''
    discordBotSourceLabel.value = ''
    discordBotLaunchTarget.value = ''
  }
}

function addSystemMessage(text: string) {
  messages.value.push({
    role: 'assistant',
    items: [{ type: 'text', content: text }],
  })
  nextTick(() => {
    if (chatAreaRef.value) {
      chatAreaRef.value.scrollTop = chatAreaRef.value.scrollHeight
    }
  })
}

// === Discord 活動（合併到主聊天） ===
function addDiscordMessage(role: 'user' | 'assistant', text: string) {
  messages.value.push({
    role,
    items: [{ type: 'text', content: text }],
    source: 'discord',
  })
  nextTick(() => {
    if (chatAreaRef.value) {
      chatAreaRef.value.scrollTop = chatAreaRef.value.scrollHeight
    }
  })
}

async function toggleDiscordBot() {
  try {
    if (discordBotRunning.value) {
      const status = await invoke<DiscordBotStatus>('stop_discord_bot')
      applyDiscordBotStatus(status)
    } else {
      const status = await invoke<DiscordBotStatus>('start_discord_bot')
      applyDiscordBotStatus(status)
      if (status.running && !status.owned_by_gui) {
        addSystemMessage('Discord Bot 本來就在外面跑了，我這邊先接狀態，不另外重開。')
      }
    }
  } catch (err) {
    console.error('切換 Discord Bot 失敗:', err)
    await refreshDiscordBotStatus()
    addSystemMessage(`Discord Bot 狀態切換失敗：${err}`)
  }
}

// 定期檢查 bot 狀態
let botCheckTimer: ReturnType<typeof setInterval> | null = null

// === Session 列表 ===
const sessions = ref<SessionEntry[]>([])
const sessionsLoading = ref(false)
const sessionSearch = ref('')

const filteredSessions = computed(() => {
  const keyword = sessionSearch.value.trim().toLowerCase()
  if (!keyword) return sessions.value
  return sessions.value.filter((session) => {
    const title = (session.title || '').toLowerCase()
    const id = (session.sessionId || '').toLowerCase()
    const label = (session.displayLabel || '').toLowerCase()
    return title.includes(keyword) || id.includes(keyword) || label.includes(keyword)
  })
})

// === 核心狀態 ===
const threadId = ref<string | null>(null)
const messages = ref<Message[]>([])
const streamingText = ref('')
const currentToolUses = ref<ToolUseItem[]>([])
const isLoading = ref(false)
const isProcessAlive = ref(false)

// === 輸入 ===
const userInput = ref('')
const chatAreaRef = ref<HTMLDivElement>()

// === 貼上圖片 ===
interface AttachedImage {
  id: string
  path: string
  name: string
  previewUrl?: string
  isLoading: boolean
}
const attachedImages = ref<AttachedImage[]>([])
let imageIdCounter = 0

async function handlePaste(e: ClipboardEvent) {
  const items = e.clipboardData?.items
  if (!items) return

  let imageFile: File | null = null
  for (const item of items) {
    if (item.type.startsWith('image/')) {
      imageFile = item.getAsFile()
      break
    }
  }
  if (!imageFile) return

  e.preventDefault()

  const id = `img_${++imageIdCounter}_${Date.now()}`
  const timestamp = new Date().toLocaleTimeString('zh-TW', { hour: '2-digit', minute: '2-digit', second: '2-digit' })
  const ext = imageFile.type.split('/')[1] || 'png'
  const name = `截圖_${timestamp}.${ext}`

  attachedImages.value.push({ id, path: '', name, isLoading: true })

  try {
    const previewUrl = URL.createObjectURL(imageFile)
    const item = attachedImages.value.find(img => img.id === id)
    if (item) item.previewUrl = previewUrl

    const arrayBuffer = await imageFile.arrayBuffer()
    const pngData = Array.from(new Uint8Array(arrayBuffer))
    const filePath = await invoke<string>('save_temp_image_png', { pngData })

    if (item) { item.path = filePath; item.isLoading = false }
  } catch (err) {
    console.error('Failed to process clipboard image:', err)
    const index = attachedImages.value.findIndex(img => img.id === id)
    if (index !== -1) attachedImages.value.splice(index, 1)
  }
}

async function removeImage(id: string) {
  const index = attachedImages.value.findIndex(img => img.id === id)
  if (index === -1) return
  const img = attachedImages.value[index]
  if (img.previewUrl) URL.revokeObjectURL(img.previewUrl)
  if (img.path) {
    try { await invoke('cleanup_temp_image', { filePath: img.path }) } catch { /* ignore */ }
  }
  attachedImages.value.splice(index, 1)
}

function formatAttachmentSummary(text: string, images: AttachedImage[]) {
  const parts: string[] = []
  if (text) parts.push(text)
  if (images.length > 0) {
    const label = images.length === 1
      ? `附圖 1 張：${images[0].name}`
      : `附圖 ${images.length} 張：${images.map(img => img.name).join('、')}`
    parts.push(`[${label}]`)
  }
  return parts.join('\n')
}

async function clearAttachedImages(cleanupFiles: boolean) {
  for (const img of attachedImages.value) {
    if (img.previewUrl) URL.revokeObjectURL(img.previewUrl)
    if (cleanupFiles && img.path) {
      try {
        await invoke('cleanup_temp_image', { filePath: img.path })
      } catch {
        // ignore cleanup errors
      }
    }
  }
  attachedImages.value = []
}

// === 小葵風格忙碌狀態文字 ===
const xiaokuiThinkingTexts = [
  "查資料中", "整理想法", "腦袋轉轉", "認真看 code",
  "分析中", "搜尋答案", "思考方案", "規劃步驟",
  "讓我看看", "稍等一下", "嗯嗯", "我想想",
  "翻筆記中", "計算中", "處理中", "正在思考",
  "研究一下", "推敲中", "核對中", "加油中",
]
const thinkingSymbols = ['🌻', '💭', '🍵', '✨', '📝', '💡', '🔍', '⚡', '🌱', '🎯', '📊', '🧩']
const busyText = ref('')
let busyTextTimer: ReturnType<typeof setInterval> | null = null

function pickRandomBusyText() {
  const text = xiaokuiThinkingTexts[Math.floor(Math.random() * xiaokuiThinkingTexts.length)]
  const symbol = thinkingSymbols[Math.floor(Math.random() * thinkingSymbols.length)]
  busyText.value = `${symbol} ${text}`
}

function startBusyTextRotation() {
  pickRandomBusyText()
  if (!busyTextTimer) {
    busyTextTimer = setInterval(pickRandomBusyText, 3000)
  }
}

function stopBusyTextRotation() {
  if (busyTextTimer) { clearInterval(busyTextTimer); busyTextTimer = null }
}

// === Avatar 狀態 ===
const avatarState = ref<AvatarState>('idle')
const workingFrame = ref(0)
let workingTimer: ReturnType<typeof setInterval> | null = null
let completeTimer: ReturnType<typeof setTimeout> | null = null

const avatarSrc = computed(() => {
  switch (avatarState.value) {
    case 'idle':
      return '/character/idle.png'
    case 'working':
      return `/character/working-${(workingFrame.value % 4) + 1}.png`
    case 'thinking':
      return '/character/thinking.png'
    case 'asking':
      return '/character/asking.png'
    case 'complete':
      return '/character/complete-1.png'
    case 'error':
      return '/character/asking.png'
    default:
      return '/character/idle.png'
  }
})

watch(avatarState, (state) => {
  if (state === 'working' || state === 'thinking') {
    if (!workingTimer) {
      workingTimer = setInterval(() => { workingFrame.value++ }, 150)
    }
    startBusyTextRotation()
  } else {
    if (workingTimer) { clearInterval(workingTimer); workingTimer = null; workingFrame.value = 0 }
    stopBusyTextRotation()
  }
})

// === Codex 事件監聽 ===
const unlisteners: UnlistenFn[] = []

function getStateRefs(): AppStateRefs {
  return {
    messages: messages.value,
    streamingText: streamingText.value,
    currentToolUses: currentToolUses.value,
    isLoading: isLoading.value,
    isProcessAlive: isProcessAlive.value,
    threadId: threadId.value,
    avatarState: avatarState.value,
  }
}

function applyEventResult(result: ReturnType<typeof handleCodexEvent>) {
  const u = result.stateUpdates
  if (u.messages !== undefined) messages.value = u.messages
  if (u.streamingText !== undefined) streamingText.value = u.streamingText
  if (u.currentToolUses !== undefined) currentToolUses.value = u.currentToolUses
  if (u.isLoading !== undefined) isLoading.value = u.isLoading
  if (u.isProcessAlive !== undefined) isProcessAlive.value = u.isProcessAlive
  if (u.threadId !== undefined) threadId.value = u.threadId
  if (u.avatarState !== undefined) avatarState.value = u.avatarState

  for (const action of result.actions) {
    switch (action.type) {
      case 'scrollToBottom':
        nextTick(() => {
          if (chatAreaRef.value) {
            chatAreaRef.value.scrollTop = chatAreaRef.value.scrollHeight
          }
        })
        break
      case 'startCompleteTimer':
        if (completeTimer) clearTimeout(completeTimer)
        completeTimer = setTimeout(() => {
          avatarState.value = 'idle'
        }, 3000)
        break
    }
  }
}

// === Thread 列表（需要 app-server 連線後才能呼叫）===
async function loadThreads() {
  sessionsLoading.value = true
  try {
    const data = await invoke<{ sessions: SessionEntry[] }>('list_threads')
    sessions.value = data.sessions || []
  } catch {
    sessions.value = []
  }
  sessionsLoading.value = false
}

function selectSession(session: SessionEntry) {
  launchMode.value = 'resume'
  resumeThreadId.value = session.sessionId
}

// === 資料夾選擇 ===
async function pickWorkingDir() {
  const selected = await open({ directory: true, multiple: false })
  if (selected) {
    workingDir.value = selected as string
  }
}

// === 啟動 Codex App-Server ===
// === Step 1: 連線 app-server ===
async function connectServer() {
  phase.value = 'connecting'

  try {
    await invoke('start_codex', {
      workingDir: workingDir.value,
      model: selectedModel.value || undefined,
      port: wsPort.value,
    })

    // 啟動 Discord 活動 watcher
    await invoke('start_discord_watcher')

    // 載入 thread 列表
    await loadThreads()
  } catch (err) {
    console.error('連線失敗:', err)
    phase.value = 'launch'
  }
}

// === Step 2: 選 thread 後開始對話 ===
async function startConversation() {
  phase.value = 'running'
  await nextTick()

  try {
    if (launchMode.value === 'resume' && resumeThreadId.value.trim()) {
      const tid = resumeThreadId.value.trim()
      await invoke('resume_thread', { threadId: tid })
      threadId.value = tid

      // 載入歷史訊息
      try {
        const history = await invoke<{ messages: Array<{ role: string; text: string; source: string }> }>('load_thread_history', { threadId: tid })
        if (history.messages?.length) {
          messages.value = history.messages.map(m => ({
            role: m.role as 'user' | 'assistant',
            items: [{ type: 'text' as const, content: m.text }],
            source: m.source === 'discord' ? 'discord' as const : undefined,
          }))
          await nextTick()
          if (chatAreaRef.value) {
            chatAreaRef.value.scrollTop = chatAreaRef.value.scrollHeight
          }
        }
      } catch (e) {
        console.warn('載入歷史訊息失敗:', e)
      }
    } else {
      const newThreadId = await invoke<string>('create_thread')
      threadId.value = newThreadId
    }
  } catch (err) {
    console.error('啟動失敗:', err)
    messages.value = [{
      role: 'assistant',
      items: [{ type: 'text', content: `啟動失敗: ${err}` }],
    }]
    avatarState.value = 'error'
  }
}

// === 送出訊息 ===
async function sendMessage() {
  const text = userInput.value.trim()
  const readyImages = attachedImages.value.filter(img => !img.isLoading && img.path)
  if ((!text && readyImages.length === 0) || !threadId.value || isLoading.value) return

  // 加入 user message
  const displayText = formatAttachmentSummary(text, readyImages)
  messages.value.push({
    role: 'user',
    items: [{ type: 'text', content: displayText || '[附圖]' }],
  })

  const originalInput = userInput.value
  userInput.value = ''
  isLoading.value = true
  avatarState.value = 'thinking'
  streamingText.value = ''

  await nextTick()
  if (chatAreaRef.value) {
    chatAreaRef.value.scrollTop = chatAreaRef.value.scrollHeight
  }

  try {
    await invoke('send_prompt', {
      threadId: threadId.value,
      text,
      imagePaths: readyImages.map(img => img.path),
    })
    await clearAttachedImages(true)
  } catch (err) {
    console.error('送出失敗:', err)
    isLoading.value = false
    avatarState.value = 'error'
    userInput.value = originalInput
  }
}

function handleKeydown(e: KeyboardEvent) {
  if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') {
    e.preventDefault()
    sendMessage()
    return
  }
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    sendMessage()
  }
}

// === 生命週期 ===
onMounted(async () => {
  unlisteners.push(await listen<CodexEvent>('codex-event', (event) => {
    const result = handleCodexEvent(event.payload, getStateRefs())
    applyEventResult(result)
  }))
  // Discord 活動監聽 — 合併到主聊天
  unlisteners.push(await listen<DiscordActivity>('discord-activity', (event) => {
    const a = event.payload
    switch (a.activity_type) {
      case 'UserMessage':
        addDiscordMessage('user', a.text ?? '')
        break
      case 'BotResponse':
        addDiscordMessage('assistant', a.text ?? '')
        break
      case 'ToolCall':
        addDiscordMessage('assistant', `⚙️ ${a.name ?? 'tool'}`)
        break
      case 'TurnComplete':
        // 不需要特別顯示
        break
    }
  }))

  // 每 5 秒檢查 bot 狀態
  botCheckTimer = setInterval(async () => {
    try {
      await refreshDiscordBotStatus()
    } catch { /* ignore */ }
  }, 5000)
  await refreshDiscordBotStatus()
})

onUnmounted(() => {
  unlisteners.forEach(fn => fn())
  if (workingTimer) clearInterval(workingTimer)
  if (completeTimer) clearTimeout(completeTimer)
  if (botCheckTimer) clearInterval(botCheckTimer)
  stopBusyTextRotation()
  void clearAttachedImages(true)
  invoke('stop_discord_bot')
  invoke('stop_codex')
})
</script>

<template>
  <div class="app">
    <!-- ===== Step 1: 啟動設定 ===== -->
    <div v-if="phase === 'launch'" class="launch-screen">
      <div class="launch-left">
        <h1 class="launch-title">Xiaokui Alive</h1>
        <p class="launch-subtitle">小葵陪你寫程式</p>

        <!-- 工作目錄 -->
        <div class="option-group">
          <label class="option-label">工作目錄</label>
          <div class="dir-picker">
            <input
              v-model="workingDir"
              class="session-input dir-input"
              placeholder="工作目錄路徑"
            />
            <button class="dir-browse-btn" @click="pickWorkingDir">📂</button>
          </div>
        </div>

        <!-- Model -->
        <div class="option-group">
          <label class="option-label">Model</label>
          <select v-model="selectedModel" class="session-select">
            <option
              v-for="opt in modelOptions"
              :key="opt.value"
              :value="opt.value"
            >{{ opt.label }}</option>
          </select>
        </div>

        <!-- WebSocket Port -->
        <div class="option-group">
          <label class="option-label">WebSocket Port</label>
          <input
            v-model.number="wsPort"
            class="session-input"
            type="number"
            placeholder="45888"
          />
          <span class="port-hint">Discord Bot 也會連到這個 port</span>
        </div>

        <!-- 連線按鈕 -->
        <button class="launch-btn" @click="connectServer">
          🌻 啟動 Codex
        </button>
      </div>

      <div class="launch-avatar">
        <img :src="avatarSrc" alt="小葵" class="avatar-img-large" />
      </div>
    </div>

    <!-- ===== Step 2: 選擇對話（連線後）===== -->
    <div v-else-if="phase === 'connecting'" class="launch-screen">
      <div class="launch-left">
        <h1 class="launch-title">Xiaokui Alive</h1>
        <p class="launch-subtitle">已連線 ✅ 選擇對話</p>

        <!-- 對話模式 -->
        <div class="option-group">
          <label class="option-label">對話模式</label>
          <div class="option-buttons">
            <button
              :class="['opt-btn', { active: launchMode === 'new' }]"
              @click="launchMode = 'new'; resumeThreadId = ''"
            >新對話</button>
            <button
              :class="['opt-btn', { active: launchMode === 'resume' }]"
              @click="launchMode = 'resume'"
            >續接對話</button>
          </div>
        </div>

        <!-- Thread 列表 -->
        <div v-if="launchMode === 'resume'" class="session-list">
          <div class="session-toolbar">
            <input
              v-model="resumeThreadId"
              class="session-input"
              placeholder="貼上 thread ID 直接續接"
            />
            <input
              v-model="sessionSearch"
              class="session-input"
              placeholder="搜尋標題或 thread ID"
            />
          </div>
          <div v-if="sessionsLoading" class="session-loading">載入中...</div>
          <div v-else-if="filteredSessions.length === 0" class="session-empty">沒有符合的歷史對話</div>
          <div
            v-for="s in filteredSessions"
            :key="s.sessionId"
            :class="['session-item', { selected: resumeThreadId === s.sessionId }]"
            @click="selectSession(s)"
          >
            <div class="session-title">{{ s.title || '（無標題）' }}</div>
            <div class="session-id">{{ s.displayLabel || s.sessionId }}</div>
            <div class="session-time">{{ s.modified ? new Date(s.modified).toLocaleString('zh-TW') : '' }}</div>
          </div>
        </div>

        <!-- 開始按鈕 -->
        <button
          class="launch-btn"
          :disabled="launchMode === 'resume' && !resumeThreadId.trim()"
          @click="startConversation"
        >
          🌻 開始對話
        </button>
      </div>

      <div class="launch-avatar">
        <img :src="avatarSrc" alt="小葵" class="avatar-img-large" />
      </div>
    </div>

    <!-- ===== 運行畫面 ===== -->
    <template v-else>
      <div class="main-area">
        <!-- Avatar 側邊欄（左側）-->
        <div class="avatar-sidebar">
          <div class="avatar-wrapper">
            <img :src="avatarSrc" alt="小葵" class="avatar-img" />
          </div>
        </div>

        <!-- Chat Area -->
        <div class="chat-container">
          <div ref="chatAreaRef" class="chat-area">
            <div v-if="messages.length === 0" class="chat-empty">
              🌻 跟小葵說點什麼吧！
            </div>
            <div
              v-for="(msg, msgIndex) in messages"
              :key="msgIndex"
              :class="['message', msg.role, { discord: msg.source === 'discord' }]"
            >
              <span v-if="msg.source === 'discord'" class="discord-badge">💬 Discord</span>
              <template v-for="(item, itemIndex) in msg.items" :key="itemIndex">
                <div v-if="item.type === 'text'" class="text-item">
                  <div
                    v-if="msg.role === 'assistant'"
                    class="markdown-body"
                    v-html="renderMarkdown(item.content)"
                  />
                  <div v-else class="user-text">{{ item.content }}</div>
                </div>
                <div v-else-if="item.type === 'tool'" class="tool-item">
                  <ToolIndicator :tool="item.tool" />
                </div>
              </template>
            </div>

            <!-- Loading 指示 -->
            <div v-if="isLoading && !streamingText && currentToolUses.length === 0" class="loading-indicator">
              <span class="loading-dot" />
              <span class="loading-dot" />
              <span class="loading-dot" />
            </div>
          </div>
        </div>
      </div>

      <!-- 忙碌文字 -->
      <div class="busy-text" v-if="avatarState === 'thinking' || avatarState === 'working'">
        <span>{{ busyText }}</span>
      </div>

      <!-- 圖片預覽列 -->
      <div v-if="attachedImages.length > 0" class="image-preview-bar">
        <div v-for="img in attachedImages" :key="img.id" class="image-preview-item">
          <div v-if="img.isLoading" class="image-loading">載入中...</div>
          <template v-else>
            <img v-if="img.previewUrl" :src="img.previewUrl" :alt="img.name" class="image-thumb" />
            <span class="image-name">{{ img.name }}</span>
            <button class="image-remove" @click="removeImage(img.id)">&times;</button>
          </template>
        </div>
      </div>

      <!-- 輸入框 -->
      <div class="input-bar">
        <textarea
          v-model="userInput"
          class="input-textarea"
          placeholder="輸入訊息... (Enter 送出, Shift+Enter 換行, Ctrl+V 貼圖)"
          rows="1"
          @keydown="handleKeydown"
          @paste="handlePaste"
        />
        <button
          class="send-btn"
          :disabled="((!userInput.trim() && attachedImages.length === 0) || attachedImages.some(img => img.isLoading) || isLoading || !threadId)"
          @click="sendMessage"
        >
          送出
        </button>
      </div>

      <!-- 狀態列 -->
      <div class="status-bar">
        <span class="status-item">📁 {{ workingDir }}</span>
        <span v-if="threadId" class="status-item status-tag">🧵 {{ threadId.slice(0, 8) }}...</span>
        <span v-if="isProcessAlive" class="status-item status-tag online-tag">● ws://localhost:{{ wsPort }}</span>
        <span v-else class="status-item status-tag offline-tag">● 離線</span>
        <button
          class="discord-bot-btn"
          :class="{ active: discordBotRunning }"
          :disabled="!isProcessAlive || (discordBotRunning && !discordBotOwnedByGui)"
          @click="toggleDiscordBot"
        >
          {{ discordBotRunning ? (discordBotOwnedByGui ? '🟢 Discord Bot' : '🟡 外部 Bot') : '⚪ Discord Bot' }}
        </button>
        <span class="status-item" style="margin-left: auto;">v{{ appVersion }}</span>
      </div>
      <div class="bot-debug-bar">
        <span class="bot-debug-item">
          Bot PID: {{ discordBotPid ?? 'none' }}
        </span>
        <span class="bot-debug-item">
          Env: {{ discordBotEnvPath || '未解析' }}
        </span>
        <span class="bot-debug-item">
          Config: {{ discordBotConfigDir || '未解析' }}
        </span>
        <span class="bot-debug-item">
          Source: {{ discordBotSourceLabel || '未解析' }}
        </span>
        <span class="bot-debug-item">
          Target: {{ discordBotLaunchTarget || '未解析' }}
        </span>
        <span v-if="discordBotAssetDir" class="bot-debug-item">
          Assets: {{ discordBotAssetDir }}
        </span>
      </div>
    </template>
  </div>
</template>

<style>
@import 'highlight.js/styles/github.css';

* {
  margin: 0;
  padding: 0;
  box-sizing: border-box;
}

html, body, #app {
  height: 100%;
  overflow: hidden;
  background: #faf7f2;
  color: #3d3730;
  font-family: 'Segoe UI', sans-serif;
}

.app {
  display: flex;
  flex-direction: column;
  height: 100vh;
}

/* ===== 啟動畫面 ===== */
.launch-screen {
  flex: 1;
  display: flex;
  overflow: hidden;
}

.launch-left {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 40px;
  overflow-y: auto;
  max-width: 500px;
}

.launch-title {
  font-size: 28px;
  color: #e8922f;
  font-weight: 700;
}

.launch-subtitle {
  font-size: 14px;
  color: #a09888;
  margin-bottom: 8px;
}

.option-group {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.option-label {
  font-size: 12px;
  color: #a09888;
  text-transform: uppercase;
  letter-spacing: 1px;
}

.option-buttons {
  display: flex;
  gap: 4px;
}

.opt-btn {
  padding: 6px 14px;
  background: #ffffff;
  color: #8a8078;
  border: 1px solid #e0d8cc;
  border-radius: 4px;
  cursor: pointer;
  font-size: 13px;
  transition: all 0.15s;
}

.opt-btn:hover {
  background: #f5efe6;
  color: #5a5048;
}

.opt-btn.active {
  background: #4ade80;
  color: #1a2e1a;
  border-color: #4ade80;
  font-weight: 600;
}

.session-input {
  padding: 6px 10px;
  background: #ffffff;
  color: #3d3730;
  border: 1px solid #e0d8cc;
  border-radius: 4px;
  font-size: 13px;
  outline: none;
}

.session-input:focus,
.session-select:focus {
  border-color: #4ade80;
}

.session-select {
  padding: 6px 10px;
  background: #ffffff;
  color: #3d3730;
  border: 1px solid #e0d8cc;
  border-radius: 4px;
  font-size: 13px;
  outline: none;
  cursor: pointer;
}

.port-hint {
  font-size: 11px;
  color: #b0a898;
}

.dir-picker {
  display: flex;
  gap: 4px;
}

.dir-input {
  flex: 1;
}

.dir-browse-btn {
  padding: 6px 10px;
  background: #ffffff;
  border: 1px solid #e0d8cc;
  border-radius: 4px;
  cursor: pointer;
  font-size: 16px;
  transition: background 0.15s;
}

.dir-browse-btn:hover {
  background: #f5efe6;
}

/* Session 列表 */
.session-list {
  max-height: 200px;
  overflow-y: auto;
  border: 1px solid #e0d8cc;
  border-radius: 6px;
  background: #ffffff;
}

.session-toolbar {
  position: sticky;
  top: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 8px;
  background: #fbf8f4;
  border-bottom: 1px solid #f0ebe3;
  z-index: 1;
}

.session-item {
  padding: 8px 12px;
  cursor: pointer;
  border-bottom: 1px solid #f0ebe3;
  transition: background 0.15s;
}

.session-item:last-child {
  border-bottom: none;
}

.session-item:hover {
  background: #faf7f2;
}

.session-item.selected {
  background: #edfcf2;
  border-left: 3px solid #4ade80;
}

.session-title {
  font-size: 13px;
  color: #3d3730;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.session-time {
  font-size: 11px;
  color: #a09888;
  margin-top: 2px;
}

.session-id {
  font-size: 11px;
  color: #6f8aa0;
  margin-top: 2px;
  font-family: 'Cascadia Code', 'Fira Code', monospace;
}

.session-loading,
.session-empty {
  padding: 16px;
  text-align: center;
  color: #a09888;
  font-size: 13px;
}

.launch-btn {
  margin-top: 12px;
  padding: 12px 24px;
  background: #4ade80;
  color: #1a2e1a;
  border: none;
  border-radius: 8px;
  font-size: 16px;
  font-weight: 700;
  cursor: pointer;
  transition: background 0.2s;
}

.launch-btn:hover {
  background: #6ee7a0;
}

.launch-avatar {
  position: fixed;
  right: 0;
  bottom: 0;
  height: 80%;
  display: flex;
  align-items: flex-end;
  justify-content: flex-end;
}

.avatar-img-large {
  max-width: 100%;
  max-height: 100%;
  image-rendering: auto;
}

/* ===== 運行畫面 ===== */
.main-area {
  flex: 1;
  display: flex;
  flex-direction: row;
  overflow: hidden;
}

/* Chat Area */
.chat-container {
  flex: 1;
  overflow: hidden;
  display: flex;
  flex-direction: column;
}

.chat-area {
  flex: 1;
  overflow-y: auto;
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.chat-empty {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  color: #b0a898;
  font-size: 16px;
}

/* Messages */
.message {
  max-width: 85%;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.message.user {
  align-self: flex-end;
}

.message.assistant {
  align-self: flex-start;
}

.user-text {
  padding: 8px 14px;
  background: #e8922f;
  color: #ffffff;
  border-radius: 12px 12px 2px 12px;
  font-size: 14px;
  line-height: 1.5;
  white-space: pre-wrap;
}

.markdown-body {
  padding: 8px 14px;
  background: #ffffff;
  border: 1px solid #ece6dc;
  border-radius: 12px 12px 12px 2px;
  font-size: 14px;
  line-height: 1.6;
  overflow-x: auto;
  color: #3d3730;
}

.markdown-body p {
  margin: 0 0 8px 0;
}

.markdown-body p:last-child {
  margin-bottom: 0;
}

.markdown-body pre {
  background: #f8f4ee;
  padding: 12px;
  border-radius: 6px;
  overflow-x: auto;
  margin: 8px 0;
}

.markdown-body code {
  font-family: 'Cascadia Code', 'Fira Code', monospace;
  font-size: 13px;
}

.markdown-body :not(pre) > code {
  background: #f8f4ee;
  padding: 2px 6px;
  border-radius: 3px;
  font-size: 13px;
  color: #c05020;
}

.markdown-body ul, .markdown-body ol {
  padding-left: 20px;
  margin: 8px 0;
}

.markdown-body a {
  color: #2e9e50;
}

.tool-item {
  max-width: 100%;
}

/* Discord badge */
.message.discord {
  opacity: 0.85;
}

.discord-badge {
  display: inline-block;
  font-size: 10px;
  padding: 1px 6px;
  background: #5865f215;
  color: #5865f2;
  border: 1px solid #5865f230;
  border-radius: 3px;
  margin-bottom: 2px;
}

/* Loading */
.loading-indicator {
  display: flex;
  gap: 4px;
  padding: 8px 14px;
  align-self: flex-start;
}

.loading-dot {
  width: 8px;
  height: 8px;
  background: #4ade80;
  border-radius: 50%;
  animation: loading-bounce 1.4s ease-in-out infinite both;
}

.loading-dot:nth-child(2) { animation-delay: 0.16s; }
.loading-dot:nth-child(3) { animation-delay: 0.32s; }

@keyframes loading-bounce {
  0%, 80%, 100% { transform: scale(0.4); opacity: 0.4; }
  40% { transform: scale(1); opacity: 1; }
}

/* Avatar 側邊欄（左側）*/
.avatar-sidebar {
  width: 320px;
  min-width: 320px;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  justify-content: flex-end;
  background: #faf7f2;
  border-right: 1px solid #e0d8cc;
  overflow: hidden;
}

.avatar-wrapper {
  height: 100%;
  display: flex;
  align-items: flex-end;
  justify-content: flex-start;
}

.avatar-img {
  height: 100%;
  object-fit: contain;
  object-position: bottom left;
  image-rendering: auto;
}

/* Busy text */
.busy-text {
  padding: 8px 16px;
  font-size: 13px;
  text-align: left;
  background: #faf7f2;
}

.busy-text span {
  color: #2e9e50;
  animation: busy-fade 3s ease-in-out infinite;
}

@keyframes busy-fade {
  0%, 100% { opacity: 0.6; }
  50% { opacity: 1; }
}

/* 圖片預覽 */
.image-preview-bar {
  display: flex;
  gap: 8px;
  padding: 8px 12px 0 12px;
  background: #f5efe6;
  border-top: 1px solid #e0d8cc;
  overflow-x: auto;
}

.image-preview-item {
  position: relative;
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px;
  background: #ffffff;
  border: 1px solid #e0d8cc;
  border-radius: 6px;
  flex-shrink: 0;
}

.image-thumb {
  width: 40px;
  height: 40px;
  object-fit: cover;
  border-radius: 4px;
}

.image-name {
  font-size: 11px;
  color: #8a8078;
  max-width: 100px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.image-loading {
  font-size: 11px;
  color: #4ade80;
}

.image-remove {
  background: none;
  border: none;
  color: #e05050;
  font-size: 16px;
  cursor: pointer;
  padding: 0 2px;
  line-height: 1;
}

.image-remove:hover {
  color: #ff6666;
}

/* 輸入框 */
.input-bar {
  display: flex;
  gap: 8px;
  padding: 8px 12px;
  background: #f5efe6;
  border-top: 1px solid #e0d8cc;
}

.input-textarea {
  flex: 1;
  resize: none;
  padding: 8px 12px;
  background: #ffffff;
  color: #3d3730;
  border: 1px solid #e0d8cc;
  border-radius: 6px;
  font-family: inherit;
  font-size: 14px;
  line-height: 1.4;
  outline: none;
  max-height: 120px;
  overflow-y: auto;
}

.input-textarea:focus {
  border-color: #4ade80;
}

.input-textarea::placeholder {
  color: #c0b8a8;
}

.send-btn {
  padding: 8px 16px;
  background: #4ade80;
  color: #1a2e1a;
  border: none;
  border-radius: 6px;
  cursor: pointer;
  font-weight: bold;
  font-size: 14px;
  align-self: flex-end;
}

.send-btn:hover:not(:disabled) {
  background: #6ee7a0;
}

.send-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

/* 狀態列 */
.status-bar {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 4px 12px 8px 12px;
  background: #f0ebe3;
  border-top: 1px solid #e0d8cc;
  font-size: 12px;
  color: #8a8078;
}

.bot-debug-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  padding: 0 12px 8px 12px;
  background: #f0ebe3;
  border-top: 1px dashed #ded3c4;
  font-size: 11px;
  color: #8a8078;
}

.bot-debug-item {
  max-width: 100%;
  padding: 2px 8px;
  background: #fbf8f3;
  border: 1px solid #e6ddd1;
  border-radius: 999px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.status-tag {
  padding: 1px 8px;
  background: #ffffff;
  border-radius: 3px;
  color: #8a8078;
}

.online-tag {
  color: #4aad60;
}

.offline-tag {
  color: #e05050;
}

.discord-bot-btn {
  padding: 1px 10px;
  background: #ffffff;
  color: #8a8078;
  border: 1px solid #e0d8cc;
  border-radius: 3px;
  cursor: pointer;
  font-size: 12px;
  transition: all 0.15s;
}

.discord-bot-btn:hover:not(:disabled) {
  background: #f5efe6;
}

.discord-bot-btn.active {
  background: #edfcf2;
  color: #2e9e50;
  border-color: #4ade80;
}

.discord-bot-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
</style>
