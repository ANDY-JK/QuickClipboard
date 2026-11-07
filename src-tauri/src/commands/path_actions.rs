//! 受约束的本地路径动作。
//!
//! 原先前端直接调用 `opener:allow-open-path`（路径通配 `**`），渲染层一旦被注入
//! 即可用系统默认程序打开任意文件（含可执行文件）。这里把能力收敛到后端：
//! - 打开数据目录只接受固定枚举，路径由后端自行拼装，前端无法传入任意路径；
//! - 打开剪贴板里的文件只接受已存在文件，并拒绝可执行/脚本类扩展名；
//! - 在文件管理器中定位只接受已存在的文件或目录，且不泄露后端绝对路径。

use std::path::{Path, PathBuf};

/// 不允许交给系统默认程序打开的可执行/脚本扩展名，避免把「打开文件」变成代码执行。
const BLOCKED_EXTENSIONS: &[&str] = &[
    "exe", "com", "bat", "cmd", "msi", "msp", "ps1", "psm1", "vbs", "vbe", "wsf", "wsh", "scr",
    "pif", "lnk", "hta", "cpl", "msc", "reg", "js", "jse", "jar", "dll", "sys", "inf", "py",
    "pyw", "sh", "bash", "run", "gadget", "url", "website", "scf", "search-ms",
    "settingcontent-ms",
];

fn is_blocked_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| BLOCKED_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn canonicalize_existing(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("路径不能为空".to_string());
    }
    let candidate = PathBuf::from(trimmed);
    if !candidate.is_absolute() {
        return Err("仅支持绝对路径".to_string());
    }
    std::fs::canonicalize(&candidate).map_err(|_| "文件不存在或无法访问".to_string())
}

/// 在系统文件管理器中打开应用数据目录。
/// `kind` 仅接受固定枚举，实际路径全部由后端解析，不接受前端传入路径。
#[tauri::command]
pub async fn open_app_data_folder(kind: String) -> Result<(), String> {
    let dir = match kind.as_str() {
        "storage" => crate::services::data_management::get_current_storage_dir()?,
        "extensions" => crate::services::extensions::extensions_dir()?,
        "clipboard_images" | "pin_images" | "image_library" | "thumbnails" => {
            crate::services::get_data_directory()?.join(kind)
        }
        _ => return Err("不支持的目录类型".to_string()),
    };

    let dir = dir
        .canonicalize()
        .or_else(|_| {
            std::fs::create_dir_all(&dir)?;
            dir.canonicalize()
        })
        .map_err(|_| "无法访问目标目录".to_string())?;

    tauri_plugin_opener::open_path(&dir, None::<&str>).map_err(|_| "无法打开目标目录".to_string())
}

/// 用系统默认程序打开剪贴板里的本地文件（目录则在文件管理器中打开）。
/// 仅接受已存在的绝对路径，并拒绝可执行/脚本扩展名。
#[tauri::command]
pub async fn open_local_file(path: String) -> Result<(), String> {
    let canonical = canonicalize_existing(&path)?;
    if canonical.is_dir() {
        return tauri_plugin_opener::open_path(&canonical, None::<&str>)
            .map_err(|_| "无法打开目标目录".to_string());
    }
    if !canonical.is_file() {
        return Err("目标不是文件".to_string());
    }
    if is_blocked_extension(&canonical) {
        return Err("出于安全考虑，不支持用默认程序打开可执行或脚本文件".to_string());
    }
    tauri_plugin_opener::open_path(&canonical, None::<&str>)
        .map_err(|_| "无法用默认程序打开文件".to_string())
}

/// 在系统文件管理器中定位本地文件/目录。
#[tauri::command]
pub async fn reveal_local_path(path: String) -> Result<(), String> {
    let canonical = canonicalize_existing(&path)?;
    let target = if canonical.is_dir() {
        canonical
    } else if canonical.is_file() {
        canonical
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "无法定位文件所在目录".to_string())?
    } else {
        return Err("目标不存在或无法访问".to_string());
    };
    tauri_plugin_opener::open_path(&target, None::<&str>)
        .map_err(|_| "无法打开文件所在目录".to_string())
}

#[cfg(test)]
mod tests {
    use super::is_blocked_extension;
    use std::path::Path;

    #[test]
    fn blocks_executable_and_script_extensions() {
        for name in [
            "a.exe", "a.EXE", "a.bat", "a.cmd", "a.ps1", "a.vbs", "a.js", "a.lnk", "a.msi",
            "a.settingcontent-ms", "a.search-ms", "a.url",
        ] {
            assert!(is_blocked_extension(Path::new(name)), "{} 应被拦截", name);
        }
    }

    #[test]
    fn allows_common_clipboard_file_extensions() {
        for name in ["a.png", "a.jpg", "a.mp4", "a.pdf", "a.txt", "a.docx", "noext"] {
            assert!(!is_blocked_extension(Path::new(name)), "{} 不应被拦截", name);
        }
    }
}
