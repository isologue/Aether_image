<template>
  <div class="space-y-6 pb-8">
    <div class="grid grid-cols-2 lg:grid-cols-4 gap-4">
      <Card
        v-for="card in statCards"
        :key="card.label"
        variant="default"
        class="p-4"
      >
        <div class="flex items-center gap-3">
          <div
            class="w-10 h-10 shrink-0 rounded-lg flex items-center justify-center"
            :class="card.background"
          >
            <component
              :is="card.icon"
              class="w-5 h-5"
              :class="card.color"
            />
          </div>
          <div>
            <p class="text-2xl font-bold tabular-nums">
              {{ card.value }}
            </p>
            <p class="text-xs text-muted-foreground">
              {{ card.label }}
            </p>
          </div>
        </div>
      </Card>
    </div>
    <Card
      variant="default"
      class="overflow-hidden"
    >
      <div class="px-4 sm:px-6 py-3.5 border-b border-border/60">
        <div class="flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between">
          <h3 class="text-base font-semibold">
            {{ title }}
          </h3>
          <div class="flex flex-wrap items-center gap-2">
            <Select v-model="status">
              <SelectTrigger
                class="w-28 h-8 text-xs border-border/60"
                aria-label="筛选任务状态"
              >
                <SelectValue placeholder="全部状态" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">
                  全部状态
                </SelectItem>
                <SelectItem
                  v-for="s in statuses"
                  :key="s"
                  :value="s"
                >
                  {{ labels[s] || s }}
                </SelectItem>
              </SelectContent>
            </Select>
            <Input
              v-model="taskId"
              type="search"
              placeholder="搜索 Task ID…"
              aria-label="搜索 Task ID"
              class="h-8 w-44 flex-1 sm:flex-none text-xs"
            />
            <Input
              v-model="model"
              type="search"
              placeholder="筛选模型…"
              aria-label="筛选模型"
              class="h-8 w-32 text-xs"
            />
            <Button
              variant="ghost"
              size="icon"
              class="h-8 w-8 shrink-0"
              :disabled="loading"
              aria-label="刷新任务"
              @click="load"
            >
              <RefreshCw
                class="w-3.5 h-3.5"
                :class="{ 'animate-spin': loading }"
              />
            </Button>
          </div>
        </div>
      </div>
      <div
        v-if="error"
        role="alert"
        class="mx-4 my-3 rounded-lg border border-destructive/20 bg-destructive/5 p-3 text-sm text-destructive"
      >
        {{ error }}
      </div>
      <div
        v-if="loading && !tasks.length"
        class="p-12 text-center"
      >
        <Loader2 class="mx-auto h-8 w-8 animate-spin text-muted-foreground" />
        <p class="mt-2 text-sm text-muted-foreground">
          加载中…
        </p>
      </div>
      <div
        v-else-if="!tasks.length"
        class="p-12 text-center"
      >
        <component
          :is="kind === 'image' ? ImageIcon : Video"
          class="mx-auto h-10 w-10 text-muted-foreground/50"
        />
        <p class="mt-3 text-sm text-muted-foreground">
          {{ hasFilters ? '没有符合筛选条件的任务' : '暂无任务' }}
        </p>
      </div>
      <template v-else>
        <Table class="hidden md:table">
          <TableHeader>
            <TableRow>
              <TableHead class="pl-6">
                任务 / 模型
              </TableHead>
              <TableHead class="w-64">
                状态 / 进度
              </TableHead>
              <TableHead>耗时 / 切号</TableHead>
              <TableHead>计费</TableHead>
              <TableHead class="w-20 pr-6 text-right">
                操作
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            <TableRow
              v-for="task in tasks"
              :key="id(task)"
            >
              <TableCell class="max-w-xs pl-6 py-4">
                <p
                  class="truncate text-sm font-medium"
                  :title="task.model"
                >
                  {{ task.model || '—' }}
                </p>
                <button
                  class="mt-1 block max-w-full truncate text-left font-mono text-xs text-muted-foreground hover:text-primary"
                  :title="`复制 Task ID: ${id(task)}`"
                  @click="copyToClipboard(id(task))"
                >
                  {{ id(task) }}
                </button>
                <p class="mt-1 text-xs text-muted-foreground">
                  {{ date(task.created_at) }}
                </p>
              </TableCell>
              <TableCell class="py-4">
                <Badge
                  :variant="statusVariant(task)"
                  class="mb-2 text-xs"
                >
                  {{ statusLabel(task) }}
                </Badge>
                <TaskProgress
                  :value="progress(task)"
                  :label="stageLabel(task)"
                  :failed="isFailed(task)"
                />
              </TableCell>
              <TableCell class="py-4">
                <p class="text-sm tabular-nums">
                  {{ duration(task.created_at, task.completed_at) }}
                </p>
                <p class="mt-1 text-xs text-muted-foreground">
                  切号 {{ task.switch_count ?? task.retry_count ?? 0 }} 次
                </p>
              </TableCell>
              <TableCell class="py-4">
                <p class="text-xs">
                  {{ billing[task.billing_state] || task.billing_state || '见使用记录' }}
                </p>
                <p
                  v-if="task.actual_cost != null"
                  class="mt-1 text-xs tabular-nums text-muted-foreground"
                >
                  ${{ Number(task.actual_cost).toFixed(6) }}
                </p>
              </TableCell>
              <TableCell class="pr-6 text-right">
                <Button
                  variant="ghost"
                  size="icon"
                  class="h-8 w-8"
                  aria-label="查看任务详情"
                  @click="detail(task)"
                >
                  <Eye class="h-4 w-4" />
                </Button>
              </TableCell>
            </TableRow>
          </TableBody>
        </Table>
        <div class="divide-y divide-border/60 md:hidden">
          <div
            v-for="task in tasks"
            :key="id(task)"
            class="space-y-3 p-4"
          >
            <div class="flex items-center justify-between gap-3">
              <p class="truncate text-sm font-medium">
                {{ task.model || '—' }}
              </p>
              <Badge
                :variant="statusVariant(task)"
                class="shrink-0 text-xs"
              >
                {{ statusLabel(task) }}
              </Badge>
            </div>
            <button
              class="block w-full truncate text-left font-mono text-xs text-muted-foreground"
              :title="id(task)"
              @click="copyToClipboard(id(task))"
            >
              {{ id(task) }}
            </button>
            <TaskProgress
              :value="progress(task)"
              :label="stageLabel(task)"
              :failed="isFailed(task)"
            />
            <div class="flex items-center justify-between gap-3 text-xs text-muted-foreground">
              <span>{{ duration(task.created_at, task.completed_at) }} · 切号 {{ task.switch_count ?? task.retry_count ?? 0 }} 次</span>
              <span>{{ billing[task.billing_state] || task.billing_state || '见使用记录' }}</span>
            </div>
            <div class="flex items-center justify-between text-xs text-muted-foreground">
              <span>{{ date(task.created_at) }}</span>
              <Button
                variant="ghost"
                size="sm"
                class="h-7 text-xs"
                @click="detail(task)"
              >
                <Eye class="mr-1.5 h-3.5 w-3.5" />详情
              </Button>
            </div>
          </div>
        </div>
      </template>
      <Pagination
        :current="page"
        :total="total"
        :page-size="pageSize"
        @update:current="changePage"
        @update:page-size="changePageSize"
      />
    </Card>
    <Dialog
      :open="!!selected"
      :title="`${title}详情`"
      :description="selected ? id(selected) : ''"
      :icon="kind === 'image' ? ImageIcon : Video"
      size="4xl"
      @update:open="open => { if (!open) closeDetail() }"
    >
      <div
        v-if="selected"
        class="space-y-5"
      >
        <div class="flex items-center gap-2">
          <Badge :variant="statusVariant(selected)">
            {{ statusLabel(selected) }}
          </Badge>
          <span class="text-sm text-muted-foreground">{{ selected.model }}</span>
        </div>
        <TaskProgress
          :value="progress(selected)"
          :label="stageLabel(selected)"
          :failed="isFailed(selected)"
        />
        <div class="grid grid-cols-2 gap-4 rounded-lg border border-border/60 bg-muted/20 p-4 text-sm md:grid-cols-3">
          <div>
            <p class="text-xs text-muted-foreground">
              排队耗时
            </p><p class="mt-1 tabular-nums">
              {{ duration(selected.created_at, selected.started_at || selected.completed_at) }}
            </p>
          </div>
          <div>
            <p class="text-xs text-muted-foreground">
              执行耗时
            </p><p class="mt-1 tabular-nums">
              {{ selected.started_at ? duration(selected.started_at, selected.completed_at) : '—' }}
            </p>
          </div>
          <div>
            <p class="text-xs text-muted-foreground">
              总耗时
            </p><p class="mt-1 tabular-nums">
              {{ duration(selected.created_at, selected.completed_at) }}
            </p>
          </div>
          <div>
            <p class="text-xs text-muted-foreground">
              尝试 / 切号
            </p><p class="mt-1">
              {{ selected.attempt_count ?? 0 }} / {{ selected.switch_count ?? selected.retry_count ?? 0 }} 次
            </p>
          </div>
          <div>
            <p class="text-xs text-muted-foreground">
              最后一次尝试
            </p><p class="mt-1 tabular-nums">
              {{ selected.last_attempt_ms == null ? '—' : `${(selected.last_attempt_ms / 1000).toFixed(1)}s` }}
            </p>
          </div>
          <div>
            <p class="text-xs text-muted-foreground">
              计费状态
            </p><p class="mt-1">
              {{ billing[selected.billing_state] || selected.billing_state || '见使用记录' }}<span
                v-if="selected.actual_cost != null"
                class="ml-1 tabular-nums"
              >· ${{ Number(selected.actual_cost).toFixed(6) }}</span>
            </p>
          </div>
        </div>
        <p
          v-if="detailError"
          role="alert"
          class="text-sm text-destructive"
        >
          {{ detailError }}
        </p>
        <pre
          v-if="selected.error || selected.error_message"
          class="whitespace-pre-wrap break-words rounded-lg border border-destructive/20 bg-destructive/5 p-4 text-xs text-destructive"
        >{{ selected.error?.message || selected.error_message }}</pre>
        <pre
          v-if="selected.result?.refusal"
          class="whitespace-pre-wrap break-words rounded-lg bg-muted p-4 text-sm"
        >{{ selected.result.refusal }}</pre>
        <div
          v-if="selected.result?.data?.length"
          class="grid gap-4 md:grid-cols-2"
        >
          <div
            v-for="(item, index) in selected.result.data"
            :key="index"
            class="overflow-hidden rounded-lg border border-border/60"
          >
            <div class="flex items-center justify-center bg-muted/20 p-2">
              <img
                :src="imageUrl(item)"
                class="max-h-96 object-contain rounded"
                alt="生成结果"
              >
            </div>
            <div class="flex items-center justify-between gap-2 border-t border-border/60 p-3 text-xs">
              <a
                :href="imageUrl(item)"
                :download="`image-${index + 1}.png`"
                target="_blank"
                rel="noopener noreferrer"
                class="inline-flex items-center gap-1.5 text-primary hover:underline"
              ><ExternalLink class="h-3.5 w-3.5" />下载 / 打开</a>
              <Button
                v-if="item.url"
                variant="ghost"
                size="sm"
                class="h-7 text-xs"
                @click="copyToClipboard(item.url)"
              >
                <Copy class="mr-1.5 h-3.5 w-3.5" />复制 URL
              </Button>
            </div>
          </div>
        </div>
        <video
          v-if="selected.video_url"
          :src="selected.video_url"
          controls
          class="max-h-96 w-full rounded-lg border border-border/60"
        />
        <div
          v-if="selected.request_id"
          class="space-y-2 border-t border-border/60 pt-4 text-xs"
        >
          <p class="break-all text-muted-foreground">
            关联请求：<code>{{ selected.request_id }}</code>
          </p>
          <RouterLink
            :to="{ path: '/admin/usage', query: { request_id: selected.request_id } }"
            class="inline-flex items-center gap-1.5 text-primary hover:underline"
          >
            <ExternalLink class="h-3.5 w-3.5" />查看使用记录与计费明细
          </RouterLink>
        </div>
      </div>
      <template #footer>
        <Button
          variant="outline"
          @click="closeDetail"
        >
          关闭
        </Button>
      </template>
    </Dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch, onMounted, onActivated, onDeactivated, onUnmounted } from 'vue'
