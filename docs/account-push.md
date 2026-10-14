# 账号推送（Sub2API）

外部程序选择 Sub2API 模式，将 OpenAI OAuth 账号推送到 Aether 的 ChatGPT Web 或 Codex 号池。账号凭据复用现有 OAuth 导入、加密和去重流程。

## 后台配置

1. 在「号池管理 → 账号推送」选择目标 ChatGPT Web 或 Codex 提供商。
2. 生成或填写推送 API Key，保存后密钥不再回显；留空保存会保留现有密钥。
3. 启用并保存账号推送。
4. 在发送程序选择 Sub2API 模式，API URL 填 Aether 服务根地址，例如 `https://aether.example.com`，API Key 填上述推送密钥。

配置沿用 `account_push` 和 `account_push_api_key`，无需重新生成已有密钥。关闭入口或更换密钥会在下次请求生效。

## 接口与请求

接收路径为 `/api/v1/admin/accounts/data`，接受 `GET` 和 `POST`。请求使用 `X-API-Key: <推送密钥>`；也接受 `Authorization: Bearer <推送密钥>`。

GET 用于发送端连接检查，返回 Sub2API 信封及空账号列表，不导出 Aether 已保存的账号或凭据。

POST 使用 `Content-Type: application/json`，接受包裹 `data` 的备份对象或直接备份对象：

```json
{
  "data": {
    "type": "sub2api-data",
    "version": 1,
    "proxies": [],
    "accounts": [
      {
        "name": "pool-account",
        "platform": "openai",
        "type": "oauth",
        "credentials": {
          "access_token": "账号 access token",
          "refresh_token": "账号 refresh token"
        },
        "extra": { "email": "account@example.com" }
      }
    ]
  }
}
```

数据 type 支持 sub2api-data / sub2api-bundle 或省略，version 支持 0 / 1 或省略。只导入 platform=openai 且 type=oauth 的账号；空包、缺少凭据、不支持的账号类型及 Agent Identity 凭据会明确返回错误。每个账号至少包含 access token 或 refresh token；仅含 refresh token 的账号需要目标提供商能够完成凭据交换。

账号名称、credentials 和身份元数据会进入现有导入器。代理、请求头、分组和调度参数不会从推送包导入；联网刷新沿用目标提供商配置。重复账号按已有 OAuth 身份规则更新凭据，建议推送完整身份信息。

单次 JSON 请求体上限 128 MiB，Aether 内部按每批 100 个账号顺序处理，发送端无需拆批。单个网关进程同一时间只执行一个导入请求，忙时返回 409。

## 返回结果与重试

接口等待全部内部批次处理完成后返回汇总；发送端断开后，已经开始的导入继续执行并记录结果。账号级失败不会中断后续批次。后台「最近一次推送」保留完成时间及新增、更新、失败统计，不记录鉴权失败或无效 JSON。

```json
{
  "code": 0,
  "message": "success",
  "data": {
    "proxy_created": 0,
    "proxy_reused": 0,
    "proxy_failed": 0,
    "account_created": 1,
    "account_updated": 1,
    "account_failed": 0,
    "errors": []
  }
}
```

account_created 表示新增，account_updated 表示更新，account_failed 表示失败；errors 包含 kind、从 0 开始的账号 index 和 message，不回显凭据。代理统计固定为 0。

全部成功或部分成功返回 HTTP 200 / code=0，部分成功仍须检查 account_failed；全部导入失败返回 HTTP 400，并包含失败统计。关闭入口返回 404，密钥错误返回 401，格式错误返回 400，Content-Type 不支持返回 415，体积超限返回 413，导入忙返回 409，服务不可用返回 503。

超时、409 或服务暂不可用时可以稍后重试，同一账号身份会更新已有账号。大批量导入可能触发多轮联网刷新，请为发送端和反向代理设置足够的超时时间，代理请求体限制应不低于 128 MiB。

并发互斥位于单个网关进程，多副本网关的 ChatGPT Web 导入暂不保证跨实例串行。
