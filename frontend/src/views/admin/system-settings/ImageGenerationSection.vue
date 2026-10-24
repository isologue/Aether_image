<template>
  <CardSection
    title="图片生成"
    description="控制单次图片数量、等待和失败处理"
  >
    <template #actions>
      <Button
        size="sm"
        :disabled="!loaded || saving || !dirty || !valid"
        @click="save"
      >
        {{ saving ? '保存中...' : '保存' }}
      </Button>
    </template>
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
    <div
      v-else-if="loaded"
      class="max-w-xl space-y-6"
    >
      <div class="space-y-2">
        <Label for="image-max-count">ChatGPT Web 单次请求图片数量上限（n）</Label>
        <Input
          id="image-max-count"
          v-model="maxCount"
          type="number"
          min="1"
          max="10"
          step="1"
          :disabled="saving"
          aria-describedby="image-max-count-help"
        />
        <p
          id="image-max-count-help"
          class="text-xs text-muted-foreground"
        >
          默认 1，可设置 1–10。此项限制客户端 n 的最大值；客户端未传 n 时仍生成 1 张。
          调高后会将请求数量传给上游，供测试多图支持，实际返回数量以上游为准。
          保存后对新请求生效，无需重启。
        </p>
        <p
          v-if="!countValid"
          role="alert"
          class="text-sm text-destructive"
        >
          请输入 1–10 的整数。
        </p>
      </div>
      <div class="space-y-2">
        <Label for="image-max-prompt-chars">ChatGPT Web 提示词本地长度限制（字符）</Label>
        <Input
          id="image-max-prompt-chars"
          v-model="maxPromptChars"
          type="number"
          min="0"
          max="1000000"
          step="1"
          :disabled="saving"
          aria-describedby="image-max-prompt-chars-help"
        />
        <p
          id="image-max-prompt-chars-help"
          class="text-xs text-muted-foreground"
        >
          默认 32768 个字符，可设置 0–1000000。设为 0 表示不做本地提示词长度限制，超长提示词会继续发送给上游，由上游返回实际错误。这里按 Unicode 字符计数，不等同于 UTF-8 字节数；保存后对新请求立即生效，无需重启。
        </p>
        <p
          v-if="!promptValid"
          role="alert"
          class="text-sm text-destructive"
        >
          请输入 0–1000000 的整数，0 表示不限制。
        </p>
      </div>
      <div class="space-y-2">
        <Label for="image-no-asset-grace">ChatGPT Web 无图片补查等待时间（秒）</Label>
        <Input
          id="image-no-asset-grace"
          v-model="seconds"
          type="number"
          min="0"
          max="120"
          step="1"
          :disabled="saving"
          aria-describedby="image-no-asset-grace-help"
        />
        <p
          id="image-no-asset-grace-help"
          class="text-xs text-muted-foreground"
        >
          默认 20 秒，可设置 0–120 秒。上游响应结束后仍无图片引用时，在此时间内补查；到期仍无图则按切号策略尝试下一个账号。
          设为 0 跳过补查。此项不限制整个生图请求耗时，已有图片引用时继续下载。
          保存后对后续进入补查阶段的账号尝试生效，无需重启；正在补查的尝试沿用原值。
        </p>
        <p
          v-if="!graceValid"
          role="alert"
          class="text-sm text-destructive"
        >
          请输入 0–120 的整数。
        </p>
      </div>
    </div>
    <p
      v-else
      class="text-sm text-muted-foreground"
    >
      配置加载中...
    </p>
  </CardSection>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { adminApi } from '@/api/admin'
import { CardSection } from '@/components/layout'
import Button from '@/components/ui/button.vue'
import Input from '@/components/ui/input.vue'
import Label from '@/components/ui/label.vue'
import { useToast } from '@/composables/useToast'

const CONFIG_KEY = 'chatgpt_web_image_no_asset_grace_seconds'
const MAX_COUNT_KEY = 'chatgpt_web_image_max_count'
const MAX_PROMPT_CHARS_KEY = 'chatgpt_web_image_max_prompt_chars'
const { success, error } = useToast()
const seconds = ref<string | number>(20)
const savedSeconds = ref<string | number>(20)
const maxCount = ref<string | number>(1)
const savedMaxCount = ref<string | number>(1)
const maxPromptChars = ref<string | number>(32768)
const savedMaxPromptChars = ref<string | number>(32768)
const loaded = ref(false)
const saving = ref(false)
const loadError = ref('')
const graceValid = computed(() => String(seconds.value).trim() !== ''
  && Number.isInteger(Number(seconds.value))
  && Number(seconds.value) >= 0 && Number(seconds.value) <= 120)

const countValid = computed(() => String(maxCount.value).trim() !== ''
  && Number.isInteger(Number(maxCount.value))
  && Number(maxCount.value) >= 1 && Number(maxCount.value) <= 10)
const promptValid = computed(() => String(maxPromptChars.value).trim() !== ''
  && Number.isInteger(Number(maxPromptChars.value))
  && Number(maxPromptChars.value) >= 0 && Number(maxPromptChars.value) <= 1000000)
const valid = computed(() => graceValid.value && countValid.value && promptValid.value)
const dirty = computed(() => Number(seconds.value) !== Number(savedSeconds.value)
  || Number(maxCount.value) !== Number(savedMaxCount.value)
  || Number(maxPromptChars.value) !== Number(savedMaxPromptChars.value))

async function load() {
  loaded.value = false
  loadError.value = ''
  try {
    const [grace, count, prompt] = await Promise.all([
      adminApi.getSystemConfig(CONFIG_KEY),
      adminApi.getSystemConfig(MAX_COUNT_KEY),
      adminApi.getSystemConfig(MAX_PROMPT_CHARS_KEY),
    ])
    seconds.value = grace.value == null ? 20 : grace.value as number
    savedSeconds.value = seconds.value
    maxCount.value = count.value == null ? 1 : count.value as number
    savedMaxCount.value = maxCount.value
    maxPromptChars.value = prompt.value == null ? 32768 : prompt.value as number
    savedMaxPromptChars.value = maxPromptChars.value
    loaded.value = true
  } catch {
    loadError.value = '图片生成配置加载失败，请重试。'
  }
}

async function save() {
  if (!loaded.value || saving.value || !valid.value || !dirty.value) return
  saving.value = true
  const updates = [
    { key: CONFIG_KEY, current: seconds, saved: savedSeconds },
    { key: MAX_COUNT_KEY, current: maxCount, saved: savedMaxCount },
    { key: MAX_PROMPT_CHARS_KEY, current: maxPromptChars, saved: savedMaxPromptChars },
  ].filter(item => Number(item.current.value) !== Number(item.saved.value))
  try {
    const results = await Promise.allSettled(updates.map(async item => {
      const value = Number(item.current.value)
      await adminApi.updateSystemConfig(item.key, value)
      item.current.value = value
      item.saved.value = value
    }))
    if (results.some(result => result.status === 'rejected')) {
      error('部分图片生成配置保存失败，请重试；已保存的配置已生效')
    } else {
      success('图片生成配置已保存，无需重启')
    }
  } finally {
    saving.value = false
  }
}

onMounted(load)
</script>
