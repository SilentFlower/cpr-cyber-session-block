import type { BlockRecord, HistoryQuery, HistoryResponse } from '../api/modules/history'
import { computed, onBeforeUnmount, shallowRef } from 'vue'
import { getHistory, releaseBlock } from '../api/modules/history'
import { configurationWriteState } from '../api/request'

/**
 * 管理查询、解除和迟到结果，组件只持有筛选输入及展开状态
 * @return 当前快照、操作状态与查询动作
 */
export function useBlockHistory() {
  const snapshot = shallowRef<HistoryResponse>()
  const loading = shallowRef(false)
  const releasingId = shallowRef('')
  const error = shallowRef('')
  const releaseUncertain = shallowRef(false)
  const disabled = computed(() => configurationWriteState.value !== 'ready')
  let generation = 0
  let alive = true
  onBeforeUnmount(() => {
    alive = false
    generation++
  })

  async function load(query: HistoryQuery) {
    if (disabled.value)
      return
    const current = ++generation
    loading.value = true
    error.value = ''
    try {
      const result = await getHistory(query)
      if (alive && current === generation) {
        snapshot.value = result
        releaseUncertain.value = false
      }
    }
    catch (cause) {
      if (alive && current === generation) {
        error.value = cause instanceof Error
          ? cause.message
          : '封禁记录读取失败'
      }
    }
    finally {
      if (alive && current === generation)
        loading.value = false
    }
  }

  async function release(record: BlockRecord, query: HistoryQuery) {
    if (disabled.value || releasingId.value || releaseUncertain.value)
      return
    releasingId.value = record.id
    error.value = ''
    try {
      await releaseBlock(record)
      if (alive)
        await load({ ...query, asOf: undefined })
    }
    catch (cause) {
      if (alive) {
        releaseUncertain.value = true
        error.value = cause instanceof Error
          ? `${cause.message}，请刷新记录核对解除结果`
          : '解除结果待确认，请刷新记录核对'
      }
    }
    finally {
      if (alive)
        releasingId.value = ''
    }
  }

  return {
    snapshot,
    loading,
    releasingId,
    error,
    releaseUncertain,
    disabled,
    load,
    release,
  }
}
