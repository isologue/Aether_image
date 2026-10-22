# 异步生图与任务管理

## 接口

`POST /v1/images/generations` 和 `POST /v1/images/edits` 支持 JSON 布尔字段 `async: true`；multipart 使用文本字段 `async=true`。缺省和 `false` 继续同步执行。与 `stream: true` 同传返回 400，调度字段不会传给上游。

```sh
curl "$AETHER_URL/v1/images/generations" \
  -H "Authorization: Bearer $AETHER_API_KEY" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: example-order-001' \
  -d '{"model":"gpt-image-1","prompt":"一只猫","async":true}'

curl "$AETHER_URL/v1/images/tasks/$TASK_ID" \
  -H "Authorization: Bearer $AETHER_API_KEY"
```

受理返回 HTTP 202，以及 `task_id/status/progress/stage/created_at/poll_after_seconds`。查询按 API Key 隔离，已有记录先校验归属。记录或所需图片不存在返回 HTTP 410、`image_cleaned` 和“图片已被清理”；存储权限、临时网络故障与资源不存在分别处理。

可选的 `Idempotency-Key` 以 API Key 为作用域。同键同内容返回原任务，不同内容返回 409；记录清理后不再保证去重。URL 比较原始地址，文件与 Base64 比较解码后的内容摘要及必要元数据，multipart boundary、文件名不参与去重。

## 输入与结果

URL 输入只保存原始 URL，包括签名参数；受理时不下载，不持久化副本。URL 的有效性由调用方保证。Base64 和上传文件保存到图片持久卷的 `task-inputs/{task_id}`，数据库只保存引用。终态并完成结算后回收输入；现有维护流程回收超过 600 秒且无活跃任务引用的孤立目录。图片存储统计单独列出任务输入占用。

JSON 蒙版编辑使用 `POST /v1/images/edits`，兼容 `images[].image_url` 和 `mask.image_url` 对象形式，例如：

```json
{
  "model": "gpt-image-2",
  "n": 1,
  "images": [{"image_url": "https://example.com/source.png"}],
  "mask": {"image_url": "https://example.com/mask.png"},
  "prompt": "只修改蒙版透明区域，其他区域保持不变。移除选中区域文字",
  "size": "1024x1024",
  "output_format": "png",
  "response_format": "url",
  "async": true
}
```

不传 `async` 时使用同步流程。原图和蒙版保持独立角色，URL 查询参数在排队、恢复和幂等比较中完整保留，不因为是对象形式而下载或落盘。蒙版建议使用与原图同尺寸的 PNG，透明区域表示待编辑区域；实际支持程度取决于上游通道。请求中的 `response_format` 是否生效仍遵守图片存储的“遵循客户端 response_format”设置。上述输入格式兼容不代表 ChatGPT Web 原生蒙版已经支持，见下方能力边界。

任务受理时冻结最终返回格式、缩放开关和目标尺寸。执行沿用同步的选号、切号、图片处理、Sharp/Lanczos3 缩放和存储流程。进度依次为排队 0、准备 10、生成 20、下载 70、处理 90、完成 100；结果未准备好不会提前成功。

异步结果保存可重读的图片引用。查询 Base64 时读取已保存的图片，不向任务表重复写入完整 Base64，也不重复缩放。本地、WebDAV、本地＋WebDAV 采用现有存储配置；组合模式优先使用仍可用的副本。跨机器使用本地卷时按持久化存储节点归属执行，不假设同名 Docker 卷共享文件。

## 调度与恢复

系统设置中的 `image_async_tasks` 默认值：

```json
{"concurrency":200,"max_queued":10000,"max_queue_seconds":3600}
```

配置可热更新。PostgreSQL 事务领取与全局锁限制整个部署的执行并发；排队不提前占用账号，执行仍遵守号池和单账号限制。租约 120 秒，每 20 秒续租；持续失联后停止执行，每个任务最多执行 30 分钟。

重启后排队任务继续执行。ChatGPT Web 已保存轮询检查点时恢复查询；没有可靠检查点的执行中任务明确失败为 `execution_interrupted`，不会盲目重复提交。初始 SSE 尚未提供可恢复标识时，同样适用这一边界。

## 计费与清理

图片完成处理和必要保存后才发出最终使用记录。终态事件和任务状态一并持久化，通过加密 outbox 重放完成结算，完成前不删除恢复信息和输入。内容拒绝成功结束但费用为零；失败释放预占；异常已实扣失败按原扣除来源幂等退款，并保留原使用记录及关联钱包流水。

