import { isObject, request, saveConfiguration } from '../request'

/** 当前实例的原生设置快照 */
export interface Settings { revision: number, configuration: { blockSeconds: number } }

/**
 * 读取封禁参数和精确版本
 * @return 当前有效配置
 */
export async function getSettings(): Promise<Settings> {
  const value = await request('settings')
  if (!isObject(value) || !Number.isSafeInteger(value.revision) || !isObject(value.configuration) || !Number.isSafeInteger(value.configuration.blockSeconds))
    throw new Error('插件设置响应无效')
  return value as unknown as Settings
}

/**
 * 保存完整参数，保留宿主原有绑定
 * @param revision 打开弹窗时读取的原生版本
 * @param blockSeconds 后续新命中使用的封禁时长
 * @return 宿主已确认的保存结果
 */
export async function saveSettings(revision: number, blockSeconds: number): Promise<void> {
  await saveConfiguration({
    expectedRevision: revision,
    configuration: { blockSeconds },
  })
}
