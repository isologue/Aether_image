import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick, ref, type App } from 'vue'

import type { PoolKeyDetail } from '@/api/endpoints/pool'
import PoolAccountBatchDialog from '../PoolAccountBatchDialog.vue'

const apiMocks = vi.hoisted(() => ({
  batchActionPoolKeys: vi.fn(),
  getPoolBatchDeleteTask: vi.fn(),
  resolvePoolKeySelection: vi.fn(),
  exportKey: vi.fn(),
  refreshProviderQuota: vi.fn(),
  refreshProviderOAuth: vi.fn(),
}))

vi.mock('@/api/endpoints/pool', () => ({
  batchActionPoolKeys: apiMocks.batchActionPoolKeys,
  getPoolBatchDeleteTask: apiMocks.getPoolBatchDeleteTask,
  resolvePoolKeySelection: apiMocks.resolvePoolKeySelection,
}))

vi.mock('@/api/endpoints/keys', () => ({
  exportKey: apiMocks.exportKey,
  refreshProviderQuota: apiMocks.refreshProviderQuota,
}))

vi.mock('@/api/endpoints/provider_oauth', () => ({
  refreshProviderOAuth: apiMocks.refreshProviderOAuth,
}))

vi.mock('@/stores/proxy-nodes', () => ({
  useProxyNodesStore: () => ({ nodes: [], ensureLoaded: vi.fn() }),
}))

vi.mock('@/composables/useToast', () => ({
  useToast: () => ({ success: vi.fn(), warning: vi.fn(), error: vi.fn() }),
}))

vi.mock('@/composables/useConfirm', () => ({
  useConfirm: () => ({ confirm: vi.fn().mockResolvedValue(true) }),
}))

vi.mock('@/features/providers/components/ProxyNodeSelect.vue', async () => {
  const { defineComponent } = await import('vue')
  return { default: defineComponent({ name: 'ProxyNodeSelectStub', render: () => null }) }
})

const mountedApps: Array<{ app: App, root: HTMLElement }> = []

function createKey(id: string, authType: PoolKeyDetail['auth_type']): PoolKeyDetail {
  return {
    key_id: id,
    key_name: id,
    is_active: true,
    auth_type: authType,
    api_formats: ['openai:chat'],
  } as PoolKeyDetail
}

async function settle(): Promise<void> {
  for (let index = 0; index < 6; index += 1) {
    await Promise.resolve()
    await nextTick()
  }
}

beforeEach(() => {
  apiMocks.batchActionPoolKeys.mockReset()
  apiMocks.refreshProviderQuota.mockReset().mockResolvedValue({
    success: 2,
    failed: 0,
    total: 2,
    results: [],
  })
  apiMocks.resolvePoolKeySelection.mockReset()
})

afterEach(() => {
  for (const { app, root } of mountedApps.splice(0)) {
    app.unmount()
    root.remove()
  }
})

