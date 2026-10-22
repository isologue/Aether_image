<template>
  <CardSection
    title="图片存储"
    description="设置生图、编辑图片接口的返回格式、本地存储和 WebDAV 同步"
  >
    <template #actions>
      <Button
        size="sm"
        :disabled="!loaded || !!busy || !dirty"
        @click="save"
      >
        {{ busy === 'save' ? '保存中...' : '保存' }}
      </Button>
    </template>
    <div
      v-if="!loaded"
      class="text-sm text-muted-foreground"
    >
      {{ loadError || '正在读取图片配置...' }}
      <Button
        v-if="loadError"
        size="sm"
        variant="outline"
        class="ml-3"
        @click="load"
      >
        重试
      </Button>
    </div>
    <fieldset
      v-else
      :disabled="!!busy"
      class="space-y-6 min-w-0"
    >
      <div>
        <Label for="image-storage-mode">存储方式</Label>
        <select
          id="image-storage-mode"
          v-model="form.storage_mode"
          class="mt-1 flex h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
        >
          <option value="local">
            本地
          </option>
          <option value="webdav">
            WebDAV
          </option>
          <option value="local_webdav">
            本地 + WebDAV
          </option>
        </select>
        <p class="mt-1 text-xs text-muted-foreground">
          {{ storesLocal ? (storesWebdav ? '新图片保存到本地，并上传到 WebDAV。' : '新图片只保存到本地。') : '新图片直接上传到 WebDAV，不保存本地副本。已有本地图片保留，可按清理配置删除。' }}
        </p>
      </div>
      <div class="grid gap-4 md:grid-cols-2">
        <div>
          <Label for="image-return-format">默认返回格式</Label>
          <select
            id="image-return-format"
            v-model="form.return_format"
            class="mt-1 flex h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
          >
            <option value="base64">
              Base64（b64_json）
            </option><option value="url">
              图片 URL
            </option>
          </select>
        </div>
        <div class="flex flex-col justify-center gap-1">
          <label class="flex items-center gap-2 text-sm"><Checkbox v-model="form.honor_response_format" />遵循请求中的 response_format</label>
          <p class="text-xs text-muted-foreground">
            默认关闭，按系统格式返回；开启后请求的 url / b64_json 优先，未传则按系统格式。
          </p>
        </div>
        <div
          v-if="storesLocal"
          class="md:col-span-2"
        >
          <Label for="image-public-url">本地图片公开站点地址</Label><Input
            id="image-public-url"
            v-model="form.public_base_url"
            placeholder="https://img.example.com"
            class="mt-1"
          />
          <p class="mt-1 text-xs text-muted-foreground">
            返回地址为此站点下的 /media/images/…；留空使用 AETHER_PUBLIC_BASE_URL 或请求地址。域名需解析并反代到 Aether。
          </p>
        </div>
      </div>
      <div class="space-y-2 border-t border-border pt-5">
        <label class="flex items-center gap-2 text-sm font-medium"><input
          type="checkbox"
          id="image-resize-enabled"
          v-model="form.resize_enabled"
          class="h-4 w-4 rounded border-border/60 bg-card/80 text-primary shadow-sm focus:ring-2 focus:ring-primary/40 focus:ring-offset-1 accent-primary"
        >按请求分辨率自动缩放</label>
        <p class="text-xs text-muted-foreground">
          使用 Sharp / Lanczos3，在返回和存储前按 size（如 2048x2048）缩放。未传 size 或为 auto 时保留原图；尺寸相同不重复处理。保存后对新请求生效。
        </p>
        <p class="text-xs text-muted-foreground">
          保持原图比例，比例不同时居中补边；支持 PNG、JPEG、WebP。单边最多 8192 像素，总计最多 3200 万像素。传统插值放大不会生成新的细节。
        </p>
      </div>
      <div
        v-if="storesWebdav"
        class="space-y-4 border-t border-border pt-5"
      >
        <p class="text-sm font-medium">
          WebDAV 配置
        </p>
        <p class="text-xs text-muted-foreground">
          URL 模式等待上传和公开访问校验成功后返回 WebDAV 地址；{{ storesLocal ? '失败时回退本地地址。' : '失败时返回存储错误。' }}Base64 模式仍返回 b64_json，并由后台上传。
        </p>
        <div class="grid gap-4 md:grid-cols-2">
          <div>
            <Label for="image-dav-url">WebDAV URL</Label><Input
              id="image-dav-url"
              v-model="form.webdav_url"
              placeholder="http://host:8088/dav"
              class="mt-1"
            />
          </div>
          <div>
            <Label for="image-dav-public">WebDAV 公开访问前缀</Label><Input
              id="image-dav-public"
              v-model="form.webdav_public_url"
              placeholder="https://img.example.com/images"
              class="mt-1"
            />
          </div>
          <div>
            <Label for="image-dav-user">用户名</Label><Input
              id="image-dav-user"
              v-model="form.webdav_username"
              autocomplete="off"
              class="mt-1"
            />
          </div>
          <div>
            <Label for="image-dav-password">密码</Label><Input
              id="image-dav-password"
              v-model="password"
              type="password"
              autocomplete="new-password"
              :placeholder="passwordSet ? '已配置，留空保留原密码' : '请输入密码'"
              class="mt-1"
            />
            <label
              v-if="passwordSet"
              class="mt-2 flex items-center gap-2 text-xs"
            ><Checkbox v-model="clearPassword" />保存时清除已配置密码</label>
          </div>
          <div class="md:col-span-2">
            <Label for="image-dav-root">WebDAV 根路径</Label><Input
              id="image-dav-root"
              v-model="form.webdav_root"
              placeholder="aether/images"
              class="mt-1"
            />
            <p class="mt-1 text-xs text-muted-foreground">
              上传到 URL / 根路径 / 日期 / 文件名；公开前缀必须映射到这个根目录，不需要访问者登录。
            </p>
          </div>
        </div>
        <div class="flex flex-wrap gap-2">
          <Button
            size="sm"
            variant="outline"
            :disabled="dirty || !!busy"
            @click="testWebdav"
          >
            {{ busy === 'test' ? '测试中...' : '测试 WebDAV' }}
          </Button>
          <Button
            size="sm"
            variant="outline"
            :disabled="dirty || !!busy || stats?.sync.running"
            @click="syncImages"
          >
            补传已有本地图片
          </Button>
          <span
            v-if="dirty"
            class="self-center text-xs text-muted-foreground"
          >保存后可测试和执行维护操作</span>
        </div>
        <div
          v-if="testResult"
          class="space-y-1 rounded-md border border-border p-3 text-sm"
          role="status"
        >
          <p
            v-for="step in testSteps"
            :key="step.label"
          >
            <span :class="step.success ? 'text-green-600' : 'text-destructive'">{{ step.success ? '通过' : '失败' }}</span> · {{ step.label }}{{ step.error ? '：' + step.error : '' }}
          </p>
          <p class="text-xs text-muted-foreground">
            测试只上传一个临时图片并尝试删除它。
          </p>
        </div>
      </div>
      <div class="space-y-4 border-t border-border pt-5">
        <label class="flex items-center gap-2 text-sm font-medium"><Checkbox v-model="form.cleanup_enabled" />启用本地图片定时清理</label>
        <div class="grid gap-4 md:grid-cols-2">
          <div>
            <Label for="image-retention">删除多少小时以前的图片</Label><Input
              id="image-retention"
              type="number"
              min="1"
              max="87600"
              :model-value="form.retention_hours"
              class="mt-1"
              @update:model-value="form.retention_hours = Number($event)"
            />
          </div>
          <div>
            <Label for="image-interval">每隔多少小时清理一次</Label><Input
              id="image-interval"
              type="number"
              min="1"
              max="8760"
              :model-value="form.cleanup_interval_hours"
              class="mt-1"
              @update:model-value="form.cleanup_interval_hours = Number($event)"
            />
          </div>
        </div>
        <p class="text-xs text-muted-foreground">
          默认关闭。自动清理会删除所有过期本地图片，包括待同步和同步失败的图片；正在读取或上传的图片会跳过，WebDAV 文件保留。清理会使过期的本地 URL 失效。
        </p>
        <label
          v-if="storesWebdav"
          class="flex items-center gap-2 text-xs"
        ><Checkbox v-model="includeFailed" />手动清理时也删除过期待同步或同步失败的本地图片</label>
        <Button
          size="sm"
          variant="outline"
          :disabled="dirty || !!busy"
          @click="cleanup"
        >
          {{ busy === 'cleanup' ? '清理中...' : '立即按保留时间清理' }}
        </Button>
      </div>
    </fieldset>
    <div
      v-if="loaded"
      class="mt-6 space-y-3 border-t border-border pt-5"
    >
      <div class="flex items-center justify-between">
        <p class="text-sm font-medium">
          本地资源占用
        </p><Button
          size="sm"
          variant="outline"
          :disabled="refreshing"
          @click="refreshStats"
        >
          {{ refreshing ? '刷新中...' : '刷新统计' }}
        </Button>
      </div>
      <p
        v-if="statsError"
        class="text-sm text-destructive"
      >
        {{ statsError }}
      </p>
      <template v-if="stats">
        <div class="grid grid-cols-2 gap-4 md:grid-cols-4">
          <div>
            <p class="text-xs text-muted-foreground">
              图片数量
            </p><p class="mt-1 text-lg font-medium">
              {{ stats.count }}
            </p>
          </div>
          <div>
            <p class="text-xs text-muted-foreground">
              图片占用
            </p><p class="mt-1 text-lg font-medium">
              {{ bytes(stats.bytes) }}
              <span class="block text-xs text-muted-foreground">异步任务输入占用：{{ bytes(stats.task_input_bytes || 0) }}</span>
            </p>
          </div>
          <div>
            <p class="text-xs text-muted-foreground">
              所在磁盘可用
            </p><p class="mt-1 text-lg font-medium">
              {{ stats.disk ? bytes(stats.disk.available) : '未知' }}
            </p>
          </div>
          <div>
            <p class="text-xs text-muted-foreground">
              磁盘已用比例
            </p><p class="mt-1 text-lg font-medium">
              {{ stats.disk ? stats.disk.used_percent.toFixed(1) + '%' : '未知' }}
            </p>
          </div>
        </div>
        <p class="break-all text-xs text-muted-foreground">
          目录：{{ stats.directory }} · 当前节点统计 · 更新于 {{ date(stats.updated_at) }}
        </p>
        <p
          v-if="stats.webdav_enabled"
          class="text-sm"
        >
          WebDAV 已同步 {{ stats.synced }} · 待同步 {{ stats.pending }} · 失败 {{ stats.failed }} · 过期待处理 {{ stats.expired_pending }}
        </p>
        <p
          v-if="stats.sync.running || stats.sync.total"
          class="text-sm"
        >
          补传{{ stats.sync.running ? '进行中' : '已结束' }}：成功 {{ stats.sync.completed }} / {{ stats.sync.total }}，失败 {{ stats.sync.failed }}{{ stats.sync.error ? '；' + stats.sync.error : '' }}
        </p>
        <p class="text-xs text-muted-foreground">
          上次清理：{{ date(stats.last_cleanup?.started_at) }} · 下次清理：{{ date(stats.next_cleanup_at) }}
        </p>
        <p
          v-if="stats.last_cleanup"
          class="text-xs text-muted-foreground"
        >
          删除 {{ stats.last_cleanup.deleted }} 张，释放 {{ bytes(stats.last_cleanup.freed_bytes) }}，保留待同步 {{ stats.last_cleanup.skipped_pending }} 张，错误 {{ stats.last_cleanup.errors }} 项
        </p>
        <ul
          v-if="stats.failure_details?.length"
          class="space-y-1 text-xs text-destructive"
        >
          <li
            v-for="f in stats.failure_details"
            :key="f.id"
            class="break-all"
          >
            {{ f.id }} · 尝试 {{ f.attempts }} 次 · {{ f.error }}
          </li>
        </ul>
      </template>
    </div>
  </CardSection>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import axios from 'axios'
