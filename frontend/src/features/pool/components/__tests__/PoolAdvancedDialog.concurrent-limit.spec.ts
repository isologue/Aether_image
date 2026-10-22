import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick, ref, type App } from 'vue'

import type { PoolAdvancedConfig } from '@/api/endpoints/types/provider'
import PoolAdvancedDialog from '../PoolAdvancedDialog.vue'

const mocks = vi.hoisted(() => ({ getProvider: vi.fn(), updateProvider: vi.fn(), error: vi.fn() }))
vi.mock('@/api/endpoints', () => ({ getProvider: mocks.getProvider, updateProvider: mocks.updateProvider }))
vi.mock('@/composables/useToast', () => ({ useToast: () => ({ success: vi.fn(), error: mocks.error }) }))

const mountedApps: Array<{ app: App, root: HTMLElement }> = []
async function settle() {
  for (let index = 0; index < 6; index += 1) {
    await Promise.resolve()
    await nextTick()
  }
}

function mountDialog(currentConfig: PoolAdvancedConfig | null = null) {
  const root = document.createElement('div')
  document.body.appendChild(root)
  const open = ref(false)
  const app = createApp(defineComponent({
    setup() {
      void nextTick(() => { open.value = true })
      return () => h(PoolAdvancedDialog, {
        modelValue: open.value,
        providerId: 'provider-1',
        currentConfig,
        'onUpdate:modelValue': (value: boolean) => { open.value = value },
      })
    },
  }))
  app.mount(root)
  mountedApps.push({ app, root })
  return open
}

function input() {
  const element = document.querySelector<HTMLInputElement>('#pool-default-concurrent-limit')
  expect(element).not.toBeNull()
  return element!
}

async function updateLimit(value: string) {
  input().value = value
  input().dispatchEvent(new Event('input', { bubbles: true }))
  await nextTick()
}

async function save() {
  const button = [...document.body.querySelectorAll<HTMLButtonElement>('button')]
    .find(element => element.textContent?.trim() === '保存')
  expect(button).toBeDefined()
  button!.click()
  await settle()
}

beforeEach(() => {
  mocks.getProvider.mockReset().mockResolvedValue({
    id: 'provider-1',
    pool_advanced: { default_concurrent_limit: 7, scheduling_presets: [{ preset: 'lru', enabled: true }] },
  })
  mocks.updateProvider.mockReset().mockResolvedValue({ id: 'provider-1' })
  mocks.error.mockReset()
})

afterEach(() => {
  for (const { app, root } of mountedApps.splice(0)) {
    app.unmount()
    root.remove()
  }
  document.body.innerHTML = ''
})

describe('PoolAdvancedDialog default account concurrency', () => {
  it('saves the configured default and preserves other current pool settings', async () => {
    mountDialog({ default_concurrent_limit: 3 })
    await settle()
    expect(input().value).toBe('3')
    expect(document.body.textContent).toContain('账号单独设置的值优先')
    await updateLimit('5')
    await save()
    expect(mocks.updateProvider).toHaveBeenCalledWith('provider-1', expect.objectContaining({
      pool_advanced: expect.objectContaining({
        default_concurrent_limit: 5,
        scheduling_presets: [{ preset: 'lru', enabled: true }],
      }),
    }))
  })

  it.each([['', null], ['0', 0]] as const)('saves %s as an unlimited default', async (value, expected) => {
    mountDialog({ default_concurrent_limit: 3 })
    await settle()
    await updateLimit(value)
    await save()
    expect(mocks.updateProvider).toHaveBeenCalledWith('provider-1', expect.objectContaining({
      pool_advanced: expect.objectContaining({ default_concurrent_limit: expected }),
    }))
  })

  it.each(['-1', '1.5', '2147483648'])('rejects invalid limit %s before sending', async (value) => {
    mountDialog()
    await settle()
    await updateLimit(value)
    await save()
    expect(mocks.error).toHaveBeenCalledWith('单账号并发上限必须是 0 到 2147483647 的整数')
    expect(mocks.updateProvider).not.toHaveBeenCalled()
  })

  it('resets unsaved edits when reopened', async () => {
    const open = mountDialog({ default_concurrent_limit: 3 })
    await settle()
    await updateLimit('2')
    open.value = false
    await settle()
    open.value = true
    await settle()
    expect(input().value).toBe('3')
  })
})