describe('PoolAccountBatchDialog initial selection', () => {
  it.each([
    { action: 'delete', count: 100, sizes: [100] },
    { action: 'delete', count: 101, sizes: [100, 1] },
    { action: 'delete', count: 201, sizes: [100, 100, 1] },
    { action: 'enable', count: 201, sizes: [100, 100, 1] },
    { action: 'disable', count: 201, sizes: [100, 100, 1] },
    { action: 'clear_proxy', count: 201, sizes: [100, 100, 1] },
  ] as const)('executes $action for $count filtered accounts within the API batch limit', async ({ action, count, sizes }) => {
    const selectedKeys = Array.from({ length: count }, (_, i) => createKey(`key-${i}`, 'oauth'))
    const filters = { status: 'expired', oauth_refresh: 'no' } as const
    apiMocks.resolvePoolKeySelection.mockResolvedValue({ total: count, items: selectedKeys })
    apiMocks.batchActionPoolKeys.mockImplementation(async (_providerId, body) => {
      if (body.key_ids.length > 100) {
        throw { response: { data: { detail: 'key_ids 最多 100 个' } } }
      }
      return { affected: body.key_ids.length }
    })
    const changed = vi.fn()
    const root = document.createElement('div')
    document.body.appendChild(root)
    const app = createApp(defineComponent({
      setup: () => () => h(PoolAccountBatchDialog, {
        modelValue: true,
        providerId: 'provider-1',
        selectedKeys: selectedKeys.slice(0, 10),
        selectAllFiltered: true,
        selectedCount: count,
        selectionFilters: filters,
        initialAction: action,
        onChanged: changed,
      }),
    }))
    app.mount(root)
    mountedApps.push({ app, root })

    await vi.waitFor(() => expect(changed).toHaveBeenCalledTimes(1))
    expect(apiMocks.resolvePoolKeySelection).toHaveBeenCalledWith('provider-1', filters)
    const calls = apiMocks.batchActionPoolKeys.mock.calls
    expect(calls.map(([, body]) => body.key_ids.length)).toEqual(sizes)
    expect(calls.flatMap(([, body]) => body.key_ids)).toEqual(selectedKeys.map(key => key.key_id))
    expect(calls.every(([providerId, body]) => providerId === 'provider-1' && body.action === action)).toBe(true)
    expect(document.body.textContent).toContain(`执行完成：成功 ${count}，失败 0，跳过 0`)
  })

  it('shows the API failure reason and continues with the remaining delete batches', async () => {
    const selectedKeys = Array.from({ length: 201 }, (_, i) => createKey(`key-${i}`, 'oauth'))
    apiMocks.batchActionPoolKeys
      .mockResolvedValueOnce({ affected: 100 })
      .mockRejectedValueOnce({ response: { data: { detail: '数据库暂时不可用' } } })
      .mockResolvedValueOnce({ affected: 1 })
    const changed = vi.fn()
    const root = document.createElement('div')
    document.body.appendChild(root)
    const app = createApp(defineComponent({
      setup: () => () => h(PoolAccountBatchDialog, {
        modelValue: true,
        providerId: 'provider-1',
        selectedKeys,
        selectAllFiltered: false,
        selectedCount: selectedKeys.length,
        selectionFilters: { status: 'all' },
        initialAction: 'delete',
        onChanged: changed,
      }),
    }))
    app.mount(root)
    mountedApps.push({ app, root })

    await vi.waitFor(() => expect(changed).toHaveBeenCalledTimes(1))
    expect(apiMocks.batchActionPoolKeys).toHaveBeenCalledTimes(3)
    expect(document.body.textContent).toContain('执行完成：成功 101，失败 100，跳过 0；失败原因：数据库暂时不可用')
  })

  it('runs the action selected from the header menu when the dialog opens', async () => {
    const selectedKeys = [createKey('oauth-key', 'oauth'), createKey('api-key', 'api_key')]
    const open = ref(false)
    const root = document.createElement('div')
    document.body.appendChild(root)
    const app = createApp(defineComponent({
      setup() {
        return () => h(PoolAccountBatchDialog, {
          modelValue: open.value,
          providerId: 'provider-1',
          providerName: 'Provider 1',
          providerType: 'codex',
          selectedKeys,
          selectAllFiltered: false,
          selectedCount: selectedKeys.length,
          selectionFilters: { status: 'all' },
          initialAction: 'refresh_quota',
          'onUpdate:modelValue': (value: boolean) => { open.value = value },
        })
      },
    }))
    app.mount(root)
    mountedApps.push({ app, root })

    open.value = true
    await settle()

    expect(apiMocks.refreshProviderQuota).toHaveBeenCalledTimes(1)
    expect(apiMocks.refreshProviderQuota).toHaveBeenCalledWith('provider-1', ['oauth-key', 'api-key'])
  })

  it('opens required configuration instead of executing an incomplete menu action', async () => {
    const selectedKeys = [createKey('api-key', 'api_key')]
    const open = ref(true)
    const root = document.createElement('div')
    document.body.appendChild(root)
    const app = createApp(defineComponent({
      setup() {
        return () => h(PoolAccountBatchDialog, {
          modelValue: open.value,
          providerId: 'provider-1',
          providerName: 'Provider 1',
          providerType: 'openai',
          selectedKeys,
          selectAllFiltered: false,
          selectedCount: selectedKeys.length,
          selectionFilters: { status: 'all' },
          initialAction: 'set_proxy',
          'onUpdate:modelValue': (value: boolean) => { open.value = value },
        })
      },
    }))
    app.mount(root)
    mountedApps.push({ app, root })

    await settle()

    expect(document.body.textContent).toContain('选择要绑定的代理节点')
    expect(apiMocks.refreshProviderQuota).not.toHaveBeenCalled()
  })

  it('uses explicit table selections without loading a second account list', async () => {
    const selectedKeys = [createKey('oauth-key', 'oauth'), createKey('api-key', 'api_key')]
    const open = ref(false)
    const root = document.createElement('div')
    document.body.appendChild(root)
    const app = createApp(defineComponent({
      setup() {
        return () => h(PoolAccountBatchDialog, {
          modelValue: open.value,
          providerId: 'provider-1',
          providerName: 'Provider 1',
          providerType: 'codex',
          selectedKeys,
          selectAllFiltered: false,
          selectedCount: selectedKeys.length,
          selectionFilters: { status: 'all' },
          'onUpdate:modelValue': (value: boolean) => { open.value = value },
        })
      },
    }))
    app.mount(root)
    mountedApps.push({ app, root })

    open.value = true
    await settle()

    expect(document.body.textContent).toContain('已选择 2 个账号')
    expect(apiMocks.resolvePoolKeySelection).not.toHaveBeenCalled()

    const actionButton = Array.from(document.querySelectorAll<HTMLButtonElement>('button'))
      .find(button => button.textContent?.includes('刷新额度'))
    actionButton?.click()
    await settle()
    expect(apiMocks.refreshProviderQuota).toHaveBeenCalledWith('provider-1', ['oauth-key', 'api-key'])
  })

  it('resolves all filtered table results with the supplied filter snapshot', async () => {
    const selectedKeys = [createKey('visible-key', 'api_key')]
    apiMocks.resolvePoolKeySelection.mockResolvedValue({
      total: 2,
      items: [
        { key_id: 'visible-key', key_name: 'visible-key', auth_type: 'api_key' },
        { key_id: 'hidden-page-key', key_name: 'hidden-page-key', auth_type: 'api_key' },
      ],
    })
    const open = ref(true)
    const root = document.createElement('div')
    document.body.appendChild(root)
    const app = createApp(defineComponent({
      setup() {
        return () => h(PoolAccountBatchDialog, {
          modelValue: open.value,
          providerId: 'provider-1',
          providerName: 'Provider 1',
          providerType: 'codex',
          selectedKeys,
          selectAllFiltered: true,
          selectedCount: 2,
          selectionFilters: { search: 'inactive', status: 'inactive' },
          'onUpdate:modelValue': (value: boolean) => { open.value = value },
        })
      },
    }))
    app.mount(root)
    mountedApps.push({ app, root })

    await settle()
    const actionButton = Array.from(document.querySelectorAll<HTMLButtonElement>('button'))
      .find(button => button.textContent?.includes('刷新额度'))
    expect(actionButton).not.toBeUndefined()
    actionButton?.click()
    await settle()

    expect(apiMocks.resolvePoolKeySelection).toHaveBeenCalledWith('provider-1', {
      search: 'inactive',
      status: 'inactive',
    })
  })
})
