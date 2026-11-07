use std::path::Path;
use tauri::Manager;

/// 把单个绝对路径按需加入 asset 协议白名单（相对路径由数据目录静态 scope 覆盖，直接忽略）
pub fn allow_file(app: &tauri::AppHandle, path: &str) {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return;
    }
    let candidate = Path::new(trimmed);
    if !candidate.is_absolute() {
        return;
    }
    let _ = app.asset_protocol_scope().allow_file(candidate);
}

/// 递归授权整个目录（用于 portable / 自定义存储目录等构建期无法确定的根目录）
pub fn allow_directory(app: &tauri::AppHandle, path: &Path) {
    if !path.is_absolute() {
        return;
    }
    let _ = app.asset_protocol_scope().allow_directory(path, true);
}

/// 剪贴板 / 收藏内容里的 `files:<json>` 记录，逐个授权其中引用到的本地路径
pub fn allow_files_content(app: &tauri::AppHandle, content: &str) {
    let Some(json) = content.strip_prefix("files:") else {
        return;
    };
    let Ok(data) = serde_json::from_str::<crate::services::paste::FilesData>(json) else {
        return;
    };
    for file in data.files {
        allow_file(app, &crate::services::resolve_stored_path(&file.path));
        if let Some(actual_path) = file.actual_path.as_deref() {
            allow_file(app, actual_path);
        }
    }
}

/// 设置里由用户自选的背景图与自定义字体可能位于任意目录，读取设置时按需授权
pub fn allow_settings_paths(app: &tauri::AppHandle, settings: &crate::services::AppSettings) {
    allow_file(app, &settings.background_image_path);
    allow_file(app, &settings.custom_font_path);
}
