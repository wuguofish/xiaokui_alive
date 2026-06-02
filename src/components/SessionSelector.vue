<script setup lang="ts">
import { ref } from 'vue'
import type { SessionEntry } from '../types/codex'

defineProps<{
  sessions: SessionEntry[]
  loading: boolean
}>()

const emit = defineEmits<{
  select: [session: SessionEntry]
}>()

const isOpen = ref(false)

function toggle() {
  isOpen.value = !isOpen.value
}

function selectSession(session: SessionEntry) {
  emit('select', session)
  isOpen.value = false
}

function formatTime(iso: string) {
  if (!iso) return ''
  const d = new Date(iso)
  const now = new Date()
  const diff = now.getTime() - d.getTime()
  if (diff < 60000) return '剛剛'
  if (diff < 3600000) return `${Math.floor(diff / 60000)} 分鐘前`
  if (diff < 86400000) return `${Math.floor(diff / 3600000)} 小時前`
  return `${Math.floor(diff / 86400000)} 天前`
}
</script>

<template>
  <div class="session-selector">
    <button class="selector-toggle" @click="toggle">
      📋 歷史對話
      <span class="toggle-arrow">{{ isOpen ? '▲' : '▼' }}</span>
    </button>
    <div v-if="isOpen" class="session-dropdown">
      <div v-if="loading" class="session-empty">載入中...</div>
      <div v-else-if="sessions.length === 0" class="session-empty">沒有歷史對話</div>
      <div
        v-for="s in sessions"
        :key="s.sessionId"
        class="session-item"
        @click="selectSession(s)"
      >
        <div class="session-title">{{ s.title || '（無標題）' }}</div>
        <div class="session-time">{{ formatTime(s.modified) }}</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.session-selector {
  position: relative;
}

.selector-toggle {
  padding: 4px 12px;
  background: #232436;
  color: #888;
  border: 1px solid #33467c;
  border-radius: 4px;
  cursor: pointer;
  font-size: 12px;
  display: flex;
  align-items: center;
  gap: 6px;
}

.selector-toggle:hover {
  background: #2a2b44;
  color: #bbb;
}

.toggle-arrow {
  font-size: 10px;
}

.session-dropdown {
  position: absolute;
  top: 100%;
  left: 0;
  right: 0;
  min-width: 300px;
  max-height: 300px;
  overflow-y: auto;
  background: #1a1b2e;
  border: 1px solid #33467c;
  border-radius: 6px;
  margin-top: 4px;
  z-index: 100;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
}

.session-item {
  padding: 8px 12px;
  cursor: pointer;
  border-bottom: 1px solid #232436;
  transition: background 0.15s;
}

.session-item:last-child {
  border-bottom: none;
}

.session-item:hover {
  background: #232436;
}

.session-title {
  font-size: 13px;
  color: #e0e0e0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.session-time {
  font-size: 11px;
  color: #666;
  margin-top: 2px;
}

.session-empty {
  padding: 16px;
  text-align: center;
  color: #666;
  font-size: 13px;
}
</style>
