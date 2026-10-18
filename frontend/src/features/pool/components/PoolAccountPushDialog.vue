<template>
  <Dialog
    :model-value="modelValue"
    title="账号推送"
    description="接收外部程序通过 Sub2API 模式推送的账号"
    size="lg"
    :persistent="saving"
    @update:model-value="emit('update:modelValue', $event)"
  >
    <div class="space-y-5 py-2">
      <p
        v-if="loadError"
        role="alert"
        class="text-sm text-destructive"
      >
        {{ loadError }}
        <Button
          variant="outline"
          size="sm"
          @click="load"
        >
          重试
        </Button>
      </p>
      <p
        v-else-if="!loaded"
        class="text-sm text-muted-foreground"
      >
        正在读取配置...
      </p>
      <template v-else>
        <div class="flex items-center justify-between gap-4">
          <div>
            <Label for="account-push-enabled">启用账号推送</Label>
            <p class="mt-1 text-xs text-muted-foreground">
              定时推送由发送程序设置，Aether 接收后自动导入号池。
            </p>
          </div>
          <Switch
            id="account-push-enabled"
            v-model="form.enabled"
            :disabled="saving"
          />
        </div>
        <div class="space-y-2">
          <Label for="account-push-provider">目标提供商</Label>
          <select
            id="account-push-provider"
            v-model="form.provider_id"
            :disabled="saving"
            class="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
          >
            <option value="">
              选择 ChatGPT Web 或 Codex 提供商
            </option>
            <option
              v-for="provider in eligibleProviders"
              :key="provider.provider_id"
              :value="provider.provider_id"
            >
              {{ provider.provider_name }} · {{ provider.provider_type }}
            </option>
          </select>
          <p class="text-xs text-muted-foreground">
            同一提供商内重复推送的账号会更新凭据。账号将导入所选的目标提供商。
          </p>
          <p
            v-if="eligibleProviders.length === 0"
            class="text-xs text-amber-600"
          >
            请先创建 ChatGPT Web 或 Codex 提供商。
          </p>
        </div>
        <div class="space-y-2">
          <Label for="account-push-secret">推送 API Key</Label>
          <div class="flex gap-2">
            <Input
              ref="secretInputRef"
              id="account-push-secret"
              v-model="secret"
              :type="revealSecret ? 'text' : 'password'"
              autocomplete="new-password"
              :disabled="saving"
              :placeholder="secretSet ? '已配置，留空保留现有密钥' : '生成或输入至少 24 个字符的密钥'"
            />
            <Button
              variant="outline"
              :disabled="saving"
              @click="generateSecret"
            >
              生成
            </Button>
            <Button
              variant="outline"
              :disabled="!secret"
              @click="revealSecret = !revealSecret"
            >
              {{ revealSecret ? '隐藏' : '显示' }}
            </Button>
          </div>
          <div class="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              :disabled="!secret || saving"
              @click="copy(secret, 'secret')"
            >
              复制新密钥
            </Button>
            <span class="text-xs text-muted-foreground">{{ secretSet ? '已保存的密钥不再展示。重新生成会在保存后替换。' : '新密钥保存后生效，请复制到发送端。' }}</span>
          </div>
        </div>
        <div class="space-y-3 rounded-lg border border-border p-4 text-sm">
          <p class="font-medium">
            发送端填写方式
          </p>
          <p class="text-xs text-muted-foreground">
            服务类型选择 Sub2API，API URL 填下面的服务根地址，API Key 填推送密钥。
          </p>
          <Label for="account-push-base-url">API URL（服务根地址）</Label>
          <div class="flex gap-2">
            <Input
              ref="baseUrlInputRef"
              id="account-push-base-url"
              :model-value="baseUrl"
              readonly
            />
            <Button
              variant="outline"
              @click="copy(baseUrl, 'baseUrl')"
            >
              复制
            </Button>
          </div>
          <p class="break-all text-xs text-muted-foreground">
            Sub2API 接收路径：GET / POST /api/v1/admin/accounts/data，使用 X-API-Key 或 Bearer 推送密钥。请求体使用 application/json。
          </p>
          <p class="text-xs text-muted-foreground">
            单次请求最大 128 MiB，内部按每批 100 个账号顺序处理，全部完成后返回 account_created、account_updated、account_failed、errors；部分成功时 code 为 0，请检查失败数后重试。
          </p>
          <p class="text-xs text-muted-foreground">
            Sub2API 支持 OpenAI OAuth 账号备份包。账号刷新由 Aether 的导入流程处理，响应不会回传账号凭据。
          </p>
        </div>
        <div class="space-y-2 text-sm">
          <div class="flex items-center justify-between">
            <span class="font-medium">最近一次推送</span>
            <Button
              variant="ghost"
              size="sm"
              :disabled="refreshing"
              @click="refreshResult"
            >
              刷新
            </Button>
          </div>
          <p
            v-if="!lastResult"
            class="text-xs text-muted-foreground"
          >
            暂无推送记录
          </p>
          <template v-else>
            <p class="text-xs text-muted-foreground">
              {{ new Date(lastResult.received_at * 1000).toLocaleString() }} · {{ lastResult.success ? '全部成功' : '含失败项' }} · HTTP {{ lastResult.http_status }}
            </p>
            <p class="text-xs text-muted-foreground">
              新增 {{ lastResult.added ?? 0 }}，更新 {{ lastResult.updated ?? 0 }}，失败 {{ lastResult.failed ?? 0 }}
            </p>
          </template>
        </div>
      </template>
    </div>
    <template #footer>
      <Button
        variant="outline"
        :disabled="saving"
        @click="emit('update:modelValue', false)"
      >
        关闭
      </Button>
      <Button
        :disabled="!loaded || saving"
        @click="save"
      >
        {{ saving ? '保存中...' : '保存' }}
      </Button>
    </template>
  </Dialog>
