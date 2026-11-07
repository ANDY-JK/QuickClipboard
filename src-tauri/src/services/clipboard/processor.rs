use super::capture::{ClipboardContent, ContentType as CaptureType};
use super::content_type::ContentType;
use crate::services::database::ClipboardDataSeed;
use image::ImageFormat;
use std::io::Cursor;
use std::fs;
use std::path::Path;
use regex::Regex;
use sha2::{Sha256, Digest};
use serde::{Serialize, Deserialize};
use crate::utils::html_sanitizer::{
    limit_html_bytes, sanitize_clipboard_document, sanitize_raw_html_formats,
    strip_oversized_data_images,
};

// html_content 与 raw "HTML Format" 行的体积上限。
// 取 8 MiB：单张截图内嵌的 data:image base64 常见在 3~4 MB，若按 1~2 MiB 截断会把图片截坏；
// 超限时只在标签边界截断前段，保留完整结构与内容，不整条丢弃、不退化成纯文本。
const MAX_STORED_HTML_BYTES: usize = 8 * 1024 * 1024;


// 文件信息结构
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileInfo {
    path: String,
    name: String,
    size: u64,
    is_directory: bool,
    icon_data: Option<String>,
    file_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
}

// 文件剪贴板数据
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileClipboardData {
    files: Vec<FileInfo>,
    operation: String,
}

// 处理后的剪贴板数据结构
pub struct ProcessedContent {
    pub content: String,              
    pub html_content: Option<String>, 
    pub content_type: String,         
    pub image_id: Option<String>,
    pub source_app: Option<String>,      
    pub source_icon_hash: Option<String>,
    pub raw_formats: Vec<ClipboardDataSeed>,
}

// 处理剪贴板内容，将原始数据转换为可存储的格式
pub fn process_content(content: ClipboardContent) -> Result<ProcessedContent, String> {
    let (source_app, source_icon_hash) = get_source_info();

    // raw 的 "HTML Format" 行优先于 html_content 写回，这里统一清洗，避免绕过净化
    let mut raw_formats = content.raw_formats;
    sanitize_raw_html_formats(&mut raw_formats, MAX_STORED_HTML_BYTES);
    
    match content.content_type {
        // 纯文本处理
        CaptureType::Text => {
            let text = content.text.ok_or("文本内容为空")?;
            
            let mut ct = ContentType::new("text");
            
            if is_url(&text) {
                ct.add_type("link");
            } else if contains_links(&text) {
                ct.add_type("link");
            }

            let image_id = content
                .image_path
                .as_deref()
                .and_then(extract_image_id_from_path);
            if image_id.is_some() {
                ct.add_type("image");
            }

            Ok(ProcessedContent {
                content: text,
                html_content: None,
                content_type: ct.to_db_string(),
                image_id,
                source_app,
                source_icon_hash,
                raw_formats,
            })
        }
        
            // 富文本处理（HTML）
            CaptureType::RichText => {
                let raw_html = content.html.ok_or("HTML内容为空")?;
                // 只剥 CF_HTML 头、保留 <head>：html_content 与 raw 走同一套清洗，
                // 若按 StartFragment/EndFragment 只取 fragment，<style> 会整块丢失
                let html = sanitize_clipboard_document(&raw_html);
                // 单张超大内嵌图先单独剔除，避免它把整条文档推到全局截断点
                let html = strip_oversized_data_images(&html, MAX_STORED_HTML_BYTES);
                let html = limit_html_bytes(&html, MAX_STORED_HTML_BYTES);
                let text = content.text.unwrap_or_else(|| strip_html(&html));
                
                let mut ct = ContentType::new("rich_text");
                
                if is_url(&text) {
                    ct.add_type("link");
                } else if contains_links(&text) {
                    ct.add_type("link");
                }
                
                let (processed_html, image_ids) = process_html_images(&html)?;
                let processed_html = limit_html_bytes(&processed_html, MAX_STORED_HTML_BYTES);
                let clipboard_image_id = content
                    .image_path
                    .as_deref()
                    .and_then(extract_image_id_from_path);
                if clipboard_image_id.is_some() {
                    ct.add_type("image");
                }
                let merged_image_ids =
                    merge_image_ids_prefer_clipboard_image(clipboard_image_id, image_ids);
                let image_id = if merged_image_ids.is_empty() {
                    None
                } else {
                    Some(merged_image_ids.join(","))
                };
                
            Ok(ProcessedContent {
                content: text,
                html_content: Some(processed_html),
                content_type: ct.to_db_string(),
                image_id,
                source_app,
                source_icon_hash,
                raw_formats,
            })
        }
        
        // 文件路径处理
        CaptureType::Files => {
            let files = content.files.ok_or("文件列表为空")?;
            
            // 获取文件详细信息
            let file_infos = collect_file_info(&files)?;
            
            // 序列化为JSON格式
            let file_data = FileClipboardData {
                files: file_infos.clone(),
                operation: "copy".to_string(),
            };
            
            let json_str = serde_json::to_string(&file_data)
                .map_err(|e| format!("序列化文件信息失败: {}", e))?;
            
            let ct = if file_infos.len() == 1 && crate::utils::is_image_file(&file_infos[0].path) {
                ContentType::new("image")
            } else {
                ContentType::new("file")
            };
            let image_id = if file_infos.len() == 1 && ct.to_db_string() == "image" {
                extract_image_id_from_path(&file_infos[0].path)
            } else { None };
            
            Ok(ProcessedContent {
                content: format!("files:{}", json_str),
                html_content: None,
                content_type: ct.to_db_string(),
                image_id,
                source_app,
                source_icon_hash,
                raw_formats,
            })
        }
    }
}

