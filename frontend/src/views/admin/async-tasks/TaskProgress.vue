<template>
  <div class="min-w-0 space-y-1.5">
    <div class="flex items-center justify-between gap-3 text-xs">
      <span
        class="truncate text-muted-foreground"
        :title="label"
      >{{ label || '等待进度更新' }}</span>
      <span
        class="shrink-0 font-medium tabular-nums"
        :class="failed ? 'text-destructive' : 'text-foreground'"
      >
        {{ percent === null ? '—' : `${percent}%` }}
      </span>
    </div>
    <div
      role="progressbar"
      :aria-label="label || '任务进度'"
      :aria-valuenow="percent ?? undefined"
      :aria-valuetext="percent === null ? '进度未知' : `${percent}%`"
      aria-valuemin="0"
      aria-valuemax="100"
      class="h-1.5 overflow-hidden rounded-full bg-muted ring-1 ring-inset ring-border/30"
    >
      <div
        class="h-full rounded-full transition-[width] duration-500 ease-out motion-reduce:transition-none"
        :class="failed ? 'bg-destructive/70' : percent === null ? 'bg-primary/30' : 'bg-primary'"
        :style="{ width: percent === null ? '100%' : `${percent}%` }"
      />
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{
  value?: number | null
  label?: string
  failed?: boolean
}>()
const percent = computed(() => props.value == null || !Number.isFinite(props.value)
  ? null
  : Math.round(Math.min(100, Math.max(0, props.value))))
</script>
