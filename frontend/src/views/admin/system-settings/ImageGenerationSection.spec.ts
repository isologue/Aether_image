import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import ImageGenerationSection from './ImageGenerationSection.vue'

const api = vi.hoisted(() => ({ get: vi.fn(), update: vi.fn(), success: vi.fn(), error: vi.fn() }))
vi.mock('@/api/admin', () => ({ adminApi: { getSystemConfig: api.get, updateSystemConfig: api.update } }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ success: api.success, error: api.error }) }))

let app: App | undefined
let host: HTMLDivElement
async function flush() {
  for (let i = 0; i < 12; i++) { await Promise.resolve(); await nextTick() }
}
async function mount() {
  host = document.createElement('div')
  document.body.append(host)
  app = createApp(ImageGenerationSection)
  app.mount(host)
  await flush()
}
function saveButton() {
  return [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === '保存') as HTMLButtonElement
}
async function input(value: string) {
  const field = host.querySelector('input') as HTMLInputElement
  field.value = value
  field.dispatchEvent(new Event('input', { bubbles: true }))
  await flush()
}
beforeEach(() => {
  vi.clearAllMocks()
  api.get.mockResolvedValue({ value: null })
  api.update.mockResolvedValue({})
})
afterEach(() => { app?.unmount(); app = undefined; host?.remove() })

describe('图片生成设置', () => {
  it('loads the default and saves zero as a number for immediate failover', async () => {
    await mount()
    expect((host.querySelector('input') as HTMLInputElement).value).toBe('20')
    expect(saveButton().disabled).toBe(true)
    await input('0')
    saveButton().click()
    await flush()
    expect(api.update).toHaveBeenCalledWith('chatgpt_web_image_no_asset_grace_seconds', 0)
    expect(api.success).toHaveBeenCalled()
    expect(saveButton().disabled).toBe(true)
    expect(host.textContent).toContain('不限制整个生图请求耗时')
  })

  it('loads a saved value and blocks empty, fractional and out of range values', async () => {
    api.get.mockResolvedValue({ value: 8 })
    await mount()
    expect((host.querySelector('input') as HTMLInputElement).value).toBe('8')
    for (const value of ['', '-1', '121', '1.5']) {
      await input(value)
      expect(saveButton().disabled).toBe(true)
    }
    expect(api.update).not.toHaveBeenCalled()
    await input('120')
    expect(saveButton().disabled).toBe(false)
  })

  it('prevents overwriting settings on load failure and allows retry', async () => {
    api.get.mockRejectedValueOnce(new Error('offline'))
    await mount()
    expect(saveButton().disabled).toBe(true)
    expect(host.querySelector('input')).toBeNull()
    const retry = [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === '重试')!
    retry.click()
    await flush()
    expect(host.querySelector('input')).not.toBeNull()
    expect(api.update).not.toHaveBeenCalled()
  })

  it('keeps unsaved changes when saving fails', async () => {
    await mount()
    await input('5')
    api.update.mockRejectedValueOnce(new Error('offline'))
    saveButton().click()
    await flush()
    expect(api.error).toHaveBeenCalled()
    expect(saveButton().disabled).toBe(false)
    expect((host.querySelector('input') as HTMLInputElement).value).toBe('5')
  })
})
