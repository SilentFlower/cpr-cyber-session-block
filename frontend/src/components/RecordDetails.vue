<script setup lang="ts">
import type { BlockRecord } from '../api/modules/history'

defineProps<{ record: BlockRecord }>()
</script>

<template>
  <dl :id="`record-${record.id}`" class="grid gap-x-6 gap-y-4 p-3 text-z-sm sm:grid-cols-2">
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
        触发请求
      </dt><dd class="mt-1 break-all font-mono text-z-xs">
        {{ record.requestId }}
      </dd>
    </div>
    <div>
      <dt class="text-z-text-secondary">
        账号 ID
      </dt><dd class="mt-1 break-all font-mono text-z-xs">
        {{ record.accountId ?? '未知' }}
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
        到期时间（UTC）
      </dt><dd class="mt-1">
        {{ record.expiresTimeUtc }}
      </dd>
    </div>
    <div v-if="record.releasedTimeUtc">
      <dt class="text-z-text-secondary">
        解除时间（UTC）
      </dt><dd class="mt-1">
        {{ record.releasedTimeUtc }}
      </dd>
    </div>
  </dl>
</template>
