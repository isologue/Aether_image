# ChatGPT Web 图片失败诊断

`503 已尝试所有本地执行候选提供商，但没有任何候选成功完成请求` 是本地候选全部失败后的汇总结果，不能据此断定上游实际返回了 HTTP 503。

## 部署后取证

从失败的使用记录详情中复制 `request_id`，在服务器 Aether 目录执行：

```bash
docker compose logs --since 1h --no-color app | rg -F '替换为完整request_id'
```

如果服务器没有 `rg`，可将最后一段替换为 `grep -F '替换为完整request_id'`。日志已经轮转时，需要保留当时的日志或扩大查询时间范围；新诊断不能还原升级前已经丢失的上游错误内容。

每个失败尝试的 `chatgpt_web_image_attempt_failed` 事件包含：

| 字段 | 用途 |
| --- | --- |
| `request_id` / `candidate_id` / `key_id` | 关联同一请求、候选尝试和账号 |
| `stage` / `stage_elapsed_ms` / `elapsed_ms` | 确认失败阶段、该阶段耗时和本次尝试耗时 |
| `error_kind` | 区分 HTTP 失败、请求发送失败、响应读取失败、响应过大和响应解码失败 |
| `upstream_status_code` | 该阶段实际获得的 HTTP 状态码；未获得状态码时为空 |
| `upstream_error_type` / `upstream_error_code` / `upstream_error_param` | 上游结构化错误类别、代码和相关参数 |
| `upstream_error_message` | 有长度限制、脱敏的上游文字摘要 |
| `has_conversation_id` / `turn_finished` / `has_failure` | 是否获得任务会话、是否识别终态、是否提取到任务失败 |
| `asset_count` / `poll_count` / `downloaded_count` | 已提取资源数量、补查次数和已下载图片数量 |

首次响应读取阶段另有 `chatgpt_web_image_initial_response_diagnostics`，保留响应读取及 SSE 解析证据。超时或取消中断有 `chatgpt_web_image_attempt_interrupted`，应一起检查，不能只筛选 `attempt_failed`。

请求级使用记录元数据保留 `first_attempt_error` 和 `last_attempt_error`。它们保存首个和最后一个候选失败摘要，方便避免只看到最终的通用 503；完整的逐次阶段证据仍以运行日志和候选详情为准。

## 判断方向

- 有 `prompt_too_long`、`invalid_request_error` 等错误信息：结合 `param` 检查请求参数和上游能力限制。
- HTTP 401 / 403：结合具体错误代码判断认证、权限或访问限制。
- HTTP 429：结合具体错误代码区分限流、额度或其他上游限制。
- HTTP 502 / 503：结合阶段和错误摘要检查上游服务及代理链路。
- `upstream_request`：检查错误链中连接、超时、TLS、代理等细节；该类型本身不足以认定具体网络故障。
- `body_read` / `body_limit` / `response_decode`：分别检查响应读取、大小限制或压缩解码问题。
- 没有结构化上游错误摘要：上游可能没有返回可提取的 JSON 错误，仍需结合实际状态码、阶段和读取日志判断。

## 记录范围

HTTP 错误正文只提取 `error.type`、`error.code`、`error.param`、`error.message`；兼容常见字符串 `error` 和顶层 `message` / `detail`。Base64 包装的错误正文最多解码 16 KiB，非 JSON 或超过限制时不提取正文摘要。不会原样持久化完整上游错误正文。

每个提取字段沿用有界错误摘要规则，并移除常见凭据、URL、邮箱和长编码串。摘要不能保证保留上游所有细节；诊断字段缺失不等于上游未报错。

本次诊断在本地模拟上游错误验证。线上是否能复现原来的 503、原始失败究竟发生在哪一阶段，需要部署本次代码后按真实失败请求确认。
