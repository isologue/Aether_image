<template>
  <div class="space-y-5">
    <Tabs v-model="tab">
      <TabsList>
        <TabsTrigger
          v-for="item in tabs"
          :key="item.id"
          :value="item.id"
        >
          {{ item.label }}
        </TabsTrigger>
      </TabsList>
    </Tabs>
    <KeepAlive>
      <SystemTasks v-if="tab === 'system'" />
      <BusinessTasks
        v-else
        :key="tab"
        :kind="tab"
      />
    </KeepAlive>
  </div>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui'
import SystemTasks from './SystemTasks.vue'
import BusinessTasks from './async-tasks/BusinessTasks.vue'
const tab = ref<'image' | 'video' | 'system'>('image')
const tabs = [{ id: 'image' as const, label: '图片任务' }, { id: 'video' as const, label: '视频任务' }, { id: 'system' as const, label: '系统任务' }]
</script>
