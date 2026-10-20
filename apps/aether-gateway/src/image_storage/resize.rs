use super::storage::{ImageStorage, MAX_IMAGE_BYTES};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ImageSize {
    pub width: u32,
    pub height: u32,
}

impl ImageSize {
    pub fn parse(value: Option<&str>) -> Result<Option<Self>, String> {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        if value == "auto" {
            return Ok(None);
        }
        let invalid = || {
            "图片缩放的 size 必须为宽x高（单边 1–8192，总计不超过 3200 万像素）或 auto".to_string()
        };
        let (width, height) = value.split_once('x').ok_or_else(invalid)?;
        if !width.bytes().all(|b| b.is_ascii_digit()) || !height.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let width = width.parse::<u32>().map_err(|_| invalid())?;
        let height = height.parse::<u32>().map_err(|_| invalid())?;
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || u64::from(width) * u64::from(height) > 32_000_000
        {
            return Err(invalid());
        }
        Ok(Some(Self { width, height }))
    }
}

fn helper_path() -> PathBuf {
    std::env::var_os("AETHER_IMAGE_RESIZER_SCRIPT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let packaged = PathBuf::from("/opt/aether/image-resizer/resize.mjs");
            if packaged.is_file() {
                packaged
            } else {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tools/image-resizer/resize.mjs")
            }
        })
}

impl ImageStorage {
    pub async fn resize(&self, bytes: Vec<u8>, size: ImageSize) -> Result<Vec<u8>, String> {
        let _permit = self
            .resize_gate
            .acquire()
            .await
            .map_err(|_| "图片缩放服务已停止".to_string())?;
        let script = helper_path();
        if !script.is_file() {
            return Err("Sharp 图片缩放组件未安装，请使用包含该组件的新镜像".into());
        }
        let node = std::env::var_os("AETHER_IMAGE_RESIZER_NODE").unwrap_or_else(|| "node".into());
        let mut child = tokio::process::Command::new(node)
            .arg(script)
            .arg(size.width.to_string())
            .arg(size.height.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                "无法启动 Sharp 图片缩放组件，请检查 Node.js 和 Sharp 是否已安装".to_string()
            })?;
        let mut input = child.stdin.take().ok_or("图片缩放输入不可用")?;
        let output = child.stdout.take().ok_or("图片缩放输出不可用")?;
        let operation = async {
            // Read and write concurrently: neither pipe can block the other.
            let write = async {
                let result = input.write_all(&bytes).await;
                drop(input);
                result
            };
            let read = async {
                let mut result = Vec::new();
                output
                    .take((MAX_IMAGE_BYTES + 1) as u64)
                    .read_to_end(&mut result)
                    .await?;
                Ok::<_, std::io::Error>(result)
            };
            let (written, result) = tokio::join!(write, read);
            let result = result.map_err(|_| "读取图片缩放结果失败".to_string())?;
            if result.len() > MAX_IMAGE_BYTES {
                return Err("缩放后的图片超过 64 MiB".to_string());
            }
            let status = child
                .wait()
                .await
                .map_err(|_| "等待图片缩放结果失败".to_string())?;
            if !status.success() {
                return Err("Sharp 图片缩放失败：请检查图片是否为有效的 PNG、JPEG 或 WebP，且未超过 3200 万像素".into());
            }
            written.map_err(|_| "写入图片缩放数据失败".to_string())?;
            if result.is_empty() {
                return Err("Sharp 图片缩放结果为空".into());
            }
            Ok(result)
        };
        tokio::time::timeout(Duration::from_secs(60), operation)
            .await
            .map_err(|_| "Sharp 图片缩放超时（60 秒）".to_string())?
    }
}
