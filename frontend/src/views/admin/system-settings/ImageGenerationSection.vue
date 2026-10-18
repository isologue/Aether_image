<template>
  <CardSection
    title="图片生成"
    description="控制生图过程中的等待和失败处理"
  >
    <template #actions>
      <Button
        size="sm"
        :disabled="!loaded || saving || seconds === savedSeconds || !valid"
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
      class="max-w-xl space-y-2"
    >
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
        v-if="!valid"
        role="alert"
        class="text-sm text-destructive"
      >
        请输入 0–120 的整数。
      </p>
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
const { success, error } = useToast()
const seconds = ref<string | number>(20)
const savedSeconds = ref<string | number>(20)
const loaded = ref(false)
const saving = ref(false)
const loadError = ref('')
const valid = computed(() => String(seconds.value).trim() !== ''
  && Number.isInteger(Number(seconds.value))
  && Number(seconds.value) >= 0 && Number(seconds.value) <= 120)

async function load() {
  loaded.value = false
  loadError.value = ''
  try {
    const result = await adminApi.getSystemConfig(CONFIG_KEY)
    seconds.value = result.value == null ? 20 : result.value as number
    savedSeconds.value = seconds.value
    loaded.value = true
  } catch {
    loadError.value = '图片生成配置加载失败，请重试。'
  }
}

async function save() {
  if (!loaded.value || saving.value || !valid.value) return
  saving.value = true
  const value = Number(seconds.value)
  try {
    await adminApi.updateSystemConfig(CONFIG_KEY, value)
    seconds.value = value
    savedSeconds.value = value
    success('图片生成配置已保存，无需重启')
  } catch {
    error('图片生成配置保存失败，请重试')
  } finally {
    saving.value = false
  }
}

onMounted(load)
</script>
