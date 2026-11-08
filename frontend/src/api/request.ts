// @env browser
import { readonly, shallowRef } from 'vue'

/** 当前隔离页面由宿主提供的固定目标管理桥 */
interface PluginBridge {
  readonly version: 2
  request: (input: { method: 'GET' | 'POST', path: string, query: string, contentType?: string, body?: string }) => Promise<{ status: number, contentType: string, body: ArrayBuffer }>
}

declare global {
  /** 插件页面不能借用管理 Cookie 或访问其他实例 */
  interface Window { readonly codexProxyPlugin?: PluginBridge }
}

const writeState = shallowRef<'ready' | 'saving' | 'saved' | 'uncertain'>('ready')
const active = new Set<Promise<unknown>>()

/** 原生参数更新会使旧页面目标失效，所有操作共享这一份状态 */
export const configurationWriteState = readonly(writeState)

/** 保留明确的 HTTP 拒绝，区分写入被拒绝和结果未知 */
class ResponseError extends Error {
  /**
   * 创建管理路由错误
   * @param status 插件或宿主返回的 HTTP 状态
   * @param message 安全中文提示
   */
  constructor(readonly status: number, message: string) { super(message) }
}

/**
 * 校验 JSON 对象，API 模块继续检查自己的数据合同
 * @param value 待检查的数据
 * @return 是否为普通 JSON 对象
 */
export function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/**
 * 访问当前插件的管理路由，不自动重试读写
 * @param path 已注册的相对路径
 * @param query 查询参数
 * @param body POST 正文，省略时使用 GET
 * @return 原始业务 JSON
 */
export async function request(path: string, query = new URLSearchParams(), body?: unknown): Promise<unknown> {
  if (writeState.value !== 'ready')
    throw new Error('当前页面需要重新连接，请点击右上角「刷新页面」')
  const operation = invoke(path, query, body)
  active.add(operation)
  try {
    return await operation
  }
  finally { active.delete(operation) }
}

/**
 * 排空在途操作后独占发布参数，成功或结果未知时停止旧页面请求
 * @param body 带原生 revision 的完整参数
 * @return 已确认的保存结果
 */
export async function saveConfiguration(body: unknown): Promise<void> {
  if (writeState.value !== 'ready')
    throw new Error('当前页面需要重新连接，请点击右上角「刷新页面」')
  writeState.value = 'saving'
  try {
    await Promise.allSettled([...active])
    const value = await invoke('settings', new URLSearchParams(), body)
    if (!isObject(value) || value.saved !== true)
      throw new Error('保存结果待确认，请刷新页面核对')
    writeState.value = 'saved'
  }
  catch (cause) {
    writeState.value = cause instanceof ResponseError && [400, 409].includes(cause.status)
      ? 'ready'
      : 'uncertain'
    throw cause
  }
}

async function invoke(path: string, query: URLSearchParams, body?: unknown): Promise<unknown> {
  const bridge = window.codexProxyPlugin
  if (!bridge || bridge.version !== 2)
    throw new Error('请从 CPR 的插件扩展入口打开此页')
  const response = await bridge.request({
    method: body === undefined
      ? 'GET'
      : 'POST',
    path,
    query: query.toString(),
    ...(body === undefined
      ? {}
      : {
          contentType: 'application/json',
          body: JSON.stringify(body),
        }),
  })
  if (response.contentType.split(';', 1)[0]?.trim().toLowerCase() !== 'application/json')
    throw new Error('插件返回了不支持的内容类型')
  let value: unknown
  try {
    value = JSON.parse(new TextDecoder().decode(response.body))
  }
  catch (cause) { throw new Error('插件返回了无效 JSON', { cause }) }
  if (response.status < 200 || response.status >= 300) {
    const message = isObject(value) && isObject(value.error) && typeof value.error.message === 'string'
      ? value.error.message
      : `插件请求失败（HTTP ${response.status}）`
    throw new ResponseError(response.status, message)
  }
  return value
}
