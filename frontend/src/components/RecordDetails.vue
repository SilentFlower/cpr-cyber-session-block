<script setup lang="ts">
import type { BlockRecord } from '../api/modules/history'
import { groupText } from '../groups'

defineProps<{ record: BlockRecord }>()
</script>

<template>
  <dl :id="`record-${record.id}`" class="grid gap-x-6 gap-y-4 rounded-z bg-z-fill-alter p-4 text-z-sm sm:grid-cols-2">
    <div>
      <dt class="text-z-text-secondary">
        会话摘要
      </dt><dd class="mt-1 break-all font-mono text-z-xs">
        {{ record.sessionHash }}
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        API Key ID
      </dt><dd class="mt-1 break-all font-mono text-z-xs">
        {{ record.clientKeyId }}
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        触发分组
      </dt>
      <dd class="mt-1 break-words">
        {{ groupText(record) }}
      </dd>
    </div>
    <div v-if="record.routingGroupRefs.length">
      <dt class="text-z-text-secondary">
        触发分组 ID
      </dt>
      <dd class="mt-1 break-all font-mono text-z-xs">
        {{ record.routingGroupRefs.join('、') || '未记录' }}
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        触发请求
      </dt><dd class="mt-1 break-all font-mono text-z-xs">
        {{ record.requestId }}
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        触发账号
      </dt><dd class="mt-1 break-all text-z-sm">
        {{ record.accountName ?? '名称不可用' }}
        <span v-if="record.accountEmail && record.accountEmail !== record.accountName" class="mt-1 block text-z-xs text-z-text-secondary">{{ record.accountEmail }}</span>
        <span class="mt-1 block font-mono text-z-xs text-z-text-secondary">{{ record.accountId ?? '未记录账号 ID' }}</span>
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        触发原因
      </dt><dd class="mt-1 font-mono text-z-xs">
        cyber_policy
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        传输 / 尝试
      </dt><dd class="mt-1">
        {{ record.transport }} / {{ record.attemptIndex ?? '未知' }}
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        触发时间（北京时间）
      </dt><dd class="mt-1">
        {{ record.createdTimeDisplay }}
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        到期时间（北京时间）
      </dt><dd class="mt-1">
        {{ record.expiresTimeDisplay }}
      </dd>
    </div>
    <div v-if="record.releasedTimeDisplay">
      <dt class="text-z-text-secondary">
        解除时间（北京时间）
      </dt><dd class="mt-1">
        {{ record.releasedTimeDisplay }}
      </dd>
    </div>
  </dl>
</template>
