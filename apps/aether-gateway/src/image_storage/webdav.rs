use super::{
    config::{validate_http_url, ImageConfig},
    storage::{now, ImageRecord, ImageStorage, MAX_IMAGE_BYTES},
};
use futures_util::StreamExt;

pub(crate) fn joined_url(base: &str, path: &str) -> Result<url::Url, String> {
    let mut url = validate_http_url(base)?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| "Invalid image URL".to_string())?;
        segments.pop_if_empty();
        for part in path.split('/').filter(|s| !s.is_empty()) {
            segments.push(part);
        }
    }
    Ok(url)
}
impl ImageStorage {
    async fn dav_directories(
        &self,
        config: &ImageConfig,
        password: &str,
        relative: &str,
    ) -> Result<url::Url, String> {
        let full_path = [config.webdav_root.trim_matches('/'), relative]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let components: Vec<_> = full_path.split('/').collect();
        for count in 1..components.len() {
            let mut dir = joined_url(&config.webdav_url, &components[..count].join("/"))?;
            dir.path_segments_mut()
                .map_err(|_| "Invalid WebDAV path".to_string())?
                .push("");
            let response = self
                .dav_client
                .request(reqwest::Method::from_bytes(b"MKCOL").expect("MKCOL"), dir)
                .basic_auth(&config.webdav_username, Some(password))
                .send()
                .await
                .map_err(|_| "WebDAV 创建目录连接失败或超时".to_string())?;
            if !matches!(response.status().as_u16(), 201 | 405) {
                return Err(format!(
                    "WebDAV 创建目录失败：HTTP {}",
                    response.status().as_u16()
                ));
            }
        }
        joined_url(&config.webdav_url, &full_path)
    }
    pub async fn download(&self, url: &str) -> Result<Vec<u8>, String> {
        // Provider URLs may legitimately include signed query parameters.
        let parsed = url::Url::parse(url).map_err(|_| "Invalid provider image URL".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err("Invalid provider image URL".into());
        }
        let response = self
            .public_client
            .get(parsed)
            .send()
            .await
            .map_err(|_| "图片下载连接失败或超时".to_string())?;
        if !response.status().is_success() {
            return Err(format!("图片下载失败：HTTP {}", response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_IMAGE_BYTES as u64)
        {
            return Err("图片超过 64 MiB".into());
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "读取图片下载响应失败".to_string())?;
            if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
                return Err("图片超过 64 MiB".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        super::storage::image_type(&bytes)?;
        Ok(bytes)
    }
    pub(super) async fn dav_put(
        &self,
        config: &ImageConfig,
        password: &str,
        relative: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<String, String> {
        let remote = self.dav_directories(config, password, relative).await?;
        let public = joined_url(&config.webdav_public_url, relative)?.to_string();
        let response = self
            .dav_client
            .put(remote)
            .basic_auth(&config.webdav_username, Some(password))
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes)
            .send()
            .await
            .map_err(|_| "WebDAV 上传连接失败或超时".to_string())?;
        if !response.status().is_success() {
            return Err(format!(
                "WebDAV 上传失败：HTTP {}",
                response.status().as_u16()
            ));
        }
        // A successful PUT completes storage; public/CDN readback must not delay delivery.
        // Public mapping can still be checked explicitly with test_webdav.
        Ok(public)
    }
    /// Store an image directly from memory, without creating a local image or index record.
    pub(super) async fn upload_bytes(
        &self,
        config: &ImageConfig,
        password: &str,
        bytes: Vec<u8>,
    ) -> Result<String, String> {
        let permit = self
            .upload_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "图片上传服务已停止".to_string())?;
        self.upload_bytes_with_permit(config, password, bytes, permit)
            .await
    }
    pub(super) async fn upload_bytes_with_permit(
        &self,
        config: &ImageConfig,
        password: &str,
        bytes: Vec<u8>,
        _permit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<String, String> {
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err("图片为空或超过 64 MiB".into());
        }
        let (extension, content_type) = super::storage::image_type(&bytes)?;
        let relative = format!(
            "{}/{}.{}",
            chrono::Utc::now().format("%Y/%m/%d"),
            uuid::Uuid::new_v4().simple(),
            extension
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.dav_put(config, password, &relative, bytes, content_type),
        )
        .await
        .unwrap_or_else(|_| Err("WebDAV 上传超过 30 秒".into()))
    }
    pub async fn upload(
        &self,
        config: &ImageConfig,
        password: &str,
        record: &ImageRecord,
    ) -> Result<String, String> {
        let index = self.index().await?;
        let destination = config.destination();
        let Some(_active) = self.claim(&record.id) else {
            return Err("图片正在使用，稍后重试".into());
        };
        // Queue snapshots may be stale; refresh after owning the image to avoid overwriting a completed upload.
        let record = index
            .read()
            .await
            .records
            .get(&record.id)
            .cloned()
            .ok_or("本地图片已被清理")?;
        if record.destination.as_deref() == Some(destination.as_str()) {
            if let Some(url) = &record.remote_url {
                return Ok(url.clone());
            }
        }
        let _permit = self
            .upload_gate
            .acquire()
            .await
            .map_err(|_| "图片上传服务已停止".to_string())?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let bytes = tokio::fs::read(self.root.join("objects").join(&record.path))
                .await
                .map_err(|_| "读取本地图片失败".to_string())?;
            self.dav_put(config, password, &record.path, bytes, &record.content_type)
                .await
        })
        .await
        .unwrap_or_else(|_| Err("WebDAV 上传超过 30 秒".into()));
        let mut updated = record.clone();
        updated.destination = Some(destination);
        updated.attempts = if record.destination == updated.destination {
            record.attempts.saturating_add(1)
        } else {
            1
        };
        match &result {
            Ok(url) => {
                updated.remote_url = Some(url.clone());
                updated.last_error = None;
                updated.next_retry_at = 0;
            }
            Err(error) => {
                updated.remote_url = None;
                updated.last_error = Some(error.clone());
                updated.next_retry_at = now() + 60 * (1i64 << updated.attempts.min(6));
            }
        }
        self.persist(&updated).await?;
        index
            .write()
            .await
            .records
            .insert(updated.id.clone(), updated);
        result
    }
    pub async fn test_webdav(&self, config: &ImageConfig, password: &str) -> serde_json::Value {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII=").expect("PNG fixture");
        let relative = format!(
            "{}/aether-test-{}.png",
            chrono::Utc::now().format("%Y/%m/%d"),
            uuid::Uuid::new_v4().simple()
        );
        // Individual outcomes distinguish authentication, public mapping, and DELETE permissions.
        let full_path = format!("{}/{}", config.webdav_root.trim_matches('/'), relative)
            .trim_start_matches('/')
            .to_string();
        let remote = joined_url(&config.webdav_url, &full_path);
        let upload = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let target = self.dav_directories(config, password, &relative).await?;
            let response = self
                .dav_client
                .put(target)
                .basic_auth(&config.webdav_username, Some(password))
                .header("content-type", "image/png")
                .body(bytes.clone())
                .send()
                .await
                .map_err(|_| "WebDAV 上传连接失败".to_string())?;
            if response.status().is_success() {
                Ok(())
            } else {
                Err(format!(
                    "WebDAV 上传失败：HTTP {}",
                    response.status().as_u16()
                ))
            }
        })
        .await
        .unwrap_or_else(|_| Err("WebDAV 测试上传超时".into()));
        let public = if upload.is_ok() {
            match joined_url(&config.webdav_public_url, &relative) {
                Ok(url) => match self.download(url.as_str()).await {
                    Ok(downloaded) if downloaded == bytes => Ok(url.to_string()),
                    Ok(_) => Err("公开地址返回内容不匹配".into()),
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        } else {
            Err("上传失败，未测试公开访问".into())
        };
        // Always attempt DELETE, including when PUT timed out after creating the file.
        let deletion = match remote {
            Ok(url) => match self
                .dav_client
                .delete(url)
                .basic_auth(&config.webdav_username, Some(password))
                .send()
                .await
            {
                Ok(response)
                    if response.status().is_success() || response.status().as_u16() == 404 =>
                {
                    Ok(())
                }
                Ok(response) => Err(format!(
                    "删除测试文件失败：HTTP {}",
                    response.status().as_u16()
                )),
                Err(_) => Err("删除测试文件连接失败或超时".into()),
            },
            Err(error) => Err(error),
        };
        serde_json::json!({"success": upload.is_ok() && public.is_ok() && deletion.is_ok(),
            "upload": {"success": upload.is_ok(), "error": upload.err()},
            "public_access": {"success": public.is_ok(), "url": public.as_ref().ok(), "error": public.as_ref().err()},
            "delete": {"success": deletion.is_ok(), "error": deletion.err()}})
    }
}
