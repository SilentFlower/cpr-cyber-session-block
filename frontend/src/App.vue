<script setup lang="ts">
import type { TableColumn } from '@codex-proxy/ui'
import type { BlockRecord, BlockStatus, HistoryQuery } from './api/modules/history'
import { ZButton, ZCard, ZConfigProvider, ZEmpty, ZInput, ZPagination, ZSelect, ZTable, ZTag } from '@codex-proxy/ui'
import { zhHans } from '@codex-proxy/ui/locale'
import { computed, shallowRef, watch } from 'vue'
import { configurationWriteState } from './api/request'
import RecordDetails from './components/RecordDetails.vue'
import SettingsDialog from './components/SettingsDialog.vue'
import { useBlockHistory } from './composables/useBlockHistory'
import { groupText } from './groups'

const { snapshot, loading, releasingId, error, releaseUncertain, disabled, load, release } = useBlockHistory()
const query = shallowRef<HistoryQuery>({
  search: '',
  status: 'all',
  page: 1,
  limit: 20,
})
const search = shallowRef('')
const expanded = shallowRef<string[]>([])
const settingsOpen = shallowRef(false)
const records = computed(() => snapshot.value?.records ?? [])
const reconnect = computed(() => ['saved', 'uncertain'].includes(configurationWriteState.value))
const controlsDisabled = computed(() => disabled.value || !!releasingId.value)
const page = computed({
  get: () => snapshot.value?.pagination.page ?? query.value.page,
  set: (value: number) => { query.value = { ...query.value, page: value, asOf: snapshot.value?.asOf } },
})
const pageSize = computed({
  get: () => query.value.limit,
  set: (value: number) => {
    query.value = {
      ...query.value,
      limit: value,
      page: 1,
      asOf: undefined,
    }
  },
})
const status = computed({
  get: () => query.value.status,
  set: (value: 'all' | BlockStatus) => {
    query.value = {
      ...query.value,
      status: value,
      page: 1,
      asOf: undefined,
    }
  },
})
const options = [
  { value: 'all', label: '全部状态' },
  { value: 'active', label: '封禁中' },
  { value: 'expired', label: '已到期' },
  { value: 'released', label: '已解除' },
]
const metrics = computed(() => [
  { label: '封禁中', value: snapshot.value?.summary.active ?? '—', color: 'text-z-warning' },
  { label: '保留记录', value: snapshot.value?.summary.retained ?? '—', color: 'text-z-text' },
  { label: '匹配记录', value: snapshot.value?.pagination.total ?? '—', color: 'text-z-primary' },
])
const labels: Record<BlockStatus, string> = { active: '封禁中', expired: '已到期', released: '已解除' }
const columns: TableColumn<BlockRecord>[] = [
  {
    key: 'createdTimeDisplay',
    label: '触发时间',
    kind: 'datetime',
    minWidth: 170,
  },
  {
    key: 'sessionHash',
    label: '会话 / Key',
    kind: 'identity',
    minWidth: 170,
  },
  {
    key: 'requestId',
    label: '触发请求 / 模型',
    kind: 'identity',
    minWidth: 190,
  },
  { key: 'routingGroups', label: '触发分组', minWidth: 150 },
  {
    key: 'accountId',
    label: '触发账号',
    kind: 'identity',
    minWidth: 170,
  },
  {
    key: 'status',
    label: '状态',
    kind: 'status',
    minWidth: 100,
  },
  {
    key: 'expiresTimeDisplay',
    label: '到期时间',
    kind: 'datetime',
    minWidth: 170,
  },
  {
    key: 'rejectedRequests',
    label: '本地拦截',
    kind: 'numeric',
    minWidth: 90,
  },
  {
    key: 'actions',
    label: '操作',
    kind: 'actions',
    width: 130,
  },
]

watch(query, value => void load(value), { immediate: true })

function submit() {
  query.value = {
    ...query.value,
    search: search.value.trim(),
    page: 1,
    asOf: undefined,
  }
}
function refresh() {
  query.value = { ...query.value, asOf: undefined }
}
function reset() {
  search.value = ''
  query.value = {
    search: '',
    status: 'all',
    page: 1,
    limit: query.value.limit,
  }
}
function details(id: string) {
  expanded.value = expanded.value.includes(id)
    ? []
    : [id]
}
</script>

