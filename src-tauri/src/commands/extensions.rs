use crate::services::extensions::install::{self, InstallOutcome};
use crate::services::extensions::{self, ExtensionInfo};
use std::path::PathBuf;

// 扩展扫描/入口读取都是磁盘 IO（read_dir + 逐项 metadata/读取 + canonicalize），
// 放到阻塞线程执行，避免占用 IPC 主线程。
#[tauri::command]
pub async fn list_extensions() -> Result<Vec<ExtensionInfo>, String> {
    tauri::async_runtime::spawn_blocking(extensions::list_extensions)
        .await
        .map_err(|e| format!("扩展列表任务执行失败: {}", e))?
}

#[tauri::command]
pub async fn read_extension_entry(id: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || extensions::read_extension_entry(&id))
        .await
        .map_err(|e| format!("扩展入口读取任务执行失败: {}", e))?
}

// 安装要解包/复制最多 8 MB 的内容，卸载要递归删目录，都属于阻塞 IO。
#[tauri::command]
pub async fn install_extension(path: String, overwrite: bool) -> Result<InstallOutcome, String> {
    let source = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || install::install_extension(&source, overwrite))
        .await
        .map_err(|e| format!("扩展安装任务执行失败: {}", e))?
}

#[tauri::command]
pub async fn uninstall_extension(id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || install::uninstall_extension(&id))
        .await
        .map_err(|e| format!("扩展卸载任务执行失败: {}", e))?
}
