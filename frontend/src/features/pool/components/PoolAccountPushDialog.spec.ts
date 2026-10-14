import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PoolAccountPushDialog from './PoolAccountPushDialog.vue'
import type { PoolOverviewItem } from '@/api/endpoints/pool'
const api = vi.hoisted(() => ({ get: vi.fn(), update: vi.fn(), success: vi.fn(), error: vi.fn() }))
vi.mock('@/api/admin', () => ({ adminApi: { getSystemConfig: api.get, updateSystemConfig: api.update } }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ success: api.success, error: api.error }) }))
let app: App | undefined
let host: HTMLDivElement
const providers = [
  { provider_id: 'web', provider_name: 'Web pool', provider_type: 'chatgpt_web' },
  { provider_id: 'codex', provider_name: 'Codex pool', provider_type: 'codex' },
  { provider_id: 'other', provider_name: 'Other provider', provider_type: 'custom' },
] as PoolOverviewItem[]
async function flush() { for (let i = 0; i < 16; i++) { await Promise.resolve(); await nextTick() } }
async function mount() {
  host = document.createElement('div'); document.body.append(host)
  app = createApp(PoolAccountPushDialog, { modelValue: true, providers, providerId: 'web' })
  app.mount(host); await flush()
}
function button(text: string) { return [...document.body.querySelectorAll('button')].find(item => item.textContent?.trim() === text)! }
async function setSecret(value: string) {
  const input = document.querySelector('#account-push-secret') as HTMLInputElement
  input.value = value; input.dispatchEvent(new Event('input', { bubbles: true })); await flush()
}
beforeEach(() => {
  vi.clearAllMocks()
  api.get.mockImplementation(async (key: string) => key === 'account_push' ? { value: { enabled: false, provider_id: '' } } : { value: null, is_set: false })
  api.update.mockResolvedValue({})
})
afterEach(async () => { app?.unmount(); app = undefined; host?.remove(); await flush() })
describe('PoolAccountPushDialog', () => {
  it('filters providers and preserves an existing secret', async () => {
    api.get.mockImplementation(async (key: string) => key === 'account_push' ? { value: { enabled: true, provider_id: 'codex' } } : { value: null, is_set: true })
    await mount()
    const select = document.querySelector('#account-push-provider') as HTMLSelectElement
    expect(select.value).toBe('codex')
    expect([...select.options].map(item => item.value)).toEqual(['', 'web', 'codex'])
    const secret = document.querySelector('#account-push-secret') as HTMLInputElement
    expect(secret.value).toBe(''); expect(secret.placeholder).toContain('已配置')
    button('保存').click(); await flush()
    expect(api.update.mock.calls).toEqual([['account_push', { enabled: true, provider_id: 'codex' }]])
  })
  it('saves a new secret before enabling the receiver', async () => {
    await mount()
    expect((document.querySelector('#account-push-provider') as HTMLSelectElement).value).toBe('web')
    await setSecret('aether_push_test_12345678901234567890')
    ;(document.querySelector('#account-push-enabled') as HTMLButtonElement).click(); await flush()
    button('保存').click(); await flush()
    expect(api.update.mock.calls).toEqual([
      ['account_push_api_key', 'aether_push_test_12345678901234567890'],
      ['account_push', { enabled: true, provider_id: 'web' }],
    ])
    expect((document.querySelector('#account-push-secret') as HTMLInputElement).value).toBe('')
  })
  it('requires a secret and stops after a failed secret save', async () => {
    await mount()
    ;(document.querySelector('#account-push-enabled') as HTMLButtonElement).click(); await flush()
    button('保存').click(); await flush()
    expect(api.update).not.toHaveBeenCalled(); expect(api.error).toHaveBeenCalledWith('请先生成或填写推送密钥')
    await setSecret('aether_push_test_12345678901234567890')
    api.update.mockRejectedValueOnce(new Error('保存密钥失败'))
    button('保存').click(); await flush()
    expect(api.update).toHaveBeenCalledTimes(1); expect(api.update.mock.calls[0][0]).toBe('account_push_api_key')
    expect(api.success).not.toHaveBeenCalled()
  })
  it('disables saving when loading fails', async () => {
    api.get.mockRejectedValue(new Error('配置加载失败')); await mount()
    expect(document.body.textContent).toContain('配置加载失败'); expect(button('保存').disabled).toBe(true)
    expect(document.querySelector('#account-push-secret')).toBeNull()
  })
  it('generates a strong secret and refreshes the last result', async () => {
    await mount()
    button('生成').click(); await flush()
    expect((document.querySelector('#account-push-secret') as HTMLInputElement).value).toMatch(/^aether_push_[0-9a-f]{64}$/)
    api.get.mockResolvedValueOnce({ value: { received_at: 1791900000, http_status: 200, success: true, added: 2, updated: 1, failed: 0 } })
    button('刷新').click(); await flush()
    expect(document.body.textContent).toContain('新增 2，更新 1，失败 0')
    expect(document.body.textContent).not.toContain('/api/accounts/upload')
    expect(document.body.textContent).not.toContain('ChatGPT2API')
    expect(document.body.textContent).toContain('account_failed')
    expect(document.body.textContent).toContain('/api/v1/admin/accounts/data')
    expect(document.body.textContent).toContain('X-API-Key')
  })
})