import axios from 'axios'
import { CheckCircle, Copy, ExternalLink, Eye, Image as ImageIcon, Loader2, RefreshCw, Video, XCircle, Zap } from 'lucide-vue-next'
import { Badge, Button, Card, Dialog, Input, Pagination, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui'
import apiClient from '@/api/client'
import { useClipboard } from '@/composables/useClipboard'
import TaskProgress from './TaskProgress.vue'

const props = defineProps<{ kind: 'image' | 'video' }>()
// Provider-specific result fields are kept intact for previews and error details.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Task = Record<string, any>
const { copyToClipboard } = useClipboard()
const tasks = ref<Task[]>([])
const selected = ref<Task | null>(null)
const stats = ref<Record<string, number>>({})
const status = ref('all')
const model = ref('')
const taskId = ref('')
const page = ref(1)
const pageSize = ref(20)
const total = ref(0)
const loading = ref(false)
const error = ref('')
const detailError = ref('')
const title = computed(() => props.kind === 'image' ? '图片任务' : '视频任务')
const hasFilters = computed(() => status.value !== 'all' || !!model.value.trim() || !!taskId.value.trim())
const labels: Record<string, string> = { queued: '排队中', pending: '等待提交', submitted: '已提交', running: '执行中', processing: '处理中', succeeded: '成功', completed: '成功', failed: '失败', cancelled: '已取消', expired: '已过期', deleted: '已删除' }
const stages: Record<string, string> = { queued: '排队中', preparing: '准备请求与输入', generating: '上游生成 / 切号补查', downloading: '下载结果', processing: '缩放与保存', completed: '结果就绪' }
const billing: Record<string, string> = { not_charged: '未扣费', pending_settlement: '结算中', charged: '已扣费', reservation_released: '预占已释放', refunded: '已退款', pending_release: '释放预占中' }
const activeStatuses = ['queued', 'running', 'pending', 'submitted', 'processing']
const statuses = props.kind === 'image' ? ['queued', 'running', 'succeeded', 'failed'] : ['pending', 'submitted', 'queued', 'processing', 'completed', 'failed', 'cancelled', 'expired', 'deleted']
const base = `/api/admin/${props.kind}-tasks`
const statCards = computed(() => [
  { label: '总任务数', value: stats.value.total ?? '—', icon: Zap, background: 'bg-primary/10', color: 'text-primary' },
  { label: '处理中', value: activeStatuses.reduce((sum, s) => sum + (stats.value[s] || 0), 0), icon: Loader2, background: 'bg-blue-500/10', color: 'text-blue-500' },
  { label: '已完成', value: (stats.value.succeeded || 0) + (stats.value.completed || 0), icon: CheckCircle, background: 'bg-green-500/10', color: 'text-green-500' },
  { label: '失败 / 过期', value: (stats.value.failed || 0) + (stats.value.expired || 0), icon: XCircle, background: 'bg-destructive/10', color: 'text-destructive' },
])
function normalize(t: Task): Task {
  return props.kind === 'video' ? { ...t, started_at: t.submitted_at, progress: t.progress_percent, stage: t.progress_message, error: t.error_message ? { message: t.error_message } : null } : t
}
const id = (t: Task) => String(t.task_id || t.id)
const time = (v: number | string | null | undefined) => typeof v === 'number' ? v * 1000 : v ? new Date(v).getTime() : Date.now()
const date = (v: number | string) => v == null ? '—' : new Date(time(v)).toLocaleString()
const duration = (a: number | string, b?: number | string) => a == null ? '—' : `${Math.max(0, (time(b) - time(a)) / 1000).toFixed(1)}s`
const isFailed = (t: Task) => ['failed', 'expired'].includes(t.status)
const statusLabel = (t: Task) => t.outcome === 'content_refused' ? '内容拒绝' : labels[t.status] || t.status
const statusVariant = (t: Task) => isFailed(t) ? 'destructive' : ['succeeded', 'completed'].includes(t.status) ? 'success' : activeStatuses.includes(t.status) ? 'secondary' : 'outline'
const stageLabel = (t: Task) => stages[t.stage] || t.stage || labels[t.status] || '等待上游进度'
const progress = (t: Task) => t.progress == null && t.progress_percent == null ? (['succeeded', 'completed'].includes(t.status) ? 100 : null) : Number(t.progress ?? t.progress_percent)
const imageUrl = (i: Task) => i.url || `data:${i.b64_json?.startsWith('/9j/') ? 'image/jpeg' : i.b64_json?.startsWith('UklGR') ? 'image/webp' : 'image/png'};base64,${i.b64_json}`
const message = (e: unknown) => axios.isAxiosError(e) ? String(e.response?.data?.error?.message || e.response?.data?.detail || e.message) : String(e)
let timer: ReturnType<typeof setTimeout> | undefined
let filterTimer: ReturnType<typeof setTimeout> | undefined
let active = false
let listVersion = 0
let controller: AbortController | undefined
async function load() {
  clearTimeout(timer)
  const version = ++listVersion
  controller?.abort()
  controller = new AbortController()
  const signal = controller.signal
  loading.value = true
  error.value = ''
  const params = { page: page.value, page_size: pageSize.value, status: status.value === 'all' ? undefined : status.value, model: model.value.trim() || undefined, task_id: taskId.value.trim() || undefined }
  try {
    const [response, summary] = await Promise.all([
      apiClient.get<{ items: Task[]; total: number; stats: Record<string, number> }>(base, { params, signal }),
      props.kind === 'video' ? apiClient.get<{ total: number; by_status: Record<string, number> }>(`${base}/stats`, { params: { model: params.model, task_id: params.task_id }, signal }) : Promise.resolve(null),
    ])
    if (version !== listVersion) return
    tasks.value = response.data.items.map(normalize)
    total.value = response.data.total
    stats.value = summary ? { ...summary.data.by_status, total: summary.data.total } : { ...response.data.stats, total: Object.values(response.data.stats).reduce((a, b) => a + b, 0) }
    if (selected.value && activeStatuses.includes(selected.value.status)) void detail(selected.value)
  } catch (e) {
    if (version === listVersion && !axios.isCancel(e)) error.value = message(e)
  } finally {
    if (version === listVersion) {
      loading.value = false
      if (active) timer = setTimeout(load, 5000)
    }
  }
}
let detailVersion = 0
async function detail(task: Task) {
  const version = ++detailVersion
  selected.value = task
  detailError.value = ''
  try {
    const { data } = await apiClient.get<Task>(`${base}/${encodeURIComponent(id(task))}`)
    if (version === detailVersion) selected.value = normalize(data)
  } catch (e) {
    if (version === detailVersion) detailError.value = message(e)
  }
}
function closeDetail() { detailVersion++; selected.value = null }
function changePage(value: number) { page.value = value; void load() }
function changePageSize(value: number) { pageSize.value = value; page.value = 1; void load() }
function reloadFilters() { clearTimeout(filterTimer); page.value = 1; void load() }
watch(status, reloadFilters)
watch([taskId, model], () => {
  clearTimeout(filterTimer)
  clearTimeout(timer)
  // Invalidate in-flight responses immediately, including during the debounce interval.
  listVersion++
  controller?.abort()
  filterTimer = setTimeout(reloadFilters, 400)
})
function activate() {
  if (active) return
  active = true
  void load()
}
function deactivate() {
  active = false
  clearTimeout(timer)
  clearTimeout(filterTimer)
  listVersion++
  controller?.abort()
  loading.value = false
  closeDetail()
}
onMounted(activate)
onActivated(activate)
onDeactivated(deactivate)
onUnmounted(deactivate)
</script>
