# ChatGPT2API 模式账号推送

Aether 可以接收外部程序定时推送的 ChatGPT Web / Codex 账号。发送周期在外部程序中配置。

## 后台配置

1. 在「号池管理」选择提供商，点击「账号推送」。
2. 选择目标 ChatGPT Web 或 Codex 提供商。
3. 生成专用推送 API Key，复制到发送程序，勾选启用并保存。
4. 在发送程序选择 ChatGPT2API 模式，API URL 填 Aether 服务根地址，例如 `https://aether.example.com`，API Key 填刚才生成的推送密钥。

密钥加密保存；已保存的密钥不会再次显示。输入框留空会保留已有密钥。替换密钥或关闭入口后，新请求立即按新配置校验。

## 接收协议

兼容 `POST /api/accounts` 和 `POST /api/accounts/upload`。使用 `Authorization: Bearer <推送密钥>` 和 `Content-Type: application/json`。

```json
{
  "accounts": [
    {
      "access_token": "账号 access token",
      "refresh_token": "账号 refresh token",
      "email": "account@example.com",
      "account_id": "账号 account id"
    }
  ],
  "tokens": [],
  "refresh": true,
  "return_items": false
}
```

`accounts` 接受凭据对象，`tokens` 接受 access token 字符串。也接受账号对象数组或单个凭据对象。支持 `accessToken` / `refreshToken` / `accountId` 等原有导入字段别名。

单个 JSON 请求体最大 128 MiB，接收后由 Aether 按每批 100 个账号顺序导入；发送程序不需要自行拆批。账号至少提供 access token 或 refresh token；只有 refresh token 的账号必须能够通过提供商配置完成交换。请求中的代理、请求头等配置不会用于账号导入；联网操作沿用目标提供商的代理设置。Agent Identity 使用原有专属入口。

`refresh` 字段接受但不覆盖 Aether 的导入策略：携带 refresh token 时，现有导入器会尝试交换；ChatGPT Web 满足原有条件时可回退使用 access token。`return_items` 字段接受，但响应不回传账号凭据。

导入复用 Aether 原有凭据加密、账号身份匹配和去重逻辑。重复账号更新凭据；只有裸 token 且无法解析账号身份时，无法保证不同 token 属于同一账号，建议推送完整账号对象。

## 返回结果与重试

接口等待全部内部批次导入完成再返回；发送端断开后，已经开始的导入继续执行并记录结果。某个批次出现账号级失败时，Aether 会继续处理后续批次，最后合并返回全部批次的统计和错误索引。后台「最近一次推送」显示最近一次完成导入的时间与数量，不记录鉴权失败或无效 JSON。

```json
{
  "success": true,
  "total": 2,
  "added": 1,
  "updated": 1,
  "skipped": 0,
  "failed": 0,
  "refreshed": 0,
  "errors": [],
  "items": []
}
```

`added` 表示新增，`updated` 表示更新，`failed` 表示导入失败。`refreshed` 固定为 0，刷新次数不单独统计。`errors` 只包含失败位置与通用提示，不回显 token。

全部成功或部分成功返回 HTTP 200；部分失败时 `success` 为 false，需要检查 `failed` / `errors`。全部导入失败返回 400。关闭入口返回 404，密钥错误返回 401，格式错误返回 400，不支持的 Content-Type 返回 415，体积超限返回 413，同一进程已有推送正在处理返回 409。服务配置或后端不可用时可能返回 503。

超时、409 或服务暂不可用时发送端可以稍后重试。使用同一账号身份重试会更新已有账号。一次大批量导入可能触发多轮联网刷新，请给发送端、反向代理和负载均衡器设置足够的请求超时时间，并确保它们的请求体限制不低于 128 MiB。

## 兼容范围

JSON 格式依据本地 ChatGPT2API 接收端源码实现；额外兼容 `/api/accounts/upload` 路径。第三方发送程序没有源码，实际的请求体、超时和响应判定仍需用该程序推送一次确认。当前接收器的并发推送互斥位于单个网关进程；多副本网关的 ChatGPT Web 导入暂不保证跨实例串行。
