import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick, type App } from 'vue'
import BusinessTasks from '../BusinessTasks.vue'
import TaskProgress from '../TaskProgress.vue'

const { get } = vi.hoisted(() => ({ get: vi.fn() }))
vi.mock('@/api/client', () => ({ default: { get } }))
vi.mock('@/composables/useClipboard', () => ({ useClipboard: () => ({ copyToClipboard: vi.fn() }) }))
vi.mock('@/components/ui', async () => {
  const { defineComponent, h } = await import('vue')
  const passthrough = (tag: string) => defineComponent({
    setup(_, { slots }) { return () => h(tag, slots.default?.()) },
  })
  return {
    Badge: passthrough('span'), Button: passthrough('button'), Card: passthrough('section'),
    Table: passthrough('table'), TableHeader: passthrough('thead'), TableBody: passthrough('tbody'),
    TableHead: passthrough('th'), TableCell: passthrough('td'), TableRow: passthrough('tr'),
    Select: passthrough('div'), SelectContent: passthrough('div'), SelectItem: passthrough('div'),
    SelectTrigger: passthrough('div'), SelectValue: passthrough('span'),
    Input: defineComponent({
      props: { modelValue: String }, emits: ['update:modelValue'],
      setup(props, { emit }) {
        return () => h('input', { value: props.modelValue, onInput: (e: Event) => emit('update:modelValue', (e.target as HTMLInputElement).value) })
      },
    }),
    Pagination: defineComponent({
      props: { current: Number }, emits: ['update:current'],
      setup(props, { emit }) {
        return () => h('button', { 'data-next-page': '', onClick: () => emit('update:current', (props.current || 1) + 1) }, 'next')
      },
    }),
    Dialog: defineComponent({
      props: { open: Boolean },
      setup(props, { slots }) { return () => props.open ? h('aside', [slots.default?.(), slots.footer?.()]) : null },
    }),
  }
})

const mounts: Array<{ app: App; root: HTMLElement }> = []
function mount(component: Parameters<typeof createApp>[0], props = {}) {
  const root = document.createElement('div')
  document.body.appendChild(root)
  const app = createApp(component, props)
  app.component('RouterLink', defineComponent({ setup(_, { slots }) { return () => h('a', slots.default?.()) } }))
  app.mount(root)
  mounts.push({ app, root })
  return root
}
async function settle() {
  for (let i = 0; i < 6; i++) await Promise.resolve()
  await nextTick()
}
const list = (id = 'img-task-1') => ({ data: { items: [{ task_id: id, model: 'gpt-image-2', status: 'running', stage: 'generating', progress: 40, created_at: 100 }], total: 50, stats: { running: 50 } } })
function search(root: HTMLElement, value: string) {
  const input = root.querySelector<HTMLInputElement>('[aria-label="搜索 Task ID"]')!
  input.value = value
  input.dispatchEvent(new Event('input', { bubbles: true }))
}
beforeEach(() => {
  vi.useFakeTimers()
  get.mockReset()
  get.mockImplementation((url: string) => Promise.resolve(url.endsWith('/stats')
    ? { data: { total: 50, by_status: { processing: 50 } } }
    : list()))
})
afterEach(() => {
  for (const { app, root } of mounts.splice(0)) { app.unmount(); root.remove() }
  vi.useRealTimers()
})

describe('business task filters', () => {
  it.each(['image', 'video'] as const)('searches %s task IDs on the server and resets the page', async kind => {
    const root = mount(BusinessTasks, { kind })
    await settle()
    root.querySelector<HTMLButtonElement>('[data-next-page]')!.click()
    await settle()
    expect(get).toHaveBeenCalledWith(`/api/admin/${kind}-tasks`, expect.objectContaining({ params: expect.objectContaining({ page: 2 }) }))
    search(root, '  TASK-123  ')
    await nextTick()
    await vi.advanceTimersByTimeAsync(400)
    await settle()
    expect(get).toHaveBeenCalledWith(`/api/admin/${kind}-tasks`, expect.objectContaining({ params: expect.objectContaining({ task_id: 'TASK-123', page: 1 }) }))
    if (kind === 'video') {
      expect(get).toHaveBeenCalledWith('/api/admin/video-tasks/stats', expect.objectContaining({ params: expect.objectContaining({ task_id: 'TASK-123' }) }))
    }
    search(root, '')
    await nextTick()
    await vi.advanceTimersByTimeAsync(400)
    expect(get).toHaveBeenLastCalledWith(kind === 'video' ? '/api/admin/video-tasks/stats' : '/api/admin/image-tasks', expect.objectContaining({ params: expect.objectContaining({ task_id: undefined }) }))
  })

  it('ignores an older response after the search changes', async () => {
    let resolveOld!: (value: ReturnType<typeof list>) => void
    get.mockImplementationOnce(() => new Promise(resolve => { resolveOld = resolve }))
    const root = mount(BusinessTasks, { kind: 'image' })
    await settle()
    search(root, 'new')
    await nextTick()
    get.mockResolvedValue(list('new-result'))
    await vi.advanceTimersByTimeAsync(400)
    await settle()
    resolveOld(list('stale-result'))
    await settle()
    expect(root.textContent).toContain('new-result')
    expect(root.textContent).not.toContain('stale-result')
  })
})

describe('task progress', () => {
  it.each([[140, '100'], [-2, '0'], [42.2, '42']] as const)('clamps and rounds progress %s', (value, expected) => {
    const root = mount(TaskProgress, { value, label: '生成中' })
    expect(root.querySelector('[role="progressbar"]')?.getAttribute('aria-valuenow')).toBe(expected)
    expect(root.textContent).toContain(`${expected}%`)
  })
  it('keeps unknown progress distinct from zero', () => {
    const root = mount(TaskProgress, { value: null, label: '等待上游进度' })
    expect(root.querySelector('[role="progressbar"]')?.hasAttribute('aria-valuenow')).toBe(false)
    expect(root.textContent).not.toContain('0%')
  })
})
