import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import ImageStorageSection from './ImageStorageSection.vue'
import { defaultImageStorageConfig, normalizeImageStorageConfig, validateImageStorageConfig } from '@/api/endpoints/images'
const api = vi.hoisted(() => ({ get: vi.fn(), update: vi.fn(), stats: vi.fn(), test: vi.fn(), sync: vi.fn(), cleanup: vi.fn(), success: vi.fn(), error: vi.fn() }))
vi.mock('@/api/admin', () => ({ adminApi: { getSystemConfig: api.get, updateSystemConfig: api.update } }))
vi.mock('@/api/endpoints/images', async importOriginal => ({ ...await importOriginal<typeof import('@/api/endpoints/images')>(), imageStorageApi: { stats: api.stats, test: api.test, sync: api.sync, cleanup: api.cleanup } }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ success: api.success, error: api.error }) }))
let app: App | undefined
let host: HTMLDivElement
async function flush() { for (let i = 0; i < 12; i++) { await Promise.resolve(); await nextTick() } }
async function mount() { host = document.createElement('div'); document.body.append(host); app = createApp(ImageStorageSection); app.mount(host); await flush() }
function button(text: string) { return [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === text) as HTMLButtonElement }
beforeEach(() => {
  vi.clearAllMocks()
  let persistedConfig = defaultImageStorageConfig()
  api.get.mockImplementation(async (key: string) => key === 'image_storage' ? { value: { ...persistedConfig } } : { value: null, is_set: true })
  api.update.mockImplementation(async (key: string, value: unknown) => {
    if (key === 'image_storage') persistedConfig = { ...persistedConfig, ...(value as typeof persistedConfig) }
    if (key === 'image_task_policy') {
      persistedConfig = { ...persistedConfig, ...((value as { image_storage: typeof persistedConfig }).image_storage) }
    }
    return {}
  })
  api.stats.mockResolvedValue({ directory: '/data/images', count: 2, bytes: 2048, disk: null, updated_at: 1, last_cleanup: null, next_cleanup_at: null, sync: { running: false, total: 0 }, failure_details: [] })
})
afterEach(() => { app?.unmount(); app = undefined; host?.remove() })
describe('图片存储设置', () => {
  it('defaults resizing off and persists the resize switch with existing preferences', async () => {
    await mount()
    const toggle = host.querySelector('#image-resize-enabled') as HTMLInputElement
    expect(toggle.checked).toBe(false)
    expect(host.textContent).toContain('Sharp / Lanczos3')
    toggle.checked = true; toggle.dispatchEvent(new Event('change', { bubbles: true })); await flush()
    button('保存').click(); await flush()
    expect(api.update).toHaveBeenCalledWith('image_storage', expect.objectContaining({ resize_enabled: true, return_format: 'base64', storage_mode: 'local' }))
    expect(api.get).toHaveBeenCalledWith('image_storage')
    expect((host.querySelector('#image-resize-enabled') as HTMLInputElement).checked).toBe(true)

    app?.unmount(); app = undefined; host.remove()
    await mount()
    expect((host.querySelector('#image-resize-enabled') as HTMLInputElement).checked).toBe(true)
  })
  it('saves automatic cleanup and task retention atomically and keeps it enabled after reload', async () => {
    await mount()
    const cleanup = host.querySelector('#image-cleanup-enabled') as HTMLInputElement
    expect(cleanup).not.toBeNull()
    expect(cleanup.checked).toBe(false)
    cleanup.checked = true
    cleanup.dispatchEvent(new Event('change', { bubbles: true }))
    await flush()

    button('保存').click()
    await flush()

    expect(api.update).toHaveBeenCalledWith('image_task_policy', {
      image_storage: expect.objectContaining({ cleanup_enabled: true }),
      async_task_retention_days: 30,
    })
    expect((host.querySelector('#image-cleanup-enabled') as HTMLInputElement).checked).toBe(true)

    app?.unmount(); app = undefined; host.remove()
    await mount()
    expect((host.querySelector('#image-cleanup-enabled') as HTMLInputElement).checked).toBe(true)
  })
  it('defaults to ignoring response_format and preserves an existing password when saving preferences', async () => {
    await mount()
    const toggle = host.querySelectorAll('input[type=checkbox]')[0] as HTMLInputElement
    expect(toggle.checked).toBe(false)
    expect(host.querySelector('#image-dav-password')).toBeNull()
    expect((host.querySelector('#image-storage-mode') as HTMLSelectElement).value).toBe('local')
    toggle.checked = true; toggle.dispatchEvent(new Event('change', { bubbles: true })); await flush()
    button('保存').click(); await flush()
    expect(api.update).toHaveBeenCalledTimes(1)
    expect(api.update).toHaveBeenCalledWith('image_storage', expect.objectContaining({ honor_response_format: true }))
  })
  it('prevents actions with unsaved settings and tests only saved WebDAV settings', async () => {
    api.get.mockImplementation(async (key: string) => key === 'image_storage' ? { value: { ...defaultImageStorageConfig(), storage_mode: 'local_webdav', webdav_url: 'https://dav.example.com', webdav_public_url: 'https://img.example.com/images' } } : { value: null, is_set: true })
    await mount()
    const password = host.querySelector('#image-dav-password') as HTMLInputElement
    expect(password.value).toBe(''); expect(password.placeholder).toContain('已配置')
    const input = host.querySelector('#image-public-url') as HTMLInputElement
    input.value = 'https://img.example.com'; input.dispatchEvent(new Event('input', { bubbles: true })); await flush()
    expect(button('测试 WebDAV').disabled).toBe(true)
    button('保存').click(); await flush(); expect(button('测试 WebDAV').disabled).toBe(false)
    api.test.mockResolvedValue({ success: false, upload: { success: true, error: null }, public_access: { success: false, error: '公开映射错误' }, delete: { success: true, error: null } })
    button('测试 WebDAV').click(); await flush(); expect(host.textContent).toContain('公开映射错误'); expect(api.test).toHaveBeenCalledTimes(1)
  })
  it('does not expose a writable default form when loading fails', async () => {
    api.get.mockRejectedValue(new Error('配置加载失败')); await mount()
    expect(host.textContent).toContain('配置加载失败'); expect(button('保存').disabled).toBe(true); expect(host.querySelector('#image-return-format')).toBeNull()
  })
  it('validates positive integer cleanup values and a public WebDAV mapping', () => {
    const config = defaultImageStorageConfig(); expect(validateImageStorageConfig(config)).toBeNull()
    config.retention_hours = 0; expect(validateImageStorageConfig(config)).toContain('保留时间')
    config.retention_hours = 168; config.cleanup_interval_hours = 1.5; expect(validateImageStorageConfig(config)).toContain('清理间隔')
    config.cleanup_interval_hours = 6; config.storage_mode = 'webdav'; expect(validateImageStorageConfig(config)).toContain('公开访问前缀')
    config.webdav_url = 'https://dav.example.com/dav'; config.webdav_public_url = 'https://img.example.com/images'; config.webdav_root = '../images'; expect(validateImageStorageConfig(config)).toContain('相对目录')
  })
  it('shows WebDAV fields only for modes that use it and saves the selected mode', async () => {
    await mount()
    expect(host.querySelector('#image-dav-url')).toBeNull()
    const mode = host.querySelector('#image-storage-mode') as HTMLSelectElement
    mode.value = 'webdav'; mode.dispatchEvent(new Event('change', { bubbles: true })); await flush()
    expect(host.querySelector('#image-dav-url')).not.toBeNull()
    expect(host.querySelector('#image-public-url')).toBeNull()
    expect(host.textContent).toContain('不保存本地副本')
    expect(host.textContent).toContain('上传成功后返回 WebDAV 地址，不再回读校验公开图片')
    expect(host.textContent).not.toContain('等待上传和公开访问校验成功')
    for (const [id, value] of [['image-dav-url', 'https://dav.example.com'], ['image-dav-public', 'https://img.example.com/images']]) {
      const input = host.querySelector(`#${id}`) as HTMLInputElement
      input.value = value; input.dispatchEvent(new Event('input', { bubbles: true }))
    }
    await flush(); button('保存').click(); await flush()
    expect(api.update).toHaveBeenCalledWith('image_storage', expect.objectContaining({ storage_mode: 'webdav' }))
    mode.value = 'local_webdav'; mode.dispatchEvent(new Event('change', { bubbles: true })); await flush()
    expect(host.querySelector('#image-public-url')).not.toBeNull()
    expect(host.querySelector('#image-dav-url')).not.toBeNull()
    mode.value = 'local'; mode.dispatchEvent(new Event('change', { bubbles: true })); await flush()
    expect(host.querySelector('#image-dav-url')).toBeNull()
    mode.value = 'local_webdav'; mode.dispatchEvent(new Event('change', { bubbles: true })); await flush()
    expect((host.querySelector('#image-dav-url') as HTMLInputElement).value).toBe('https://dav.example.com')
  })
  it('migrates the legacy WebDAV toggle without saving it again', async () => {
    api.get.mockImplementation(async (key: string) => key === 'image_storage' ? { value: { webdav_enabled: true, webdav_url: 'https://dav.example.com', webdav_public_url: 'https://img.example.com/images' } } : { value: null, is_set: true })
    await mount()
    expect((host.querySelector('#image-storage-mode') as HTMLSelectElement).value).toBe('local_webdav')
    expect(host.querySelector('#image-dav-url')).not.toBeNull()
    const mode = host.querySelector('#image-storage-mode') as HTMLSelectElement
    mode.value = 'webdav'; mode.dispatchEvent(new Event('change', { bubbles: true })); await flush()
    button('保存').click(); await flush()
    const saved = api.update.mock.calls[0][1]
    expect(saved.storage_mode).toBe('webdav')
    expect(saved).not.toHaveProperty('webdav_enabled')
    expect(normalizeImageStorageConfig({ webdav_enabled: false }).storage_mode).toBe('local')
    expect(normalizeImageStorageConfig({ storage_mode: 'webdav', webdav_enabled: true }).storage_mode).toBe('webdav')
  })

})