<template>
  <ZConfigProvider :locale="zhHans">
    <main class="space-y-4 text-z-text" :class="{ 'overflow-clip': settingsOpen }" :style="settingsOpen ? { maxHeight: 'var(--plugin-viewport-height, 100dvh)' } : undefined">
      <SettingsDialog v-model="settingsOpen" />
      <p v-if="reconnect && !settingsOpen" role="status" class="rounded-z bg-z-info-container p-3 text-z-sm text-z-info-on-container">
        {{ configurationWriteState === 'saved' ? '设置已保存并生效' : '保存结果待确认' }}，点击右上角「刷新页面」继续操作
      </p>
      <section aria-label="封禁概览" class="grid grid-cols-3 gap-2 sm:gap-4">
        <ZCard v-for="metric in metrics" :key="metric.label">
          <p class="text-z-xs text-z-text-secondary sm:text-z-sm">
            {{ metric.label }}
          </p>
          <p class="mt-1 text-2xl font-emphasis tabular-nums" :class="metric.color">
            {{ metric.value }}
          </p>
        </ZCard>
      </section>
      <ZCard class="min-w-0">
        <div class="mb-3 flex flex-wrap items-baseline justify-between gap-2">
          <h2 class="text-z-base font-emphasis">
            会话封禁记录
          </h2>
          <span class="text-z-xs text-z-text-secondary">北京时间 · UTC+8</span>
        </div>
        <form class="mb-4 flex flex-wrap items-center gap-2" @submit.prevent="submit">
          <ZInput v-model="search" aria-label="请求或会话" placeholder="请求 ID、会话摘要或 Key ID" maxlength="256" :disabled="controlsDisabled" class="w-full sm:max-w-80" />
          <ZSelect v-model="status" aria-label="封禁状态" :options="options" :disabled="controlsDisabled" class="w-full sm:w-36" />
          <ZButton type="primary" native-type="submit" :disabled="controlsDisabled || loading">
            查询
          </ZButton>
          <ZButton :disabled="controlsDisabled || loading || (!search && query.status === 'all' && !query.search)" @click="reset">
            重置
          </ZButton>
          <ZButton :disabled="controlsDisabled" class="sm:ml-auto" @click="settingsOpen = true">
            插件设置
          </ZButton>
          <ZButton :loading="loading" :disabled="controlsDisabled" @click="refresh">
            刷新
          </ZButton>
        </form>
        <p v-if="snapshot?.summary.pending" role="status" class="mb-3 text-z-sm text-z-text-secondary">
          有失败记录正在核实
        </p>
        <div v-if="error" role="alert" class="mb-4 flex flex-wrap items-center gap-3 rounded-z bg-z-error-container p-3 text-z-error-on-container">
          <span>{{ error }}</span>
          <ZButton size="small" :disabled="disabled || loading" @click="refresh">
            重新读取
          </ZButton>
        </div>
        <ZTable :columns="columns" :data="records" row-key="id" :loading="loading" :expanded-row-keys="expanded" size="small">
          <template #createdTimeDisplay="{ row }">
            <span class="whitespace-nowrap text-z-sm tabular-nums">{{ row.createdTimeDisplay }}</span>
          </template>
          <template #expiresTimeDisplay="{ row }">
            <span class="whitespace-nowrap text-z-sm tabular-nums">{{ row.expiresTimeDisplay }}</span>
          </template>
          <template #sessionHash="{ row }">
            <div class="space-y-1">
              <div class="truncate font-mono text-z-xs" :title="row.sessionHash">
                {{ row.sessionHash.slice(0, 16) }}
              </div><div class="truncate text-z-xs text-z-text-secondary" :title="row.clientKeyId">
                {{ row.clientKeyId }}
              </div>
            </div>
          </template>
          <template #requestId="{ row }">
            <div class="space-y-1">
              <div class="truncate font-mono text-z-xs" :title="row.requestId">
                {{ row.requestId }}
              </div><div class="truncate text-z-xs text-z-text-secondary">
                {{ row.model ?? '未知模型' }}
              </div>
            </div>
          </template>
          <template #routingGroups="{ row }">
            <span class="block truncate text-z-xs" :title="groupText(row)">{{ groupText(row) }}</span>
          </template>
          <template #accountId="{ row }">
            <div class="min-w-0 space-y-1" :title="row.accountId ?? undefined">
              <span class="block truncate text-z-sm font-emphasis">{{ row.accountName ?? row.accountId ?? '未记录' }}</span>
              <span v-if="row.accountEmail && row.accountEmail !== row.accountName" class="block truncate text-z-xs text-z-text-secondary" :title="row.accountEmail">{{ row.accountEmail }}</span>
            </div>
          </template>
          <template #status="{ row }">
            <ZTag :type="row.status === 'active' ? 'warning' : 'info'" size="small">
              {{ labels[row.status] }}
            </ZTag>
          </template>
          <template #actions="{ row }">
            <div class="flex items-center gap-2 whitespace-nowrap">
              <ZButton variant="link" size="small" :aria-expanded="expanded.includes(row.id)" :aria-controls="`record-${row.id}`" @click="details(row.id)">
                {{ expanded.includes(row.id) ? '收起' : '详情' }}
              </ZButton>
              <ZButton variant="link" size="small" :loading="releasingId === row.id" :disabled="disabled || loading || !!releasingId || releaseUncertain || row.status !== 'active'" @click="release(row, query)">
                解除
              </ZButton>
            </div>
          </template>
          <template #expanded="{ row }">
            <RecordDetails :record="row" />
          </template>
          <template #empty>
            <ZEmpty :title="error ? '暂时无法读取记录' : '暂无匹配的封禁记录'" :description="error ? '可重新读取已有记录' : '上游实际命中 cyber_policy 后记录'" surface="none" />
          </template>
        </ZTable>
        <ZPagination v-if="snapshot?.pagination.total" v-model:current-page="page" v-model:page-size="pageSize" :total="snapshot.pagination.total" :page-sizes="[10, 20, 50]" :disabled="controlsDisabled || loading" class="mt-4" />
      </ZCard>
    </main>
  </ZConfigProvider>
</template>
