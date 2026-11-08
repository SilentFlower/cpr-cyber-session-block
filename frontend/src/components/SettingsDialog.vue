<script setup lang="ts">
import type { Settings } from '../api/modules/settings'
import { ZButton, ZDialog, ZFormItem, ZInputNumber } from '@codex-proxy/ui'
import { computed, onBeforeUnmount, shallowRef, watch } from 'vue'
import { getSettings, saveSettings } from '../api/modules/settings'
import { configurationWriteState } from '../api/request'

const open = defineModel<boolean>({ required: true })
const source = shallowRef<Settings>()
const seconds = shallowRef(3600)
const loading = shallowRef(false)
const error = shallowRef('')
const saving = computed(() => configurationWriteState.value === 'saving')
const finished = computed(() => ['saved', 'uncertain'].includes(configurationWriteState.value))
const dirty = computed(() => source.value && seconds.value !== source.value.configuration.blockSeconds)
const valid = computed(() => typeof seconds.value === 'number' && Number.isInteger(seconds.value) && seconds.value >= 1 && seconds.value <= 2592000)
let alive = true
let generation = 0
onBeforeUnmount(() => {
  alive = false
  generation++
})
watch(open, (value) => {
  if (value && configurationWriteState.value === 'ready')
    void load()
})

async function load() {
  const current = ++generation
  loading.value = true
  error.value = ''
  try {
    const value = await getSettings()
    if (alive && current === generation) {
      source.value = value
      seconds.value = value.configuration.blockSeconds
    }
  }
  catch (cause) {
    if (alive && current === generation) {
      error.value = cause instanceof Error
        ? cause.message
        : '插件设置读取失败'
    }
  }
  finally {
    if (alive && current === generation)
      loading.value = false
  }
}

async function save() {
  if (!valid.value || !dirty.value || !source.value || seconds.value === undefined || saving.value)
    return
  error.value = ''
  try {
    await saveSettings(source.value.revision, seconds.value)
  }
  catch (cause) {
    if (alive) {
      error.value = cause instanceof Error
        ? cause.message
        : '插件设置保存失败'
    }
  }
}
</script>

<template>
  <ZDialog v-model="open" title="插件设置" width="34rem" :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving">
    <form id="cyber-session-settings" class="space-y-4" @submit.prevent="save">
      <p v-if="loading" role="status" class="text-z-text-secondary">
        正在读取设置
      </p>
      <div v-if="error" role="alert" class="rounded-z bg-z-error-container p-3 text-z-error-on-container">
        {{ error }}
        <ZButton v-if="!finished" variant="link" size="small" :disabled="loading || saving" @click="load">
          重新读取
        </ZButton>
      </div>
      <p v-if="finished" role="status" class="rounded-z bg-z-info-container p-3 text-z-info-on-container">
        {{ configurationWriteState === 'saved' ? '设置已保存并生效' : '保存结果待确认' }}，点击右上角「刷新页面」继续操作
      </p>
      <template v-if="source && !finished">
        <ZFormItem label="封禁时长（秒）" description="命中 cyber_policy 后限制同一会话，期间重试不延长封禁">
          <ZInputNumber v-model="seconds" aria-label="封禁时长（秒）" :min="1" :max="2592000" :precision="0" :disabled="loading || saving" class="w-full" />
        </ZFormItem>
        <p class="text-z-sm text-z-text-secondary">
          仅影响后续新命中，已有封禁按原到期时间结束
        </p>
      </template>
    </form>
    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ finished ? '关闭' : '取消' }}
      </ZButton>
      <ZButton v-if="!finished" type="primary" native-type="submit" form="cyber-session-settings" :loading="saving" :disabled="loading || !valid || !dirty">
        保存设置
      </ZButton>
    </template>
  </ZDialog>
</template>
