use crate::{AppState, GatewayError};
use serde::{Deserialize, Serialize};

pub(crate) const CONFIG_KEY: &str = "image_storage";
pub(crate) const PASSWORD_KEY: &str = "image_webdav_password";

/// Storage and response preferences are independent; request overrides are opt-in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ImageConfig {
    pub return_format: String,
    pub honor_response_format: bool,
    pub public_base_url: String,
    pub storage_mode: String,
    pub webdav_url: String,
    pub webdav_username: String,
    pub webdav_root: String,
    pub webdav_public_url: String,
    pub cleanup_enabled: bool,
    pub retention_hours: u64,
    pub cleanup_interval_hours: u64,
}
impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            return_format: "base64".into(),
            honor_response_format: false,
            public_base_url: String::new(),
            storage_mode: "local".into(),
            webdav_url: String::new(),
            webdav_username: String::new(),
            webdav_root: "aether/images".into(),
            webdav_public_url: String::new(),
            cleanup_enabled: false,
            retention_hours: 168,
            cleanup_interval_hours: 6,
        }
    }
}
impl ImageConfig {
    pub fn stores_local(&self) -> bool {
        matches!(self.storage_mode.as_str(), "local" | "local_webdav")
    }
    pub fn stores_webdav(&self) -> bool {
        matches!(self.storage_mode.as_str(), "webdav" | "local_webdav")
    }
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.storage_mode.as_str(),
            "local" | "webdav" | "local_webdav"
        ) {
            return Err("图片存储方式必须为 local、webdav 或 local_webdav".into());
        }
        if !matches!(self.return_format.as_str(), "base64" | "url") {
            return Err("图片返回格式必须为 base64 或 url".into());
        }
        for (label, value, enabled) in [
            ("本地公开地址", &self.public_base_url, self.stores_local()),
            ("WebDAV URL", &self.webdav_url, self.stores_webdav()),
            (
                "WebDAV 公开前缀",
                &self.webdav_public_url,
                self.stores_webdav(),
            ),
        ] {
            if enabled && !value.is_empty() {
                validate_http_url(value).map_err(|_| {
                    format!("{label} 必须为不带凭据、查询参数和片段的 http(s) 地址")
                })?;
            }
        }
        if self.stores_webdav() && (self.webdav_url.is_empty() || self.webdav_public_url.is_empty())
        {
            return Err("启用 WebDAV 时必须配置 WebDAV URL 和公开访问前缀".into());
        }
        if self.stores_webdav() {
            validate_relative_root(&self.webdav_root)?;
        }
        if self.retention_hours == 0
            || self.retention_hours > 87600
            || self.cleanup_interval_hours == 0
            || self.cleanup_interval_hours > 8760
        {
            return Err("保留时间应为 1–87600 小时，清理间隔应为 1–8760 小时".into());
        }
        if self.stores_webdav() && self.webdav_username.len() > 256 {
            return Err("WebDAV 用户名过长".into());
        }
        Ok(())
    }
    pub fn use_url(&self, requested: Option<&str>) -> Result<bool, String> {
        if self.honor_response_format {
            match requested {
                Some("url") => return Ok(true),
                Some("b64_json" | "base64") => return Ok(false),
                Some(_) => return Err("response_format 必须为 b64_json 或 url".into()),
                None => {}
            }
        }
        Ok(self.return_format == "url")
    }
    pub fn destination(&self) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "{:x}",
            Sha256::digest(format!(
                "{}\n{}\n{}\n{}",
                self.webdav_url, self.webdav_root, self.webdav_public_url, self.webdav_username
            ))
        )
    }
}
pub(crate) fn validate_http_url(value: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(value).map_err(|_| "Invalid URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Invalid public URL".into());
    }
    Ok(url)
}
pub(crate) fn validate_relative_root(value: &str) -> Result<(), String> {
    if value.len() > 512
        || value.contains(['\\', '?', '#', '%'])
        || value.starts_with('/')
        || value
            .split('/')
            .any(|p| p == "." || p == ".." || p.chars().any(char::is_control))
    {
        return Err("WebDAV 根路径必须为相对目录，不能包含 ..、反斜杠或 URL 转义".into());
    }
    Ok(())
}
/// Migrate the original opt-in WebDAV setting without losing the existing destination.
pub(crate) fn parse_config(mut value: serde_json::Value) -> Result<ImageConfig, serde_json::Error> {
    if let Some(object) = value.as_object_mut() {
        if let Some(legacy) = object.remove("webdav_enabled") {
            let enabled: bool = serde_json::from_value(legacy)?;
            object.entry("storage_mode").or_insert_with(|| {
                serde_json::Value::String(if enabled { "local_webdav" } else { "local" }.into())
            });
        }
    }
    serde_json::from_value(value)
}
pub(crate) async fn load_config(state: &AppState) -> Result<ImageConfig, GatewayError> {
    let config: ImageConfig = match state.read_system_config_json_value(CONFIG_KEY).await? {
        Some(value) => parse_config(value)
            .map_err(|_| GatewayError::Internal("图片存储配置格式错误".into()))?,
        None => ImageConfig::default(),
    };
    config.validate().map_err(GatewayError::Internal)?;
    Ok(config)
}
pub(crate) async fn load_password(state: &AppState) -> Result<String, String> {
    let stored = state
        .read_system_config_json_value(PASSWORD_KEY)
        .await
        .map_err(|_| "读取 WebDAV 密码失败".to_string())?;
    let Some(stored) = stored
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
    else {
        return Ok(String::new());
    };
    crate::handlers::shared::decrypt_system_config_secret(state, PASSWORD_KEY, &stored)
        .ok_or_else(|| "WebDAV 密码解密失败，请重新保存密码".into())
}