// 获取剪贴板来源信息
fn get_source_info() -> (Option<String>, Option<String>) {
    #[cfg(target_os = "windows")]
    {
        let source = crate::services::system::get_clipboard_source();
        if source.process_name.is_empty() {
            return (None, None);
        }

        let icon_hash = if !source.process_path.is_empty() {
            crate::utils::icon::save_app_icon(&source.process_path)
        } else {
            None
        };
        
        (Some(source.process_name), icon_hash)
    }
    
    #[cfg(not(target_os = "windows"))]
    {
        (None, None)
    }
}

// 收集文件信息
fn collect_file_info(file_paths: &[String]) -> Result<Vec<FileInfo>, String> {
    let mut file_infos = Vec::new();
    let data_dir = crate::services::get_data_directory().ok();
    
    for path_str in file_paths {
        let path = Path::new(path_str);
        
        let (actual_path, stored_path) = if let Some(ref data_dir) = data_dir {
            if path_str.starts_with("clipboard_images/") || path_str.starts_with("clipboard_images\\") 
                || path_str.starts_with("image_library/") || path_str.starts_with("image_library\\")
                || path_str.starts_with("pin_images/") || path_str.starts_with("pin_images\\") {
                let full_path = data_dir.join(path_str);
                (full_path.to_string_lossy().to_string(), path_str.clone())
            } 
            else if path.starts_with(data_dir) {
                if let Ok(relative) = path.strip_prefix(data_dir) {
                    let relative_str = relative.to_string_lossy().to_string().replace('\\', "/");
                    (path_str.clone(), relative_str)
                } else {
                    (path_str.clone(), path_str.clone())
                }
            } else {
                (path_str.clone(), path_str.clone())
            }
        } else {
            (path_str.clone(), path_str.clone())
        };
        
        let path = Path::new(&actual_path);
        
        // 获取文件元数据
        let metadata = fs::metadata(path)
            .map_err(|e| format!("无法读取文件信息 {}: {}", actual_path, e))?;
        
        // 提取文件名
        let name = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("未知文件")
            .to_string();
        
        // 获取文件类型（扩展名）
        let file_type = if metadata.is_dir() {
            "folder".to_string()
        } else {
            path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_uppercase())
                .unwrap_or_else(|| "文件".to_string())
        };
        
        // 获取文件图标
        let icon_data = crate::utils::icon::get_file_icon_base64(&actual_path);
        
        let (width, height) = if crate::utils::is_image_file(&actual_path) {
            crate::utils::get_image_dimensions(&actual_path)
                .map(|(w, h)| (Some(w), Some(h)))
                .unwrap_or((None, None))
        } else {
            (None, None)
        };
        
        file_infos.push(FileInfo {
            path: stored_path,
            name,
            size: metadata.len(),
            is_directory: metadata.is_dir(),
            icon_data,
            file_type,
            width,
            height,
        });
    }
    
    Ok(file_infos)
}

