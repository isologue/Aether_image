import client from '../client'

export interface ImageStorageConfig {
  resize_enabled: boolean
  return_format: 'base64' | 'url'
  honor_response_format: boolean
  public_base_url: string
  storage_mode: 'local' | 'webdav' | 'local_webdav'
  webdav_url: string
  webdav_username: string
  webdav_root: string
  webdav_public_url: string
  cleanup_enabled: boolean
  retention_hours: number
  cleanup_interval_hours: number
}
export const defaultImageStorageConfig = (): ImageStorageConfig => ({
  resize_enabled: false,
  return_format: 'base64', honor_response_format: false, public_base_url: '',
  storage_mode: 'local', webdav_url: '', webdav_username: '', webdav_root: 'aether/images',
  webdav_public_url: '', cleanup_enabled: false, retention_hours: 168, cleanup_interval_hours: 6,
})
export function normalizeImageStorageConfig(value: Partial<ImageStorageConfig> & { webdav_enabled?: boolean } = {}): ImageStorageConfig {
  const { webdav_enabled: legacyEnabled, ...config } = value
  return { ...defaultImageStorageConfig(), ...config, storage_mode: config.storage_mode ?? (legacyEnabled ? 'local_webdav' : 'local') }
}
export interface ImageCleanupRun {
  started_at: number; deleted: number; freed_bytes: number; skipped_pending: number; errors: number
}
export interface ImageStorageStats {
  directory: string; count: number; bytes: number; synced: number; pending: number; failed: number
  expired_pending: number; webdav_enabled: boolean; updated_at: number
  failure_details: { id: string; error: string; attempts: number }[]
  disk: { total: number; available: number; used_percent: number } | null
  last_cleanup: ImageCleanupRun | null; next_cleanup_at: number | null
  sync: { running: boolean; total: number; completed: number; failed: number; error: string | null }
}
export interface WebdavTestResult {
  success: boolean
  upload: { success: boolean; error: string | null }
  public_access: { success: boolean; url: string | null; error: string | null }
  delete: { success: boolean; error: string | null }
}
const base = '/api/admin/system/images'
export const imageStorageApi = {
  async stats(): Promise<ImageStorageStats> { return (await client.get<ImageStorageStats>(`${base}/stats`)).data },
  async test(): Promise<WebdavTestResult> { return (await client.post<WebdavTestResult>(`${base}/webdav-test`, {}, { timeout: 100000 })).data },
  async sync(): Promise<void> { await client.post(`${base}/sync`) },
  async cleanup(includeFailed: boolean): Promise<ImageCleanupRun> { return (await client.post<ImageCleanupRun>(`${base}/cleanup`, { include_failed: includeFailed }, { timeout: 100000 })).data },
}
export function validateImageStorageConfig(c: ImageStorageConfig): string | null {
  if (!['local', 'webdav', 'local_webdav'].includes(c.storage_mode)) return '请选择图片存储方式'
  const local = c.storage_mode !== 'webdav'
  const webdav = c.storage_mode !== 'local'
  if (!['base64', 'url'].includes(c.return_format)) return '请选择图片返回格式'
  for (const [name, value, enabled] of [['本地公开地址', c.public_base_url, local], ['WebDAV URL', c.webdav_url, webdav], ['WebDAV 公开前缀', c.webdav_public_url, webdav]] as const) {
    if (!enabled || !value) continue
    try {
      const url = new URL(value)
      if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) return `${name}必须为不带凭据和查询参数的 HTTP(S) 地址`
    } catch { return `${name}不是有效的地址` }
  }
  if (webdav && (!c.webdav_url || !c.webdav_public_url)) return '启用 WebDAV 时请填写 URL 和公开访问前缀'
  if (webdav && (c.webdav_root.length > 512 || /[\\?#%]/.test(c.webdav_root) || [...c.webdav_root].some(char => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127) || c.webdav_root.startsWith('/') || c.webdav_root.split('/').some(p => p === '.' || p === '..'))) return 'WebDAV 根路径必须为相对目录，不能包含 .. 或 URL 转义'
  if (!Number.isInteger(c.retention_hours) || c.retention_hours < 1 || c.retention_hours > 87600) return '保留时间应为 1–87600 的整数小时'
  if (!Number.isInteger(c.cleanup_interval_hours) || c.cleanup_interval_hours < 1 || c.cleanup_interval_hours > 8760) return '清理间隔应为 1–8760 的整数小时'
  return null
}
