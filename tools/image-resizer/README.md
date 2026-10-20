# Sharp / Lanczos3 图片缩放

系统设置 → 图片存储 → 按请求分辨率自动缩放，默认关闭。保存后对新请求生效。

开启时，`/v1/images/generations` 和 `/v1/images/edits` 根据 JSON 或 multipart 请求中的 `size=宽x高`，在存储、WebDAV 上传、Base64 / URL 返回和 SSE 完成事件之前统一处理。`size` 未指定或为 `auto` 时保留原始图片。相同尺寸不重新编码；比例不同则等比缩放并居中补边（PNG/WebP 透明，JPEG 白色）。不修改上游计费数据。

ChatGPT Web 通道会先使用最接近目标比例的原生 1K 尺寸生成，再缩放到指定 `size`，避免上游的分辨率限制。其他提供商继续使用原请求尺寸。仅传 `resolution` / `size_tier` 不会指定本地缩放的目标像素，需要同时传入 `size=宽x高`。ChatGPT Web 的本地参数校验失败会返回 400，不会遍历号池重试。

支持静态 PNG、JPEG、WebP，保留原格式。输入与输出最多 64 MiB，单边最多 8192，总像素最多 32,000,000。每个网关实例最多并行处理两张图片，单张处理超时为 60 秒。处理失败会明确返回错误，不会把原始尺寸的图片作为缩放成功返回。

Docker 构建会自动安装和打包 Node.js、Sharp 及其原生依赖，不需要额外服务。源码运行时先在本目录执行 `npm ci`，并确保 PATH 中存在 Node.js；可用 `AETHER_IMAGE_RESIZER_NODE` 和 `AETHER_IMAGE_RESIZER_SCRIPT` 指定可执行文件和脚本路径。

验证：本目录执行 `npm test`。网关测试：`cargo test -p aether-gateway --lib image_storage`；包含实际缩放的测试需要上述组件可用。
