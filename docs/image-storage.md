# 图片存储与 WebDAV

入口：管理员后台 → 系统设置 → 图片存储。

适用于 OpenAI Images 的 `/v1/images/generations` 和 `/v1/images/edits`，包括 JSON 响应和 SSE 完成事件。原生 Responses / Gemini 接口保持各自协议；SSE 预览图片仍为 Base64，完成事件使用所选返回格式。

## 存储方式

总配置项“存储方式”提供三种选择，默认“本地”。选择 WebDAV 或本地 + WebDAV 时才显示 WebDAV 配置；选择本地时隐藏该区域，已填写的连接信息和密码保留。

| 存储方式 | 新图片保存位置 | URL 返回行为 |
| --- | --- | --- |
| 本地 | 本地持久卷 | 返回本地图片 URL |
| WebDAV | 直接从内存上传，不保存本地图片或元数据 | 等待上传和公开访问校验成功后返回 WebDAV URL，失败返回存储错误 |
| 本地 + WebDAV | 本地持久卷及 WebDAV | 等待上传和校验，成功返回 WebDAV URL，失败回退本地 URL |

切换方式只影响之后的新请求，不删除已有本地图片或远程文件。原配置 `webdav_enabled=false` 兼容为本地，`true` 兼容为本地 + WebDAV；保存后使用 `storage_mode=local/webdav/local_webdav`。

## 返回规则

系统默认返回 Base64，字段为 `b64_json`。“遵循请求中的 response_format”默认关闭：关闭时忽略请求字段；开启后请求的 `url` / `b64_json` 优先，未传或 null 时回退系统配置。开启后无效格式返回 400。

| 系统格式 | 遵循请求 | 请求值 | 最终返回 |
| --- | --- | --- | --- |
| Base64 | 否 | url | b64_json |
| URL | 否 | b64_json | url |
| Base64 | 是 | url | url |
| URL | 是 | b64_json | b64_json |
| 任意 | 是 | 未传 | 系统配置 |

URL 模式选择包含 WebDAV 的存储方式时，等待上传，并验证公开地址匿名访问返回同一张图片，通过才返回 WebDAV URL。仅 WebDAV 模式失败返回存储错误；本地 + WebDAV 模式失败返回本地 URL，保留错误供重试。上传和校验合计最多 30 秒，不含等待上传并发名额的时间。

Base64 模式返回 `b64_json`。本地模式保存本地图片；本地 + WebDAV 模式保存本地后由维护任务上传；仅 WebDAV 模式在内存中发起后台上传，不创建本地图片或元数据，不等待上传完成。内存后台上传并发受限，繁忙时会等待上传名额。仅 WebDAV 模式的后台失败记录在服务日志，没有本地副本供重启后重试或补传；Base64 返回不受上传失败影响。

本地磁盘不可写时 Base64 仍可返回内容；URL 模式若 WebDAV 可用则直接上传返回远程地址，否则返回图片存储错误。存储失败不会再次向上游生图。上游生成和计费可能已经完成，调用者不应无限重试。

## 本地地址与持久化

Compose 新增命名卷 `image_data:/data/images`，环境变量 `AETHER_IMAGE_STORAGE_DIR` 指定目录，直接启动默认 `data/images`。容器重建保留卷即可保留图片及索引，`docker compose down -v` 会删除卷。

- `objects/YYYY/MM/DD/<随机ID>.<真实格式>`：图片文件。
- `metadata/<随机ID>.json`：元数据和同步状态。
- `cleanup.json`：上次清理结果。

本地公开站点填写 `https://img.example.com`，返回 `https://img.example.com/media/images/YYYY/MM/DD/…png`。留空时优先 `AETHER_PUBLIC_BASE_URL`，再按请求 Host 和可信反代协议构造。域名须解析到服务器并反代 `/media/images/`，配置项不会自动创建 DNS 或证书。

```nginx
location /media/images/ {
    proxy_pass http://127.0.0.1:8084;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
}
```

