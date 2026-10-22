use super::config::ImageConfig;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{OnceCell, RwLock, Semaphore};

pub(crate) const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImageRecord {
    pub id: String,
    pub path: String,
    pub content_type: String,
    pub bytes: u64,
    pub created_at: i64,
    pub remote_url: Option<String>,
    pub destination: Option<String>,
    pub attempts: u32,
    pub next_retry_at: i64,
    pub last_error: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CleanupRun {
    pub started_at: i64,
    pub deleted: u64,
    pub freed_bytes: u64,
    pub skipped_pending: u64,
    pub errors: u64,
}
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct SyncProgress {
    pub running: bool,
    pub total: u64,
    pub completed: u64,
    pub failed: u64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}
#[derive(Debug, Default)]
pub(crate) struct ImageIndex {
    pub records: HashMap<String, ImageRecord>,
    pub last_cleanup: Option<CleanupRun>,
    pub sync: SyncProgress,
}
#[derive(Debug)]
pub(crate) struct ImageStorage {
    pub root: PathBuf,
    pub started_at: i64,
    pub index: OnceCell<RwLock<ImageIndex>>,
    pub active: Arc<Mutex<HashMap<String, usize>>>,
    pub operation: tokio::sync::Mutex<()>,
    pub sync_gate: Arc<Semaphore>,
    pub response_gate: Arc<Semaphore>,
    pub resize_gate: Semaphore,
    pub upload_gate: Arc<Semaphore>,
    pub dav_client: reqwest::Client,
    pub public_client: reqwest::Client,
}
pub(crate) struct ActiveImage {
    active: Arc<Mutex<HashMap<String, usize>>>,
    id: String,
}
impl Drop for ActiveImage {
    fn drop(&mut self) {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = active.get_mut(&self.id) {
            *count -= 1;
            if *count == 0 {
                active.remove(&self.id);
            }
        }
    }
}
pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub(crate) fn image_type(bytes: &[u8]) -> Result<(&'static str, &'static str), String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok(("png", "image/png"))
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok(("jpg", "image/jpeg"))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Ok(("webp", "image/webp"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Ok(("gif", "image/gif"))
    } else if bytes.get(4..8) == Some(b"ftyp")
        && bytes
            .get(8..12)
            .is_some_and(|b| b == b"avif" || b == b"avis")
    {
        Ok(("avif", "image/avif"))
    } else {
        Err("上游返回的图片不是受支持的 PNG、JPEG、WebP、GIF 或 AVIF".into())
    }
}
impl ImageStorage {
    pub fn from_env() -> Result<Self, reqwest::Error> {
        let root = std::env::var_os("AETHER_IMAGE_STORAGE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data/images"));
        Self::new(root)
    }
    pub fn new(root: PathBuf) -> Result<Self, reqwest::Error> {
        Ok(Self {
            root,
            started_at: now(),
            index: OnceCell::new(),
            active: Arc::default(),
            operation: tokio::sync::Mutex::new(()),
            sync_gate: Arc::new(Semaphore::new(1)),
            response_gate: Arc::new(Semaphore::new(8)),
            resize_gate: Semaphore::new(2),
            upload_gate: Arc::new(Semaphore::new(4)),
            dav_client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(25))
                .build()?,
            public_client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::limited(3))
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(25))
                .build()?,
        })
    }
    pub fn claim(&self, id: &str) -> Option<ActiveImage> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active.contains_key(id) {
            return None;
        }
        active.insert(id.into(), 1);
        Some(ActiveImage {
            active: self.active.clone(),
            id: id.into(),
        })
    }
    pub fn reader(&self, id: &str) -> ActiveImage {
        *self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.into())
            .or_default() += 1;
        ActiveImage {
            active: self.active.clone(),
            id: id.into(),
        }
    }
    pub async fn index(&self) -> Result<&RwLock<ImageIndex>, String> {
        self.index
            .get_or_try_init(|| async {
                tokio::fs::create_dir_all(self.root.join("objects"))
                    .await
                    .map_err(|_| "图片目录不可写，请检查持久化挂载".to_string())?;
                let meta = self.root.join("metadata");
                tokio::fs::create_dir_all(&meta)
                    .await
                    .map_err(|_| "图片元数据目录不可写".to_string())?;
                let mut index = ImageIndex::default();
                let mut dir = tokio::fs::read_dir(meta)
                    .await
                    .map_err(|_| "读取图片索引失败".to_string())?;
                while let Some(entry) = dir
                    .next_entry()
                    .await
                    .map_err(|_| "扫描图片索引失败".to_string())?
                {
                    if !entry
                        .file_type()
                        .await
                        .map(|t| t.is_file())
                        .unwrap_or(false)
                        || entry.path().extension().and_then(|s| s.to_str()) != Some("json")
                    {
                        continue;
                    }
                    if !entry
                        .metadata()
                        .await
                        .map(|m| m.len() <= 64 * 1024)
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let Ok(bytes) = tokio::fs::read(entry.path()).await else {
                        continue;
                    };
                    let Ok(record) = serde_json::from_slice::<ImageRecord>(&bytes) else {
                        continue;
                    };
                    if !valid_record(&record) {
                        continue;
                    }
                    if let Ok(metadata) =
                        tokio::fs::symlink_metadata(self.root.join("objects").join(&record.path))
                            .await
                    {
                        if metadata.is_file() && !metadata.file_type().is_symlink() {
                            index.records.insert(
                                record.id.clone(),
                                ImageRecord {
                                    bytes: metadata.len(),
                                    ..record
                                },
                            );
                        }
                    }
                }
                index.last_cleanup = tokio::fs::read(self.root.join("cleanup.json"))
                    .await
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok());
                Ok(RwLock::new(index))
            })
            .await
    }
    pub async fn persist(&self, record: &ImageRecord) -> Result<(), String> {
        let bytes = serde_json::to_vec(record).map_err(|_| "图片元数据序列化失败".to_string())?;
        atomic_write(
            &self
                .root
                .join("metadata")
                .join(format!("{}.json", record.id)),
            &bytes,
        )
        .await
    }
    pub async fn save(&self, bytes: &[u8]) -> Result<ImageRecord, String> {
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err("图片为空或超过 64 MiB".into());
        }
        let (extension, content_type) = image_type(bytes)?;
        let index = self.index().await?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let record = ImageRecord {
            path: format!(
                "{}/{}.{}",
                chrono::Utc::now().format("%Y/%m/%d"),
                id,
                extension
            ),
            id,
            content_type: content_type.into(),
            bytes: bytes.len() as u64,
            created_at: now(),
            remote_url: None,
            destination: None,
            attempts: 0,
            next_retry_at: 0,
            last_error: None,
        };
        let path = self.root.join("objects").join(&record.path);
        tokio::fs::create_dir_all(path.parent().ok_or("Invalid image path")?)
            .await
            .map_err(|_| "创建图片目录失败".to_string())?;
        atomic_write(&path, bytes).await?;
        if let Err(err) = self.persist(&record).await {
            let _ = tokio::fs::remove_file(path).await;
            return Err(err);
        }
        index
            .write()
            .await
            .records
            .insert(record.id.clone(), record.clone());
        Ok(record)
    }
    pub async fn cleanup(
        &self,
        config: &ImageConfig,
        include_failed: bool,
    ) -> Result<CleanupRun, String> {
        let _operation = self
            .operation
            .try_lock()
            .map_err(|_| "已有清理正在运行".to_string())?;
        let index = self.index().await?;
        let cutoff = now().saturating_sub((config.retention_hours * 3600) as i64);
        let records: Vec<_> = index
            .read()
            .await
            .records
            .values()
            .filter(|r| r.created_at < cutoff)
            .cloned()
            .collect();
        let mut run = CleanupRun {
            started_at: now(),
            ..CleanupRun::default()
        };
        for record in records {
            let Some(_active) = self.claim(&record.id) else {
                continue;
            };
            let needs_sync = config.stores_webdav()
                && (record.remote_url.is_none()
                    || record.destination.as_deref() != Some(config.destination().as_str()));
            if needs_sync && !include_failed {
                run.skipped_pending += 1;
                continue;
            }
            let file = self.root.join("objects").join(&record.path);
            match tokio::fs::remove_file(&file).await {
                Ok(()) => {
                    run.deleted += 1;
                    run.freed_bytes += record.bytes;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    run.errors += 1;
                    continue;
                }
            }
            if tokio::fs::remove_file(
                self.root
                    .join("metadata")
                    .join(format!("{}.json", record.id)),
            )
            .await
            .is_err()
            {
                run.errors += 1;
            }
            index.write().await.records.remove(&record.id);
            // Only remove the three managed date folders, and only when empty.
            let mut folder = file.parent();
            for _ in 0..3 {
                if let Some(path) = folder {
                    let _ = tokio::fs::remove_dir(path).await;
                    folder = path.parent();
                }
            }
        }
        let encoded = serde_json::to_vec(&run).map_err(|_| "清理记录序列化失败".to_string())?;
        atomic_write(&self.root.join("cleanup.json"), &encoded).await?;
        index.write().await.last_cleanup = Some(run.clone());
        Ok(run)
    }
    pub async fn stats(&self, config: &ImageConfig) -> Result<serde_json::Value, String> {
        let index = self.index().await?.read().await;
        let destination = config.destination();
        let mut total_bytes = 0u64;
        let mut synced = 0u64;
        let mut failed = 0u64;
        let mut expired_pending = 0u64;
        for record in index.records.values() {
            total_bytes += record.bytes;
            if record.remote_url.is_some()
                && record.destination.as_deref() == Some(destination.as_str())
            {
                synced += 1;
            } else {
                if record.last_error.is_some()
                    && record.destination.as_deref() == Some(destination.as_str())
                {
                    failed += 1;
                }
                if config.stores_webdav()
                    && record.created_at < now() - (config.retention_hours * 3600) as i64
                {
                    expired_pending += 1;
                }
            }
        }
        let last_cleanup = index.last_cleanup.clone();
        let next_cleanup_at = config.cleanup_enabled.then(|| {
            last_cleanup
                .as_ref()
                .map(|r| r.started_at)
                .unwrap_or(self.started_at)
                + (config.cleanup_interval_hours * 3600) as i64
        });
        let mut failures: Vec<_> = index
            .records
            .values()
            .filter(|r| {
                r.last_error.is_some() && r.destination.as_deref() == Some(destination.as_str())
            })
            .collect();
        failures.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        let failure_details: Vec<_> = failures
            .into_iter()
            .take(10)
            .map(|r| serde_json::json!({"id":r.id,"error":r.last_error,"attempts":r.attempts}))
            .collect();
        let result = serde_json::json!({
            "directory": self.root.to_string_lossy(), "count": index.records.len(), "bytes": total_bytes,
            "synced": synced, "pending": index.records.len() as u64 - synced - failed,
            "failed": failed, "expired_pending": expired_pending, "webdav_enabled": config.stores_webdav(),
            "last_cleanup": last_cleanup, "next_cleanup_at": next_cleanup_at,
            "sync": index.sync, "updated_at": now(), "failure_details": failure_details,
        });
        drop(index);
        let mut result = result;
        result["task_input_bytes"] =
            serde_json::json!(crate::image_tasks::input_bytes(&self.root).await);
        let root = self.root.clone();
        let disk = tokio::task::spawn_blocking(move || {
            let canonical = std::fs::canonicalize(root).ok()?;
            let disks = sysinfo::Disks::new_with_refreshed_list();
            let disk = disks.iter().filter(|d| canonical.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len())?;
            Some(serde_json::json!({"total": disk.total_space(), "available": disk.available_space(), "used_percent": if disk.total_space() > 0 { 100.0 * (disk.total_space() - disk.available_space()) as f64 / disk.total_space() as f64 } else { 0.0 }}))
        }).await.ok().flatten();
        let mut result = result;
        result["disk"] = disk.unwrap_or(serde_json::Value::Null);
        Ok(result)
    }
}
pub(crate) fn valid_record(record: &ImageRecord) -> bool {
    record.id.len() == 32
        && record.id.bytes().all(|b| b.is_ascii_hexdigit())
        && valid_image_path(&record.path)
        && record
            .path
            .rsplit('/')
            .next()
            .and_then(|f| f.split('.').next())
            == Some(record.id.as_str())
        && matches!(
            record.content_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "image/avif"
        )
}
pub(crate) fn valid_image_path(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 4
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || parts[..3]
            .iter()
            .any(|s| !s.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    let Some((id, extension)) = parts[3].split_once('.') else {
        return false;
    };
    id.len() == 32
        && id.bytes().all(|b| b.is_ascii_hexdigit())
        && matches!(extension, "png" | "jpg" | "webp" | "gif" | "avif")
}
pub(crate) async fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await
            .map_err(|_| "创建图片文件失败".to_string())?;
        file.write_all(bytes)
            .await
            .map_err(|_| "写入图片文件失败".to_string())?;
        file.sync_all()
            .await
            .map_err(|_| "同步图片文件失败".to_string())?;
        drop(file);
        tokio::fs::rename(&temp, path)
            .await
            .map_err(|_| "保存图片文件失败".to_string())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(temp).await;
    }
    result
}