</template>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import { Dialog, Button, Input, Label, Switch } from '@/components/ui'
import { adminApi } from '@/api/admin'
import type { PoolOverviewItem } from '@/api/endpoints/pool'
import { useClipboard } from '@/composables/useClipboard'
import { useToast } from '@/composables/useToast'
import { parseApiError } from '@/utils/errorParser'

const props = defineProps<{ modelValue: boolean, providers: PoolOverviewItem[], providerId?: string | null }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
const { success, error } = useToast()
const { copyToClipboard } = useClipboard()
const secretInputRef = ref<{ inputRef: HTMLInputElement | null } | null>(null)
const baseUrlInputRef = ref<{ inputRef: HTMLInputElement | null } | null>(null)
const form = ref({ enabled: false, provider_id: '' })
const secret = ref('')
const secretSet = ref(false)
const savedSecret = ref('')
const revealSecret = ref(false)
const loaded = ref(false)
const loadError = ref('')
const saving = ref(false)
const refreshing = ref(false)
interface PushResult { received_at: number, http_status: number, success: boolean, added: number | null, updated: number | null, failed: number | null }
const lastResult = ref<PushResult | null>(null)
const eligibleProviders = computed(() => props.providers.filter(provider => ['chatgpt_web', 'codex'].includes(provider.provider_type)))
const baseUrl = new URL(import.meta.env.VITE_API_URL || window.location.origin, window.location.origin).href.replace(/\/$/, '')
let loadSequence = 0
async function load() {
  const sequence = ++loadSequence
  loaded.value = false
  loadError.value = ''
  secret.value = ''
  savedSecret.value = ''
  revealSecret.value = false
  try {
    const [config, savedSecretConfig, result] = await Promise.all([
      adminApi.getSystemConfig('account_push'),
      adminApi.getSystemConfig('account_push_api_key'),
      adminApi.getSystemConfig('account_push_last_result'),
    ])
    if (sequence !== loadSequence || !props.modelValue) return
    const value = config.value as Partial<typeof form.value> | null
    form.value = { enabled: value?.enabled === true, provider_id: value?.provider_id || '' }
    if (!form.value.provider_id && eligibleProviders.value.some(provider => provider.provider_id === props.providerId)) form.value.provider_id = props.providerId || ''
    secretSet.value = savedSecretConfig.is_set === true
    secret.value = secretSet.value && typeof savedSecretConfig.value === 'string' ? savedSecretConfig.value : ''
    savedSecret.value = secret.value
    lastResult.value = result.value as PushResult | null
    loaded.value = true
  } catch (err) { if (sequence === loadSequence) loadError.value = parseApiError(err) }
}
function generateSecret() {
  secret.value = `aether_push_${Array.from(crypto.getRandomValues(new Uint8Array(32)), byte => byte.toString(16).padStart(2, '0')).join('')}`
  revealSecret.value = false
}
async function copy(value: string, target: 'secret' | 'baseUrl') {
  if (await copyToClipboard(value, false)) {
    success('已复制')
    return
  }

  if (target === 'secret') revealSecret.value = true
  await nextTick()
  const input = target === 'secret' ? secretInputRef.value?.inputRef : baseUrlInputRef.value?.inputRef
  input?.focus()
  input?.select()
  error(input
    ? '浏览器禁止自动复制，文本已选中，请按 Ctrl+C；一键复制请使用 HTTPS 打开'
    : '复制失败，请使用 HTTPS 打开后重试')
}
async function save() {
  if (form.value.enabled && !eligibleProviders.value.some(provider => provider.provider_id === form.value.provider_id)) { error('请选择 ChatGPT Web 或 Codex 提供商'); return }
  if (form.value.enabled && !secret.value && !secretSet.value) { error('请先生成或填写推送密钥'); return }
  if (secret.value && !/^[\x21-\x7e]{24,256}$/.test(secret.value)) { error('推送密钥必须为 24–256 个 ASCII 字符且不包含空白'); return }
  saving.value = true
  try {
    if (secret.value) {
      if (!secretSet.value || secret.value !== savedSecret.value) {
        await adminApi.updateSystemConfig('account_push_api_key', secret.value)
        secretSet.value = true
        savedSecret.value = secret.value
      }
    }
    await adminApi.updateSystemConfig('account_push', form.value)
    success('账号推送配置已保存')
  } catch (err) { error(parseApiError(err)) }
  finally { saving.value = false }
}
async function refreshResult() {
  refreshing.value = true
  try { lastResult.value = (await adminApi.getSystemConfig('account_push_last_result')).value as PushResult | null }
  catch (err) { error(parseApiError(err)) }
  finally { refreshing.value = false }
}
watch(() => props.modelValue, open => {
  if (open) void load()
  else { ++loadSequence; secret.value = ''; revealSecret.value = false }
}, { immediate: true })
</script>