`async_task_retention_days` 默认 30 天，统一管理图片、视频业务任务，复用记录清理调度。只清理已完成结算的终态，不顺带删除图片或使用记录。开启图片按时长自动清理时，天数不得小于 `max(1, ceil(retention_hours / 24))`。前端、后端配置入口和数据库约束校验最终组合，支持同时保存图片与记录策略。

后台“异步任务”包含图片、视频、系统三个标签页，各自保留分页和筛选。图片详情展示阶段、耗时、切号、结果和关联使用记录；普通同步请求不新增异步任务行。

## 当前能力边界

已按要求移除 ChatGPT Web 对蒙版输入的本地拒绝判断，请求可继续执行。此次仅取消拦截，当前 Web 请求构建尚未把蒙版映射到上游消息，不能据此认定局部编辑会遵循蒙版。OpenAI 图片编辑通道继续保留蒙版角色并传递。方案中的 ChatGPT Web 蒙版验收项尚未完成。

2026-10-22 再次核对：本地独立 Chrome 访问 ChatGPT 首页返回 `ERR_CONNECTION_CLOSED`；已授权服务器请求返回 HTTP 403，未取得页面脚本；通过服务器转发的独立浏览器导航超时。这些结果仅说明当前未取得真实协议，不能证明上游不支持蒙版。参考项目 `chatgpt2api_newui` 使用透明通道合成，不能作为原生蒙版协议依据。完成此项仍需一次真实的“选择区域 → 编辑 → 发送”请求、相关上传请求及编辑前后结果对照；共享请求时移除 Authorization、Cookie 等凭据，保留蒙版与原图的引用关系和消息元数据。

受理阶段对套餐请求额度使用只读检查，不写入或延长 PostgreSQL 准入记录，也不修改 Redis 滑动窗口或占用执行并发。执行前重新检查并按现有机制预占；结算和失败释放由持久化任务及使用记录对账维护。

## 验证记录

- 图片相关 Rust 回归：185 项通过，3 项需要外部环境的测试跳过。
- 套餐用量策略回归：17 项通过；前端用量展示相关回归：199 项通过。
- 隔离 PostgreSQL 集成测试通过：并发幂等、全局执行上限、租约隔离、检查点恢复、配置约束、排队超时和按原来源幂等退款。
- 隔离 HTTP 联调通过：任务生命周期、API Key 隔离、重复提交及 409、稳定结果、失败与内容拒绝免费、同步兼容、缩放快照、清理后的 410、multipart 编辑与蒙版角色、重启恢复和中断不重新提交。
- HTTP 联调使用可控的 OpenAI 图片模拟上游，不能据此认定 ChatGPT Web 原生蒙版已通过验收。
- 前端类型检查、前端生产构建和数据库 schema 检查通过。
- 受理阶段只读额度回归通过：套餐策略 17 项、内存准入记录检查、PostgreSQL 实库窗口检查、Redis 实例滑动窗口及 TTL 检查。
- 异步输入回归 5 项通过，包含 JSON `images[].image_url` 与 `mask.image_url` 在保存、恢复和幂等比较中保持 URL 与角色，且纯 URL 请求不创建输入目录。

在配置好 Rust/Linux 构建环境后，可重跑以下检查。数据库测试会创建并删除临时数据库，`AETHER_TEST_DATABASE_URL` 需要具备相应权限；Redis 测试使用独立随机键前缀。

```sh
cargo test --locked -p aether-gateway image --lib
cargo test --locked -p aether-gateway plan_usage_policy::tests --lib
cargo test --locked -p aether-data queue_probe_does_not_write_admissions_or_tombstones --lib
cargo test --locked -p aether-runtime-state runtime_backends_share_atomic_sliding_usage_limit_contract --lib

# 环境变量使用专门的测试数据库与 Redis，不把连接凭据写进命令历史。
cargo test --locked -p aether-data-postgres live_image_queue_policy_leases_and_refunds -- --ignored --nocapture
cargo test --locked -p aether-data-postgres live_usage_policy_window_aggregates_preserve_exact_admission_and_idempotency -- --ignored --nocapture
cargo test --locked -p aether-runtime-state live_usage_limit_snapshot_is_read_only --lib -- --ignored --nocapture
```

前端检查在 `frontend` 目录执行 `npm run type-check`、`npm run test:run -- src/features/usage` 和 `npm run build`。

2026-10-22 已执行 `docker compose up -d --build`，最终 release 镜像构建成功，保留原有优化配置。本地服务 `http://localhost:8084` 的健康检查、管理员登录、图片/视频任务接口、系统配置接口和前端资源检查通过，数据库迁移 `20261022120000` 成功。使用同一份最终镜像再次完成上述隔离 HTTP 联调，全部通过；未部署到线上。