import { CardSection } from '@/components/layout'
import Button from '@/components/ui/button.vue'
import Label from '@/components/ui/label.vue'
import Input from '@/components/ui/input.vue'
import Checkbox from '@/components/ui/checkbox.vue'
import { adminApi } from '@/api/admin'
import { defaultImageStorageConfig, normalizeImageStorageConfig, imageStorageApi, validateImageStorageConfig, type ImageStorageConfig, type ImageStorageStats, type WebdavTestResult } from '@/api/endpoints/images'
import { useToast } from '@/composables/useToast'
const { success, error } = useToast()
const form = ref(defaultImageStorageConfig())
const saved = ref('')
const loaded = ref(false)
const loadError = ref('')
const busy = ref('')
const password = ref('')
const passwordSet = ref(false)
const clearPassword = ref(false)
const includeFailed = ref(false)
const stats = ref<ImageStorageStats | null>(null)
const refreshing = ref(false)
const statsError = ref('')
const testResult = ref<WebdavTestResult | null>(null)
let poll: ReturnType<typeof setTimeout> | undefined
let disposed = false
const storesLocal = computed(() => form.value.storage_mode !== 'webdav')
const storesWebdav = computed(() => form.value.storage_mode !== 'local')
const dirty = computed(() => JSON.stringify(form.value) !== saved.value || !!password.value || clearPassword.value)
const testSteps = computed(() => testResult.value ? [
  { label: '上传', ...testResult.value.upload },
  { label: '匿名访问与图片内容校验', ...testResult.value.public_access },
  { label: '删除测试图片', ...testResult.value.delete },
] : [])
const message = (e: unknown) => axios.isAxiosError(e) ? String(e.response?.data?.detail || e.message) : e instanceof Error ? e.message : '操作失败'
function bytes(n: number) {
  if (n < 1024) return `${n} B`
  const units = ['KiB', 'MiB', 'GiB', 'TiB']
  let u = -1
  do { n /= 1024; u++ } while (n >= 1024 && u < units.length - 1)
  return `${n.toFixed(2)} ${units[u]}`
}
function date(n?: number | null) { return n ? new Date(n * 1000).toLocaleString() : '—' }
async function load() {
  loadError.value = ''
  try {
    const [config, secret] = await Promise.all([adminApi.getSystemConfig('image_storage'), adminApi.getSystemConfig('image_webdav_password')])
    form.value = normalizeImageStorageConfig((config.value as Partial<ImageStorageConfig> & { webdav_enabled?: boolean }) || {})
    passwordSet.value = secret.is_set === true
    saved.value = JSON.stringify(form.value); loaded.value = true
    await refreshStats()
  } catch (e) { loadError.value = message(e) }
}
async function save() {
  const invalid = validateImageStorageConfig(form.value)
  if (invalid) { error(invalid); return }
  busy.value = 'save'
  try {
    if (form.value.cleanup_enabled) {
      const policy=await adminApi.getSystemConfig('async_task_retention_days')
      const days=Number(policy.value ?? 30)
      const minimum=Math.max(1,Math.ceil(form.value.retention_hours/24))
      if (!Number.isInteger(days) || days<minimum) { error(`请先将记录清理策略中的异步任务记录保留天数调整为至少 ${minimum} 天`); return }
    }
    // Save the secret before enabling a destination; never populate the input with a masked value.
    if (password.value || clearPassword.value) {
      await adminApi.updateSystemConfig('image_webdav_password', clearPassword.value ? '' : password.value)
      passwordSet.value = !clearPassword.value; password.value = ''; clearPassword.value = false
    }
    // All config fields are primitives; spread the Vue proxy into a plain object.
    const configToSave = { ...form.value }
    await adminApi.updateSystemConfig('image_storage', configToSave)
    const persisted = await adminApi.getSystemConfig('image_storage')
    form.value = normalizeImageStorageConfig((persisted.value as Partial<ImageStorageConfig> & { webdav_enabled?: boolean }) || {})
    saved.value = JSON.stringify(form.value); testResult.value = null
    success('图片存储配置已保存'); await refreshStats()
  } catch (e) { error(message(e)) } finally { busy.value = '' }
}
async function refreshStats() {
  if (refreshing.value || disposed) return
  if (poll) clearTimeout(poll)
  refreshing.value = true; statsError.value = ''
  try { stats.value = await imageStorageApi.stats() }
  catch (e) { statsError.value = message(e) }
  finally {
    refreshing.value = false
    if (!disposed && stats.value?.sync.running) poll = setTimeout(refreshStats, 3000)
  }
}
async function testWebdav() {
  busy.value = 'test'; testResult.value = null
  try { testResult.value = await imageStorageApi.test(); if (testResult.value.success) success('WebDAV 上传、公开访问和删除均通过') }
  catch (e) { error(message(e)) } finally { busy.value = '' }
}
async function syncImages() {
  busy.value = 'sync'
  try { await imageStorageApi.sync(); success('已有本地图片补传已开始'); await refreshStats() }
  catch (e) { error(message(e)) } finally { busy.value = '' }
}
async function cleanup() {
  busy.value = 'cleanup'
  try { const r = await imageStorageApi.cleanup(includeFailed.value); success(`已删除 ${r.deleted} 张图片，释放 ${bytes(r.freed_bytes)}`); await refreshStats() }
  catch (e) { error(message(e)) } finally { busy.value = '' }
}
onMounted(load)
onUnmounted(() => { disposed = true; if (poll) clearTimeout(poll) })
</script>