图片支持 GET、HEAD、Range；不提供目录列表或元数据访问。URL 可公开访问，随机名称不能替代访问控制。统计和维护针对当前节点，多实例请使用独立卷及能定位对应节点的公开地址，或通过 WebDAV 统一公开访问。多个进程不能同时写同一图片卷。

## WebDAV 映射

先选择 WebDAV 或本地 + WebDAV，再配置 URL、用户名、密码、根路径、公开前缀。密码独立加密保存，后台只显示是否配置；留空保留原密码，可明确勾选清除。

- URL：`http://host:8088/dav`。
- 根路径：`chatgpt2api/images`。
- 公开前缀：`https://img.example.com/images`。
- 上传：`http://host:8088/dav/chatgpt2api/images/2026/10/13/<ID>.png`。
- 返回：`https://img.example.com/images/2026/10/13/<ID>.png`。

公开前缀须实际映射到这个 WebDAV 根目录，访问者无需登录。系统逐层 MKCOL，PUT 上传，再匿名下载校验大小及 SHA256；不把密码放进 URL。WebDAV 不走系统代理，不携带凭据跟随重定向。公开地址存在延迟时会记录失败后重试。

保存后点击“测试 WebDAV”，分别显示上传、匿名访问及内容校验、删除测试文件结果。只删除本次随机测试图片；删除权限不足时可能需要手动移除测试文件。

“补传已有本地图片”同步本节点未成功上传、或目的地变更的图片，显示进度和最近错误。仅 WebDAV 模式也可以补传切换前保留的本地图片；新图片直接上传，不计入本地资源统计和补传队列。后台每分钟检查，按退避间隔重试，同一目的地最多自动尝试 3 次，手动补传可再尝试。重启后未完成图片通过元数据重新入队；手动批次进度不跨重启保留。Base64 返回不受后台上传失败影响。

## 占用与清理

显示图片数量、图片字节数、所在磁盘可用容量和整体已用比例、同步数量、待同步、失败、过期待处理、最近失败详情、上次及下次清理时间。图片字节数不含元数据和文件系统额外占用。

定时清理默认关闭，默认保留 168 小时、每隔 6 小时执行，可分别配置整数小时。维护任务每分钟检查时间，到期才执行。只删除过期本地图片和元数据，不删除远程文件。本地 URL 会失效，保留时间应覆盖客户端使用周期。

自动清理删除所有超过保留时间的本地图片，包括待同步和同步失败的图片，不等待补传成功。只保护正在读取或上传的图片，本次操作结束后仍过期的图片可在后续清理中删除。刚生成的图片未超过保留时间，不会因短暂处于待同步状态被删除。远程文件不删除；过期待同步图片删除后无法再补传。

手动清理使用已保存的保留时间，即使定时关闭也可执行；保留原有复选项，明确勾选“也删除过期待同步或同步失败的本地图片”才在手动清理中删除这些待处理图片。切换为仅 WebDAV 后，清理仍可用于旧的本地图片。

首次清理按服务启动时间加间隔计算，有历史记录则从上次清理开始计算。修改间隔可能让下一次维护检查立即执行。刷新统计使用缓存索引，应通过系统维护目录，运行时不要手工增删文件。

## 部署

重建 app：`docker compose up -d --build app`。检查卷和目录权限，后台保存配置，测试 WebDAV 并生成图片验证。切换配置影响之后的新请求。

后台接口均受现有管理员权限保护：

- `GET /api/admin/system/images/stats`。
- `POST /api/admin/system/images/webdav-test`。
- `POST /api/admin/system/images/sync`。
- `POST /api/admin/system/images/cleanup`，JSON `{"include_failed":false}`。

单张图片最多 64 MiB，单个 JSON/SSE 事件最多 256 MiB，单次 JSON 最多 64 张。此功能不处理视频任务或旧任务表 Base64 数据迁移，不自动清理远程图片。
