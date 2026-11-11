import { isObject, request } from '../request'

/** 后端已经计算的封禁状态 */
export type BlockStatus = 'active' | 'expired' | 'released'

/** 一次命中对应的持久记录，时间展示文本由后端生成 */
export interface BlockRecord {
  id: string
  sessionHash: string
  clientKeyId: string
  requestId: string
  accountId: string | null
  accountName: string | null
  model: string | null
  attemptIndex: number | null
  transport: string
  createdAtMs: number
  expiresAtMs: number
  releasedAtMs: number | null
  rejectedRequests: number
  lastRejectedAtMs: number | null
  status: BlockStatus
  createdTimeDisplay: string
  expiresTimeDisplay: string
  releasedTimeDisplay: string | null
}

/** 记录页与统计来自同一次后端查询 */
export interface HistoryResponse {
  records: BlockRecord[]
  pagination: { page: number, pageSize: number, total: number }
  asOf: number
  serverTimeMs: number
  summary: { active: number, retained: number, pending: boolean }
}

/** 记录查询参数 */
export interface HistoryQuery {
  search: string
  status: 'all' | BlockStatus
  page: number
  limit: number
  asOf?: number
}

/**
 * 读取分页记录并验证业务合同
 * @param input 当前筛选与分页
 * @return 可用于显示的封禁记录快照
 */
export async function getHistory(input: HistoryQuery): Promise<HistoryResponse> {
  const query = new URLSearchParams({
    search: input.search,
    status: input.status,
    page: String(input.page),
    limit: String(input.limit),
  })
  if (input.asOf !== undefined)
    query.set('asOf', String(input.asOf))
  const value = await request('history', query)
  if (!isObject(value) || !Array.isArray(value.records) || !value.records.every(record)
    || !isObject(value.pagination) || !['page', 'pageSize', 'total'].every(key => Number.isSafeInteger(value.pagination && (value.pagination as Record<string, unknown>)[key]))
    || !Number.isSafeInteger(value.asOf) || !Number.isSafeInteger(value.serverTimeMs)
    || !isObject(value.summary) || !Number.isSafeInteger(value.summary.active) || !Number.isSafeInteger(value.summary.retained) || typeof value.summary.pending !== 'boolean') {
    throw new Error('封禁记录响应无效')
  }
  return value as unknown as HistoryResponse
}

/**
 * 解除指定记录，结果未知时由用户刷新核对，不重放写入
 * @param record 当前页面读取的精确封禁记录
 * @return 确认该记录已经解除
 */
export async function releaseBlock(record: BlockRecord): Promise<void> {
  const value = await request('release', new URLSearchParams(), { sessionHash: record.sessionHash, recordId: record.id })
  if (!isObject(value) || value.released !== true || value.recordId !== record.id)
    throw new Error('解除结果待确认，请刷新记录核对')
}

function record(value: unknown): value is BlockRecord {
  return isObject(value)
    && [
      'id',
      'sessionHash',
      'clientKeyId',
      'requestId',
      'transport',
      'createdTimeDisplay',
      'expiresTimeDisplay',
    ].every(key => typeof value[key] === 'string')
    && [
      'accountId',
      'accountName',
      'model',
      'releasedTimeDisplay',
    ].every(key => value[key] === null || typeof value[key] === 'string')
    && ['createdAtMs', 'expiresAtMs', 'rejectedRequests'].every(key => Number.isSafeInteger(value[key]))
    && ['attemptIndex', 'releasedAtMs', 'lastRejectedAtMs'].every(key => value[key] === null || Number.isSafeInteger(value[key]))
    && ['active', 'expired', 'released'].includes(String(value.status))
}
