<script setup lang="ts">
import { computed } from 'vue'
import type { ToolUseItem } from '../types/codex'

const props = defineProps<{
  tool: ToolUseItem
}>()

const displayName = computed(() => {
  const t = props.tool
  switch (t.type) {
    case 'commandExecution':
      return `shell: ${t.name}`
    case 'toolCall':
    case 'mcpToolCall':
      return t.name
    case 'fileChange':
      return `file: ${t.name}`
    default:
      return t.name || t.type
  }
})

const statusIcon = computed(() => {
  if (props.tool.isRunning) return '⏳'
  if (props.tool.result?.includes('error') || props.tool.result?.includes('Error')) return '❌'
  return '✅'
})
</script>

<template>
  <div :class="['tool-card', { running: tool.isRunning }]">
    <div class="tool-header">
      <span class="tool-icon">{{ statusIcon }}</span>
      <span class="tool-name">{{ displayName }}</span>
      <span v-if="tool.isRunning" class="tool-spinner" />
    </div>
    <div v-if="tool.result" class="tool-result">
      <pre class="tool-output">{{ tool.result.slice(0, 2000) }}{{ tool.result.length > 2000 ? '...' : '' }}</pre>
    </div>
  </div>
</template>

<style scoped>
.tool-card {
  margin: 4px 0;
  padding: 8px 12px;
  background: #f8f4ee;
  border: 1px solid #e0d8cc;
  border-radius: 6px;
  font-size: 13px;
}

.tool-card.running {
  border-color: #4ade80;
}

.tool-header {
  display: flex;
  align-items: center;
  gap: 6px;
}

.tool-icon {
  font-size: 14px;
}

.tool-name {
  color: #c07020;
  font-family: 'Cascadia Code', 'Fira Code', monospace;
  font-size: 12px;
}

.tool-spinner {
  width: 12px;
  height: 12px;
  border: 2px solid #e0d8cc;
  border-top-color: #4ade80;
  border-radius: 50%;
  animation: spin 0.8s linear infinite;
  margin-left: auto;
}

@keyframes spin {
  to { transform: rotate(360deg); }
}

.tool-result {
  margin-top: 6px;
  padding-top: 6px;
  border-top: 1px solid #e0d8cc;
}

.tool-output {
  margin: 0;
  padding: 0;
  font-family: 'Cascadia Code', 'Fira Code', monospace;
  font-size: 11px;
  color: #6a6058;
  white-space: pre-wrap;
  word-break: break-all;
  max-height: 200px;
  overflow-y: auto;
}
</style>