// 检测字符串是否是URL
fn is_url(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("http://") || 
    trimmed.starts_with("https://") ||
    trimmed.starts_with("ftp://") ||
    trimmed.starts_with("www.")
}

// 检测文本中是否包含链接
fn contains_links(text: &str) -> bool {
    let url_regex = Regex::new(r#"(?i)\b(https?://|ftp://|www\.)[^\s<>"]+\b"#).unwrap();
    url_regex.is_match(text)
}

// 从HTML中提取纯文本
fn strip_html(html: &str) -> String {
    // html_content 现在保留 <head>/<style>，提取纯文本时先把样式表整块去掉，避免 CSS 混进正文
    let style_regex = Regex::new(r"(?is)<(style|head)\b[^>]*>.*?</(style|head)>").unwrap();
    let tag_regex = Regex::new(r"<[^>]*>").unwrap();
    let entity_regex = Regex::new(r"&[a-zA-Z]+;").unwrap();
    
    let mut text = style_regex.replace_all(html, " ").to_string();
    text = tag_regex.replace_all(&text, " ").to_string();
    text = entity_regex.replace_all(&text, " ").to_string();
    
    // 清理多余的空白
    let whitespace_regex = Regex::new(r"\s+").unwrap();
    whitespace_regex.replace_all(&text, " ").trim().to_string()
}

// 处理HTML中的图片
fn process_html_images(html: &str) -> Result<(String, Vec<String>), String> {
    use regex::Regex;
    
    let mut processed_html = html.to_string();
    let mut image_ids = Vec::new();
    
    if let Ok(re) = Regex::new(r#"(<img\b[^>]*?\bsrc\s*=\s*")([^"]+)(")"#) {
        processed_html = re.replace_all(&processed_html, |caps: &regex::Captures| {
            let full_tag = &caps[0];
            let src = &caps[2];
            
            if full_tag.contains("data-image-id") {
                return full_tag.to_string();
            }
            
            if let Some(image_id) = try_save_image_from_url(src) {
                image_ids.push(image_id.clone());
                // 在 <img 后插入 data-image-id 属性
                full_tag.replacen("<img", &format!(r#"<img data-image-id="{}""#, image_id), 1)
            } else {
                full_tag.to_string()
            }
        }).to_string();
    }
    
    if let Ok(re) = Regex::new(r#"(<img\b[^>]*?\bsrc\s*=\s*')([^']+)(')"#) {
        processed_html = re.replace_all(&processed_html, |caps: &regex::Captures| {
            let full_tag = &caps[0];
            let src = &caps[2];
            
            if full_tag.contains("data-image-id") {
                return full_tag.to_string();
            }
            
            if let Some(image_id) = try_save_image_from_url(src) {
                if !image_ids.contains(&image_id) {
                    image_ids.push(image_id.clone());
                }
                full_tag.replacen("<img", &format!(r#"<img data-image-id="{}""#, image_id), 1)
            } else {
                full_tag.to_string()
            }
        }).to_string();
    }
    
    Ok((processed_html, image_ids))
}

fn merge_image_ids_prefer_clipboard_image(
    clipboard_image_id: Option<String>,
    html_image_ids: Vec<String>,
) -> Vec<String> {
    let mut merged = Vec::new();

    if let Some(image_id) = clipboard_image_id {
        if !image_id.trim().is_empty() {
            merged.push(image_id);
        }
    }

    for image_id in html_image_ids {
        if image_id.trim().is_empty() || merged.contains(&image_id) {
            continue;
        }
        merged.push(image_id);
    }

    merged
}

// 尝试从URL保存图片并返回图片ID
fn try_save_image_from_url(src: &str) -> Option<String> {
    let src = src.trim();
    
    if src.is_empty() || src == "about:blank" || src.contains("/none.") {
        return None;
    }
    
    match fetch_image_data(src) {
        Ok(image_data) => {
            match save_image_as_file(&image_data) {
                Ok(file_path) => {
                    std::path::Path::new(&file_path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                }
                Err(_) => None,
            }
        }
        Err(_) => None,
    }
}


// 获取图片数据（网络或本地）
fn fetch_image_data(src: &str) -> Result<Vec<u8>, String> {
    let src = if src.starts_with("//") {
        format!("https:{}", src)
    } else {
        src.to_string()
    };
    
    if src.starts_with("http://") || src.starts_with("https://") {
        Err("离线版不支持抓取网络图片".to_string())
    } else if src.starts_with("data:image/") {
        parse_data_url(&src)
    } else if src.starts_with("file://") {
        let path = src.trim_start_matches("file://");
        let path = path.trim_start_matches('/');
        std::fs::read(path).map_err(|e| format!("读取本地图片失败 [{}]: {}", path, e))
    } else if std::path::Path::new(&src).exists() {
        std::fs::read(&src).map_err(|e| format!("读取图片失败 [{}]: {}", src, e))
    } else {
        Err(format!("不支持的图片源或文件不存在: {}", src))
    }
}

// 解析Data URL
fn parse_data_url(data_url: &str) -> Result<Vec<u8>, String> {
    use base64::{Engine as _, engine::general_purpose};

    let parts: Vec<&str> = data_url.splitn(2, ',').collect();
    if parts.len() != 2 {
        return Err("无效的Data URL格式".to_string());
    }
    
    let data = parts[1];
    general_purpose::STANDARD.decode(data).map_err(|e| format!("Base64解码失败: {}", e))
}

// 保存图片到本地文件和数据库，返回图片ID
fn save_image_as_file(image_data: &[u8]) -> Result<String, String> {
    // 解码图片
    let cursor = Cursor::new(image_data);
    let img = image::ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|e| format!("图片格式识别失败: {}", e))?
        .decode()
        .map_err(|e| format!("图片解码失败: {}", e))?;
    
    // 编码为PNG格式
    let mut png_data = Vec::new();
    {
        let mut cursor = Cursor::new(&mut png_data);
        img.write_to(&mut cursor, ImageFormat::Png)
            .map_err(|e| format!("PNG编码失败: {}", e))?;
    }
    
    // 生成图片ID
    let image_id = calculate_image_id(&png_data);
    
    // 保存到文件系统
    use crate::services::get_data_directory;
    use std::fs;
    
    let data_dir = get_data_directory()?;
    let images_dir = data_dir.join("clipboard_images");
    
    if !images_dir.exists() {
        fs::create_dir_all(&images_dir)
            .map_err(|e| format!("创建图片目录失败: {}", e))?;
    }
    
    let image_path = images_dir.join(format!("{}.png", image_id));
    
    if !image_path.exists() {
        fs::write(&image_path, png_data)
            .map_err(|e| format!("保存图片文件失败: {}", e))?;
    }
    
    image_path.to_str()
        .ok_or("文件路径转换失败".to_string())
        .map(|s| s.to_string())
}

// 根据图片数据计算图片ID
fn calculate_image_id(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let hash = format!("{:x}", hasher.finalize());
    hash[..16].to_string()
}

fn extract_image_id_from_path(path_str: &str) -> Option<String> {
    if path_str.starts_with("clipboard_images/") || path_str.starts_with("clipboard_images\\") {
        let p = std::path::Path::new(path_str);
        return p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::merge_image_ids_prefer_clipboard_image;

    #[test]
    fn clipboard_image_id_should_be_first_for_rich_text_images() {
        let merged = merge_image_ids_prefer_clipboard_image(
            Some("region_snapshot".to_string()),
            vec!["inline_a".to_string(), "inline_b".to_string()],
        );

        assert_eq!(merged, vec!["region_snapshot", "inline_a", "inline_b"]);
    }

    #[test]
    fn clipboard_image_id_should_not_be_duplicated() {
        let merged = merge_image_ids_prefer_clipboard_image(
            Some("region_snapshot".to_string()),
            vec!["inline_a".to_string(), "region_snapshot".to_string()],
        );

        assert_eq!(merged, vec!["region_snapshot", "inline_a"]);
    }

    use super::{process_content, ClipboardContent, CaptureType, ClipboardDataSeed, MAX_STORED_HTML_BYTES};

    // 不含 data:image（避免测试写入真实 clipboard_images 目录）；外链图片离线不会被下载
    const SAMPLE_BODY: &str = r#"<p style="color: rgb(255, 0, 0); font-weight: bold; font-size: 18px; font-family: 'Microsoft YaHei'; line-height: 1.8; text-align: center; position: fixed; top: 0; z-index: 9999">第一段 <span style="font-style: italic; text-decoration: underline; background-color: #ffff00">斜体下划线</span><br>第二行 &amp; 实体</p><a href="https://example.com/a?b=1&amp;c=2" onclick="alert(1)">安全链接</a><a href="javascript:alert(1)">危险链接</a><a href="&#106;avascript:alert(1)">实体混淆</a><img src="https://example.com/x.png" onerror="alert(1)" width="40" height="40"><div style="white-space: pre-wrap; margin-left: 12px">保留空白</div><iframe src="https://evil.example"></iframe><object data="x"></object><script>alert(2)</script>"#;

    // 采集入库核对：html_content 与 raw "HTML Format" 行都应已净化，且同时保留 <style> 与格式
    #[test]
    fn process_content_sanitizes_html_and_raw_html_format() {
        let word_style = r#"p.MsoNormal {font-family:"Microsoft YaHei"; font-size:11.0pt; line-height:1.5} @page {margin: 2cm;} p.Bad {position:fixed; z-index:9999} @import url("http://blocked.example/x.css"); p.Local {background:url(file:///C:/Users/x/a.png)} p.Rel {background:url(./img/a.png)} p.Remote {background:url(http://evil/a.png)}"#;
        let doc = format!(
            "<!DOCTYPE html>\n<html><head><style>{}</style><script>alert(0)</script></head><body>\n<!--StartFragment-->{}\n<!--EndFragment-->\n</body></html>",
            word_style, SAMPLE_BODY
        );
        let header = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
        let start_html = header.len();
        let end_html = start_html + doc.len();
        let start_fragment = start_html + doc.find("<!--StartFragment-->").unwrap();
        let end_fragment = start_html + doc.find("<!--EndFragment-->").unwrap();
        let cf_html = format!(
            "Version:0.9\r\nStartHTML:{:010}\r\nEndHTML:{:010}\r\nStartFragment:{:010}\r\nEndFragment:{:010}\r\n{}",
            start_html, end_html, start_fragment, end_fragment, doc
        );

        let content = ClipboardContent {
            content_type: CaptureType::RichText,
            text: Some("第一段\n第二行".to_string()),
            html: Some(doc),
            files: None,
            image_path: None,
            raw_formats: vec![ClipboardDataSeed {
                format_name: "HTML Format".to_string(),
                raw_data: cf_html.into_bytes(),
                is_primary: true,
                format_order: 0,
            }],
        };

        let processed = process_content(content).expect("process_content 失败");
        let html = processed.html_content.expect("html_content 为空");

        for keep in [
            "<style>",
            "p.MsoNormal",
            "line-height:1.5",
            "<br",
            "<img src=\"https://example.com/x.png\"",
            "href=\"https://example.com/a?b=1&amp;c=2\"",
            "color: rgb(255, 0, 0)",
            "font-weight: bold",
            "font-size: 18px",
            "font-family: 'Microsoft YaHei'",
            "line-height: 1.8",
            "text-align: center",
            "background-color: #ffff00",
            "text-decoration: underline",
            "font-style: italic",
            "white-space: pre-wrap",
            "margin-left: 12px",
            // 保真优先：本地 file:/// 与相对路径、http 外链图片的 url() 都要保留
            "background:url(file:///C:/Users/x/a.png)",
            "background:url(./img/a.png)",
            "background:url(http://evil/a.png)",
        ] {
            assert!(html.contains(keep), "html_content 缺少格式属性: {}", keep);
        }
        for forbidden in [
            "<script", "<iframe", "<object", "onerror=", "onclick=",
            "@import", "http://blocked.example", "expression(", "alert(",
            "javascript:", "vbscript:", "position:", "z-index:", "top:",
        ] {
            assert!(!html.contains(forbidden), "html_content 危险内容未清除: {}", forbidden);
        }
        assert!(html.len() <= MAX_STORED_HTML_BYTES, "html_content 超过体积上限");

        let raw = processed
            .raw_formats
            .iter()
            .find(|item| item.format_name == "HTML Format")
            .expect("raw HTML Format 行被丢弃");
        assert!(raw.raw_data.len() <= MAX_STORED_HTML_BYTES, "raw 行超过体积上限");
        let raw_text = String::from_utf8(raw.raw_data.clone()).unwrap();

        assert!(raw_text.contains("Version:0.9"));
        // html_content 与 raw 必须同时保留清洗后的 <style>（留 MsoNormal/行距，清危险声明）
        assert!(raw_text.contains("<style>"), "raw 缺少 style: {}", raw_text);
        assert!(raw_text.contains("p.MsoNormal"), "raw 缺少 MsoNormal: {}", raw_text);
        assert!(raw_text.contains("line-height:1.5"), "raw 缺少 line-height: {}", raw_text);
        // raw 行同样保留本地/相对/外链 url()，与 html_content 口径一致
        assert!(raw_text.contains("background:url(file:///C:/Users/x/a.png)"), "raw 缺少 file url: {}", raw_text);
        assert!(raw_text.contains("background:url(./img/a.png)"), "raw 缺少相对路径 url: {}", raw_text);
        assert!(raw_text.contains("background:url(http://evil/a.png)"), "raw 缺少外链 url: {}", raw_text);
        assert!(raw_text.contains("<br"));
        assert!(raw_text.contains("href=\"https://example.com/a?b=1&amp;c=2\""));
        for forbidden in [
            "<script", "<object", "onerror=", "javascript:",
            "@import", "http://blocked.example", "position:", "z-index:",
        ] {
            assert!(!raw_text.contains(forbidden), "raw 危险内容未清除: {}", forbidden);
        }

        let parse_offset = |key: &str| -> usize {
            let idx = raw_text.find(key).unwrap() + key.len();
            raw_text[idx..].chars().take(10).collect::<String>().parse().unwrap()
        };
        let start_html = parse_offset("StartHTML:");
        let start_fragment = parse_offset("StartFragment:");
        let end_fragment = parse_offset("EndFragment:");
        assert_eq!(&raw_text[start_html..start_html + 1], "<");
        assert_eq!(
            &raw_text[start_fragment..start_fragment + "<!--StartFragment-->".len()],
            "<!--StartFragment-->"
        );
        assert_eq!(
            &raw_text[end_fragment..end_fragment + "<!--EndFragment-->".len()],
            "<!--EndFragment-->"
        );

        let out = std::env::temp_dir().join("qc_processor_sanitized.txt");
        std::fs::write(&out, html.as_bytes()).unwrap();
        eprintln!("html_content {} 字节, raw {} 字节 -> {:?}", html.len(), raw.raw_data.len(), out);
    }
}


