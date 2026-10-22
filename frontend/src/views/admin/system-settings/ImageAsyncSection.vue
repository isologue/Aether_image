<template>
  <CardSection
    title="图片异步任务"
    description="通过 async: true 受理后台任务；配置保存后即时生效。"
  >
    <template #actions>
      <Button
        :disabled="loading || saving || !dirty"
        @click="save"
      >
        {{ saving ? '保存中…' : '保存' }}
      </Button>
    </template>
    <div class="grid gap-6 md:grid-cols-3">
      <div>
        <Label for="image-concurrency">全部署执行并发</Label><Input
          id="image-concurrency"
          v-model.number="config.concurrency"
          type="number"
          min="1"
          max="10000"
        /><p class="text-xs text-muted-foreground">
          默认 200，仍遵守号池及单账号限制。
        </p>
      </div>
      <div>
        <Label for="image-queued">最大排队数量</Label><Input
          id="image-queued"
          v-model.number="config.max_queued"
          type="number"
          min="1"
          max="1000000"
        />
      </div>
      <div>
        <Label for="image-wait">最大排队时间（秒）</Label><Input
          id="image-wait"
          v-model.number="config.max_queue_seconds"
          type="number"
          min="1"
          max="604800"
        />
      </div>
    </div>
    <p
      v-if="error"
      class="mt-3 text-sm text-destructive"
    >
      {{ error }}
    </p>
  </CardSection>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import Button from '@/components/ui/button.vue'
import Input from '@/components/ui/input.vue'
import Label from '@/components/ui/label.vue'
import { CardSection } from '@/components/layout'
import { adminApi } from '@/api/admin'
import { useToast } from '@/composables/useToast'
const config=ref({concurrency:200,max_queued:10000,max_queue_seconds:3600}), original=ref(''), loading=ref(true),saving=ref(false),error=ref('')
const dirty=computed(()=>JSON.stringify(config.value)!==original.value)
const toast=useToast()
onMounted(async()=>{try{const r=await adminApi.getSystemConfig('image_async_tasks');config.value={...config.value,...(r.value as Partial<typeof config.value>)};original.value=JSON.stringify(config.value)}catch{error.value='读取异步任务配置失败'}finally{loading.value=false}})
async function save(){
  const c=config.value
  if(![c.concurrency,c.max_queued,c.max_queue_seconds].every(Number.isInteger)||c.concurrency<1||c.concurrency>10000||c.max_queued<1||c.max_queued>1000000||c.max_queue_seconds<1||c.max_queue_seconds>604800){error.value='请输入范围内的正整数';return}
  saving.value=true;error.value=''
  try{await adminApi.updateSystemConfig('image_async_tasks',c);original.value=JSON.stringify(c);toast.success('异步任务配置已保存')}catch(e){error.value=String(e)}finally{saving.value=false}
}
</script>


