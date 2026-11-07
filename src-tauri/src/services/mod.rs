pub mod clipboard;
pub mod database;
pub mod data_management;
pub mod extensions;
pub mod notification;
pub mod settings;
pub mod system;
pub mod asset_scope;
pub mod paste;
pub mod sound;
pub mod image_library;
pub mod low_memory;
pub mod memory;
pub mod store;
pub mod storage_maintenance;

pub use settings::{AppSettings, get_settings, update_settings, get_data_directory};
pub use notification::show_startup_notification;
pub use system::hotkey;
pub use sound::{SoundPlayer, AppSounds, mark_paste_operation};

// 图片限制策略（0 表示对应维度/大小不限制）
pub struct ImageLimits {
    pub max_size_bytes: u64,
    pub max_width: u32,
    pub max_height: u32,
}

pub fn image_limits() -> ImageLimits {
    let settings = get_settings();
    ImageLimits {
        max_size_bytes: if settings.image_max_size_mb == 0 {
            0
        } else {
            settings.image_max_size_mb as u64 * 1024 * 1024
        },
        max_width: settings.image_max_width,
        max_height: settings.image_max_height,
    }
}

// 按宽高上限等比缩放尺寸；0 表示该方向不限制
pub fn fit_dimensions(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let limit_w = if max_width == 0 { u32::MAX } else { max_width };
    let limit_h = if max_height == 0 { u32::MAX } else { max_height };
    if width <= limit_w && height <= limit_h {
        return (width, height);
    }

    let ratio = (limit_w as f64 / width as f64).min(limit_h as f64 / height as f64);
    (
        (width as f64 * ratio).round().max(1.0) as u32,
        (height as f64 * ratio).round().max(1.0) as u32,
    )
}

// 编码结果超过大小上限时，按面积比例估算等比缩小尺寸；None 表示无需缩小
pub fn shrink_dimensions_for_bytes(
    width: u32,
    height: u32,
    current_bytes: u64,
    max_bytes: u64,
) -> Option<(u32, u32)> {
    if max_bytes == 0 || current_bytes <= max_bytes {
        return None;
    }

    let ratio = (max_bytes as f64 / current_bytes as f64).sqrt();
    let new_width = ((width as f64 * ratio).round() as u32).max(1);
    let new_height = ((height as f64 * ratio).round() as u32).max(1);

    if new_width >= width && new_height >= height {
        None
    } else {
        Some((new_width, new_height))
    }
}

pub fn normalize_path_for_hash(path: &str) -> String {
    let normalized = path.replace("\\", "/");
    for prefix in ["clipboard_images/", "pin_images/"] {
        if let Some(idx) = normalized.find(prefix) {
            return normalized[idx..].to_string();
        }
    }
    normalized
}

// 解析存储的路径为实际绝对路径
pub fn resolve_stored_path(stored_path: &str) -> String {
    let normalized_input = stored_path.replace("/", "\\");
    
    if normalized_input.starts_with("clipboard_images\\") 
        || normalized_input.starts_with("pin_images\\")
        || normalized_input.starts_with("image_library\\") {
        if let Ok(data_dir) = get_data_directory() {
            return data_dir.join(&normalized_input).to_string_lossy().to_string();
        }
    }
    
    let search_path = stored_path.replace("\\", "/");
    for prefix in ["clipboard_images/", "pin_images/", "image_library/"] {
        if let Some(idx) = search_path.find(prefix) {
            if let Ok(data_dir) = get_data_directory() {
                let relative = search_path[idx..].replace("/", "\\");
                let new_path = data_dir.join(&relative);
                if new_path.exists() {
                    return new_path.to_string_lossy().to_string();
                }
            }
        }
    }
    
    stored_path.to_string()
}

pub fn is_portable_build() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()))
        .map(|name| name.contains("portable"))
        .unwrap_or(false)
}
