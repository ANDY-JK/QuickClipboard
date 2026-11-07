// 采集期 HTML 净化与体积约束。
//
// 只在剪贴板内容入库前执行一次（processor::process_content），粘贴链路仍按清洗后的字节原样写回，
// 避免每次粘贴都重新清洗数 MB 的 HTML。
//
// 保留（格式保真优先）：
// - 全部文本节点、<br>/<p>/<div> 等块级换行结构、white-space: pre-wrap/pre 等排版样式
// - 颜色/背景色/字号/字体/对齐/行高/加粗/斜体/下划线等内联样式与常规属性
// - <img src="data:image/*;base64,...">（原样保留，粘贴后仍能显示）
// - 外链 <img src="http(s)://...">（不下载、不改写、不替换）
// - http/https/mailto/tel/ftp 等安全链接
//
// 剥离（安全第二）：
// - <script>/<iframe>/<object>/<embed>/<applet>/<frame>/<frameset> 及其内容
// - <style> 改为「保留但清洗」：Word/Outlook 的 MsoNormal、@page 等规则要留下，
//   但 @import、expression()、javascript:、position/z-index 等危险声明会被逐条剔除
// - on* 事件属性
// - javascript:/vbscript:/data:text/html 等危险 URI（含实体编码混淆）
// - position/inset/top/right/bottom/left/z-index/behavior/-moz-binding 等会把元素抛出文档流的样式
// - expression() 与样式里的 javascript:/vbscript:
// - CSS 里 url() 放行 http(s)、file:///、data:image 与相对路径（Word 本地图片靠它，剥了会变空白框）；
//   只剥 url() 里会执行代码的 scheme：javascript:/vbscript:/非图片 data:
// - 仍然剥离（不属于格式保真范畴）：javascript:/vbscript:/data:text/html/expression()/@import 属于
//   「贴到网页会执行代码」；position/inset/top/right/bottom/left/z-index/behavior 属于「贴出去元素浮起来、
//   排版整体走形」

use crate::services::database::ClipboardDataSeed;
use crate::utils::cf_html::{generate_cf_html, normalize_clipboard_html};

const BLOCKED_ELEMENTS: &[&str] = &[
    "script",
    "iframe",
    "object",
    "embed",
    "applet",
    "frame",
    "frameset",
];

const DROPPED_EMPTY_ELEMENTS: &[&str] = &["base", "meta", "link"];

const BLOCKED_STYLE_PROPERTIES: &[&str] = &[
    "position",
    "inset",
    "inset-block",
    "inset-block-start",
    "inset-block-end",
    "inset-inline",
    "inset-inline-start",
    "inset-inline-end",
    "top",
    "right",
    "bottom",
    "left",
    "z-index",
    "behavior",
    "-moz-binding",
];

#[derive(Debug, Clone)]
struct Attr {
    name: String,
    value: Option<String>,
}

#[derive(Debug, Clone)]
struct TagInfo {
    name_raw: String,
    name_lower: String,
    is_closing: bool,
    self_closing: bool,
    attrs: Vec<Attr>,
}

// 清洗整段 HTML（保留注释，因为 CF_HTML 的 StartFragment/EndFragment 就是注释标记）
pub fn sanitize_clipboard_html(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }

    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0usize;

    while i < len {
        let lt = match bytes[i..].iter().position(|&b| b == b'<') {
            Some(pos) => i + pos,
            None => {
                out.push_str(&input[i..]);
                break;
            }
        };

        if lt > i {
            out.push_str(&input[i..lt]);
        }

        let rest = &input[lt..];

        // 注释：原样保留（不能丢 StartFragment/EndFragment）
        if rest.starts_with("<!--") {
            match rest[4..].find("-->") {
                Some(pos) => {
                    let end = lt + 4 + pos + 3;
                    out.push_str(&input[lt..end]);
                    i = end;
                }
                None => break,
            }
            continue;
        }

        // DOCTYPE / 处理指令：丢弃标签本身，CF_HTML 生成时会补 DOCTYPE
        if rest.starts_with("<!") || rest.starts_with("<?") {
            match find_tag_end(input, lt) {
                Some(end) => i = end,
                None => break,
            }
            continue;
        }

        // '<' 后面不是字母或 '/' 时按文本处理（例如 "1 < 2"、"a <3"）
        let next = rest.as_bytes().get(1).copied().unwrap_or(b'\0');
        if !(next.is_ascii_alphabetic() || next == b'/') {
            out.push('<');
            i = lt + 1;
            continue;
        }

        let end = match find_tag_end(input, lt) {
            Some(end) => end,
            None => {
                // 未闭合的 '<' 会被 HTML 解析器当作文本，原样保留
                out.push_str(rest);
                break;
            }
        };

        let tag_text = &input[lt..end];
        let tag = match parse_tag(tag_text) {
            Some(tag) => tag,
            None => {
                out.push_str(tag_text);
                i = end;
                continue;
            }
        };

        // <style>：保留元素本身，但逐条清洗 CSS（Word/Outlook 依赖 MsoNormal/@page 等规则）
        if tag.name_lower == "style" {
            if tag.is_closing || tag.self_closing {
                i = end;
                continue;
            }
            let (raw_css, after) = match find_closing_tag(input, end, "style") {
                Some(close_start) => match find_tag_end(input, close_start) {
                    Some(close_end) => (&input[end..close_start], close_end),
                    None => (&input[end..], len),
                },
                None => (&input[end..], len),
            };
            let css = sanitize_css_stylesheet(raw_css);
            if !css.trim().is_empty() {
                let attrs = sanitize_attributes(&tag.attrs);
                out.push_str(&render_tag(&tag.name_raw, &attrs, false, false));
                out.push_str(&css);
                out.push_str("</style>");
            }
            i = after;
            continue;
        }

        // 危险元素：连内容一起丢弃
        if BLOCKED_ELEMENTS.contains(&tag.name_lower.as_str()) {
            if tag.is_closing || tag.self_closing {
                i = end;
                continue;
            }
            match find_closing_tag(input, end, &tag.name_lower) {
                Some(close_start) => match find_tag_end(input, close_start) {
                    Some(close_end) => i = close_end,
                    None => break,
                },
                // 没有闭合标签：丢弃余下内容，避免把脚本正文当正文
                None => break,
            }
            continue;
        }

        // 只影响文档级行为的空元素：直接丢弃标签
        if DROPPED_EMPTY_ELEMENTS.contains(&tag.name_lower.as_str()) {
            i = end;
            continue;
        }

        if tag.is_closing {
            out.push_str(&render_tag(&tag.name_raw, &[], true, false));
        } else {
            let sanitized_attrs = sanitize_attributes(&tag.attrs);
            out.push_str(&render_tag(&tag.name_raw, &sanitized_attrs, false, tag.self_closing));
        }

        i = end;
    }

    out
}

// 清洗整份剪贴板文档：只去掉 CF_HTML 头，保留 <head>（含 <style> 样式表），
// 避免 html_content 走 fragment 提取把 Word 的 MsoNormal/行距规则整块丢掉
pub fn sanitize_clipboard_document(input: &str) -> String {
    let document = strip_cf_html_header(input);
    sanitize_clipboard_html(&document)
}

// 体积上限：超限时在最后一个完整标签边界截断，保留前段完整结构。
// `<style>` / `<script>` 的正文属于 raw text，CSS 里出现 `<` 时不能按标签解析；
// 若截断点落进 raw text 块内部，整块丢弃（回退到该块起始标签之前），
// 避免输出未闭合的 `<style>` 把后半段文档样式全吞掉。
pub fn limit_html_bytes(html: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || html.len() <= max_bytes {
        return html.to_string();
    }

    let mut limit = max_bytes.min(html.len());
    while limit > 0 && !html.is_char_boundary(limit) {
        limit -= 1;
    }

    let head = &html[..limit];
    let bytes = head.as_bytes();
    let len = bytes.len();

    // 最近一个「完整收尾」的位置：完整标签结束、注释结束或 raw text 块闭合之后。
    let mut boundary = 0usize;
    // 当前正在扫描的 raw text 元素及其起始标签位置。
    let mut raw_tag: Option<String> = None;
    let mut raw_open_start = 0usize;
    let mut i = 0usize;

    while i < len {
        if let Some(name) = raw_tag.as_deref() {
            match find_closing_tag(head, i, name) {
                Some(close_start) => match find_tag_end(head, close_start) {
                    Some(close_end) => {
                        boundary = close_end;
                        raw_tag = None;
                        i = close_end;
                        continue;
                    }
                    // 闭合标签本身被截断：整块丢弃。
                    None => break,
                },
                // 在 head 内找不到闭合标签：截断在 raw text 块中间。
                None => break,
            }
        }

        let lt = match bytes[i..].iter().position(|&b| b == b'<') {
            Some(pos) => i + pos,
            None => break,
        };

        if lt + 1 >= len {
            break;
        }
        let next = bytes[lt + 1];

        // 注释（含 CF_HTML 的 StartFragment/EndFragment 标记）：未闭合则丢弃整段注释。
        if head[lt..].starts_with("<!--") {
            match head[lt + 4..].find("-->") {
                Some(pos) => {
                    let end = lt + 4 + pos + 3;
                    boundary = end;
                    i = end;
                }
                None => break,
            }
            continue;
        }

        // DOCTYPE / 处理指令：按标签边界处理。
        if next == b'!' || next == b'?' {
            match find_tag_end(head, lt) {
                Some(end) => {
                    boundary = end;
                    i = end;
                }
                None => break,
            }
            continue;
        }

        // 文本里的裸 '<'（如 "1 < 2"）：不是标签，跳过。
        if !(next.is_ascii_alphabetic() || next == b'/') {
            i = lt + 1;
            continue;
        }

        let end = match find_tag_end(head, lt) {
            Some(end) => end,
            // 标签本身被截断：停在最近一个完整边界。
            None => break,
        };

        let tag = match parse_tag(&head[lt..end]) {
            Some(tag) => tag,
            None => {
                i = end;
                continue;
            }
        };

        if tag.is_closing || tag.self_closing {
            boundary = end;
            i = end;
            continue;
        }

        if tag.name_lower == "style" || tag.name_lower == "script" {
            raw_tag = Some(tag.name_lower);
            raw_open_start = lt;
            i = end;
            continue;
        }

        boundary = end;
        i = end;
    }

    // raw text 块被截断时，raw_open_start 可能为 0（整个文档以 style 开头），
    // 这种情况下要返回空串而不是误把半个 style 留下。
    if raw_tag.is_some() {
        return head[..raw_open_start].to_string();
    }
    if boundary == 0 {
        // 没有任何完整结构可保留：维持旧行为，返回按字符边界裁剪的前缀。
        head.to_string()
    } else {
        head[..boundary].to_string()
    }
}

// 单张内嵌图片超过上限时只丢弃该 <img> 元素，保留其余结构与文字，
// 避免一张超大图把整条文档推到全局截断点、连带丢掉后面的内容。
pub fn strip_oversized_data_images(html: &str, max_image_bytes: usize) -> String {
    if max_image_bytes == 0 || !html.contains("data:image") {
        return html.to_string();
    }

    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0usize;

    while i < len {
        let lt = match bytes[i..].iter().position(|&b| b == b'<') {
            Some(pos) => i + pos,
            None => {
                out.push_str(&html[i..]);
                break;
            }
        };

        let next = bytes.get(lt + 1).copied().unwrap_or(b'\0');
        if !(next.is_ascii_alphabetic() || next == b'/') {
            out.push_str(&html[i..=lt]);
            i = lt + 1;
            continue;
        }

        if lt > i {
            out.push_str(&html[i..lt]);
        }

        let Some(end) = find_tag_end(html, lt) else {
            out.push_str(&html[lt..]);
            break;
        };

        let drop_tag = parse_tag(&html[lt..end]).is_some_and(|tag| {
            if tag.is_closing || tag.name_lower != "img" {
                return false;
            }
            tag.attrs
                .iter()
                .find(|attr| attr.name.eq_ignore_ascii_case("src"))
                .and_then(|attr| attr.value.as_deref())
                .is_some_and(|src| {
                    let lower = src.trim_start().to_ascii_lowercase();
                    lower.starts_with("data:image/") && src.len() > max_image_bytes
                })
        });

        if !drop_tag {
            out.push_str(&html[lt..end]);
        }
        i = end;
    }

    out
}

// 同步清洗 raw 的 "HTML Format" 行：用清洗后的文档重新生成 CF_HTML；
// 无法解析或清洗后为空时丢弃该行，让粘贴走 html_content 的 CF_HTML 兜底。
pub fn sanitize_raw_html_formats(raw_formats: &mut Vec<ClipboardDataSeed>, max_bytes: usize) {
    let mut rebuilt = Vec::with_capacity(raw_formats.len());

    for item in raw_formats.drain(..) {
        if item.format_name != "HTML Format" {
            rebuilt.push(item);
            continue;
        }

        match rebuild_html_raw_data(&item.raw_data, max_bytes) {
            Some(bytes) => rebuilt.push(ClipboardDataSeed {
                raw_data: bytes,
                ..item
            }),
            None => {
                eprintln!("HTML Format 原始格式无法清洗，已丢弃该格式行");
            }
        }
    }

    *raw_formats = rebuilt;
}

fn rebuild_html_raw_data(raw: &[u8], max_bytes: usize) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(raw).ok()?;
    let document = strip_cf_html_header(text);
    let sanitized = sanitize_clipboard_html(&document);
    if sanitized.trim().is_empty() {
        return None;
    }
    let sanitized = strip_oversized_data_images(&sanitized, max_bytes);
    let limited = limit_html_bytes(&sanitized, max_bytes);
    let healed = ensure_fragment_markers(&limited);
    Some(generate_cf_html(&healed).into_bytes())
}

// 去掉 "Version:0.9 / StartHTML:..." 头，保留文档本体（含 head 里的排版属性）
fn strip_cf_html_header(text: &str) -> String {
    if let Some(pos) = find_ignore_case(text, "<html") {
        return text[pos..].to_string();
    }
    if let Some(pos) = find_ignore_case(text, "<!doctype") {
        return text[pos..].to_string();
    }
    if let Some(pos) = find_ignore_case(text, "<body") {
        return text[pos..].to_string();
    }
    normalize_clipboard_html(text)
}

// 保证文档里有 StartFragment/EndFragment 标记，CF_HTML 偏移量才能算对
fn ensure_fragment_markers(document: &str) -> String {
    if document.contains("<!--StartFragment-->") && document.contains("<!--EndFragment-->") {
        return document.to_string();
    }

    let Some(body_pos) = find_ignore_case(document, "<body") else {
        return document.to_string();
    };
    let Some(body_end) = find_tag_end(document, body_pos) else {
        return document.to_string();
    };

    let mut out = String::with_capacity(document.len() + 48);
    out.push_str(&document[..body_end]);
    out.push_str("\n<!--StartFragment-->");

    let rest = &document[body_end..];
    match find_ignore_case(rest, "</body") {
        Some(close) => {
            out.push_str(&rest[..close]);
            out.push_str("\n<!--EndFragment-->");
            out.push_str(&rest[close..]);
        }
        None => {
            out.push_str(rest);
            out.push_str("<!--EndFragment-->");
        }
    }

    out
}

fn find_tag_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = start + 1;

    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => {
                if b == b'"' || b == b'\'' {
                    quote = Some(b);
                } else if b == b'>' {
                    return Some(i + 1);
                }
            }
        }
        i += 1;
    }

    None
}

fn find_closing_tag(input: &str, from: usize, name: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let name_bytes = name.as_bytes();
    let mut i = from;

    while i + 1 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] == b'/' {
            let rest = &input[i + 2..];
            if rest.len() >= name_bytes.len()
                && rest.as_bytes()[..name_bytes.len()].eq_ignore_ascii_case(name_bytes)
            {
                let after = rest.as_bytes().get(name_bytes.len()).copied();
                if matches!(
                    after,
                    Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'/')
                ) {
                    return Some(i);
                }
            }
        }
        i += 1;
    }

    None
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }

    let mut i = 0usize;
    while i + n.len() <= h.len() {
        if h[i..i + n.len()].eq_ignore_ascii_case(n) {
            return Some(i);
        }
        i += 1;
    }

    None
}

fn parse_tag(tag_text: &str) -> Option<TagInfo> {
    let bytes = tag_text.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'<' || bytes[bytes.len() - 1] != b'>' {
        return None;
    }

    let mut i = 1usize;
    let mut is_closing = false;
    if bytes[i] == b'/' {
        is_closing = true;
        i += 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }

    let name_start = i;
    while i < bytes.len()
        && !bytes[i].is_ascii_whitespace()
        && bytes[i] != b'>'
        && bytes[i] != b'/'
    {
        i += 1;
    }
    if i <= name_start {
        return None;
    }
    let name_raw = tag_text[name_start..i].to_string();
    let name_lower = name_raw.to_ascii_lowercase();

    let mut attrs: Vec<Attr> = Vec::new();
    let mut self_closing = false;

    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' {
            break;
        }
        if bytes[i] == b'/' {
            self_closing = true;
            i += 1;
            continue;
        }

        let attr_start = i;
        while i < bytes.len()
            && bytes[i] != b'>'
            && bytes[i] != b'='
            && !bytes[i].is_ascii_whitespace()
        {
            i += 1;
        }
        let attr_end = i;
        if attr_end <= attr_start {
            i += 1;
            continue;
        }
        let attr_name = tag_text[attr_start..attr_end].to_string();

        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        let mut value = None;
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() {
                if bytes[i] == b'"' || bytes[i] == b'\'' {
                    let quote = bytes[i];
                    i += 1;
                    let value_start = i;
                    while i < bytes.len() && bytes[i] != quote {
                        i += 1;
                    }
                    value = Some(tag_text[value_start..i].to_string());
                    if i < bytes.len() {
                        i += 1;
                    }
                } else {
                    let value_start = i;
                    while i < bytes.len()
                        && bytes[i] != b'>'
                        && !bytes[i].is_ascii_whitespace()
                    {
                        i += 1;
                    }
                    value = Some(tag_text[value_start..i].to_string());
                }
            }
        }

        attrs.push(Attr {
            name: attr_name,
            value,
        });
    }

    Some(TagInfo {
        name_raw,
        name_lower,
        is_closing,
        self_closing,
        attrs,
    })
}

fn render_tag(
    name_raw: &str,
    attrs: &[Attr],
    is_closing: bool,
    self_closing: bool,
) -> String {
    let mut out = String::with_capacity(name_raw.len() + 8);
    out.push('<');
    if is_closing {
        out.push('/');
    }
    out.push_str(name_raw);
    for attr in attrs {
        out.push(' ');
        out.push_str(&attr.name);
        if let Some(value) = &attr.value {
            out.push_str("=\"");
            out.push_str(&escape_attr_value(value));
            out.push('"');
        }
    }
    if self_closing {
        out.push_str(" /");
    }
    out.push('>');
    out
}

fn escape_attr_value(value: &str) -> String {
    if value.contains('"') {
        value.replace('"', "&quot;")
    } else {
        value.to_string()
    }
}

fn sanitize_attributes(attrs: &[Attr]) -> Vec<Attr> {
    let mut result = Vec::with_capacity(attrs.len());

    for attr in attrs {
        let lower = attr.name.to_ascii_lowercase();

        // on* 事件属性
        if lower.starts_with("on") {
            continue;
        }

        match lower.as_str() {
            "style" => {
                if let Some(value) = attr.value.as_deref() {
                    let sanitized = sanitize_style(value);
                    if !sanitized.is_empty() {
                        result.push(Attr {
                            name: attr.name.clone(),
                            value: Some(sanitized),
                        });
                    }
                }
                continue;
            }
            "src" | "poster" | "background" => {
                if let Some(value) = attr.value.as_deref() {
                    if is_unsafe_image_url(value) {
                        continue;
                    }
                }
                result.push(attr.clone());
                continue;
            }
            "srcset" => {
                if let Some(value) = attr.value.as_deref() {
                    if is_unsafe_srcset(value) {
                        continue;
                    }
                }
                result.push(attr.clone());
                continue;
            }
            "href" | "xlink:href" | "action" | "formaction" | "cite" | "longdesc" => {
                if let Some(value) = attr.value.as_deref() {
                    if is_unsafe_link_url(value) {
                        continue;
                    }
                }
                result.push(attr.clone());
                continue;
            }
            _ => {}
        }

        result.push(attr.clone());
    }

    result
}

fn is_unsafe_image_url(value: &str) -> bool {
    let decoded = decode_entities(value);
    let lower = decoded.trim_start().to_ascii_lowercase();

    // data:image/* 原样保留；其余 data:（含 data:text/html）一律剥离
    if lower.starts_with("data:") {
        return !lower.starts_with("data:image/");
    }

    is_dangerous_scheme(&decoded)
}

fn is_unsafe_link_url(value: &str) -> bool {
    let decoded = decode_entities(value);
    let lower = decoded.trim_start().to_ascii_lowercase();

    if lower.starts_with("data:") {
        return true;
    }

    is_dangerous_scheme(&decoded)
}

fn is_unsafe_srcset(value: &str) -> bool {
    let decoded = decode_entities(value);
    let lower = decoded.to_ascii_lowercase();

    lower.contains("javascript:")
        || lower.contains("vbscript:")
        || lower.contains("data:text/html")
        || lower.contains("data:application")
        || lower.contains("expression(")
}

fn is_dangerous_scheme(value: &str) -> bool {
    let mut scheme = String::with_capacity(16);

    for c in value.chars().take(64) {
        if c == ':' {
            return matches!(scheme.as_str(), "javascript" | "vbscript");
        }
        // 浏览器解析 scheme 时会忽略空白与控制字符
        if c.is_whitespace() || (c as u32) < 0x20 {
            continue;
        }
        if c == '/' || c == '#' || c == '?' || c == '.' {
            return false;
        }
        if !(c.is_ascii_alphanumeric() || c == '+' || c == '-') {
            return false;
        }
        scheme.push(c.to_ascii_lowercase());
    }

    false
}

fn decode_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }

    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some((decoded, consumed)) = decode_entity(&value[i..]) {
                out.push_str(&decoded);
                i += consumed;
                continue;
            }
        }
        let ch = value[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

fn decode_entity(rest: &str) -> Option<(String, usize)> {
    if !rest.starts_with('&') {
        return None;
    }
    let semi = rest.find(';')?;
    if semi > 12 {
        return None;
    }

    let body = &rest[1..semi];
    let decoded = if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
        u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)?.to_string()
    } else if let Some(dec) = body.strip_prefix('#') {
        dec.parse::<u32>().ok().and_then(char::from_u32)?.to_string()
    } else {
        match body.to_ascii_lowercase().as_str() {
            "amp" => "&".to_string(),
            "colon" => ":".to_string(),
            "tab" => "\t".to_string(),
            "newline" => "\n".to_string(),
            "quot" => "\"".to_string(),
            "apos" => "'".to_string(),
            "lt" => "<".to_string(),
            "gt" => ">".to_string(),
            "nbsp" => " ".to_string(),
            _ => return None,
        }
    };

    Some((decoded, semi + 1))
}

fn sanitize_style(style: &str) -> String {
    // 属性值里的引号常见以实体形式出现（&quot;/&#34;/&#39;），实体自身的 ';' 会被误当声明分隔符，
    // 先把引号实体还原成真实引号，再做引号感知的声明切分
    let style = decode_css_quotes(style);
    // 注释里可能藏 expression()/危险 url，先整体剥掉注释
    let style = strip_css_comments(&style);
    sanitize_css_declarations(&style)
}

// 逐条清洗 CSS 声明；返回 None 表示该声明被剔除
fn sanitize_declaration(decl: &str) -> Option<String> {
    let decl = decl.trim();
    if decl.is_empty() {
        return None;
    }

    // 引号/括号未闭合 = 半截声明，整条丢弃，绝不输出残片
    if !is_balanced_css_text(decl) {
        return None;
    }

    let colon = decl.find(':')?;
    if decl[colon + 1..].trim().is_empty() {
        return None;
    }
    let raw_prop = unescape_css(decl[..colon].trim()).to_ascii_lowercase();
    let prop = strip_vendor_prefix(&raw_prop);
    if BLOCKED_STYLE_PROPERTIES.contains(&raw_prop.as_str())
        || BLOCKED_STYLE_PROPERTIES.contains(&prop.as_str())
    {
        return None;
    }

    // 输出仍用原声明；判危险时先解实体再解转义，防止 url(javascript&#58;...) 这类混淆漏过
    let lower = unescape_css(&decode_entities(&decl[colon + 1..])).to_ascii_lowercase();
    if lower.contains("expression(")
        || lower.contains("javascript:")
        || lower.contains("vbscript:")
        || lower.contains("data:text/html")
        || has_unsafe_css_url(&lower)
    {
        return None;
    }

    Some(decl.to_string())
}

// 引号与括号是否配对；不配对说明声明被截断，调用方整条丢弃
fn is_balanced_css_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut quote: Option<u8> = None;
    let mut depth = 0usize;
    let mut stray_close = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'(' => depth += 1,
                b')' => {
                    if depth == 0 {
                        stray_close = true;
                    } else {
                        depth -= 1;
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }

    quote.is_none() && depth == 0 && !stray_close
}

// 把属性值里的引号实体还原成真实引号，避免实体末尾 ';' 被当成声明分隔符
fn decode_css_quotes(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }

    const QUOTE_ENTITIES: &[(&str, &str)] = &[
        ("&quot;", "\""),
        ("&#34;", "\""),
        ("&#x22;", "\""),
        ("&apos;", "'"),
        ("&#39;", "'"),
        ("&#x27;", "'"),
    ];

    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'&' {
            let rest = &input[i..];
            if let Some((replacement, consumed)) = QUOTE_ENTITIES.iter().find_map(|(pat, rep)| {
                let pat_bytes = pat.as_bytes();
                if rest.len() >= pat_bytes.len()
                    && rest.as_bytes()[..pat_bytes.len()].eq_ignore_ascii_case(pat_bytes)
                {
                    Some((*rep, pat_bytes.len()))
                } else {
                    None
                }
            }) {
                out.push_str(replacement);
                i += consumed;
                continue;
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

fn sanitize_css_declarations(body: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for decl in split_style_declarations(body) {
        if let Some(clean) = sanitize_declaration(decl) {
            kept.push(clean);
        }
    }
    kept.join("; ")
}

// CSS 反斜杠转义还原：\70 osition -> position，p\osition -> position，java\script -> javascript
fn unescape_css(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next == b'\n' {
                i += 2;
                continue;
            }
            if next == b'\r' {
                i += 2;
                if bytes.get(i) == Some(&b'\n') {
                    i += 1;
                }
                continue;
            }
            if next.is_ascii_hexdigit() {
                let mut j = i + 1;
                let mut hex = String::new();
                while j < bytes.len() && hex.len() < 6 && bytes[j].is_ascii_hexdigit() {
                    hex.push(bytes[j] as char);
                    j += 1;
                }
                if j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    if bytes[j] == b'\r' && bytes.get(j + 1) == Some(&b'\n') {
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
                i = j;
                continue;
            }
            let ch = input[i + 1..].chars().next().unwrap();
            out.push(ch);
            i += 1 + ch.len_utf8();
            continue;
        }

        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

// -webkit-position -> position；-moz-binding 这类整体在黑名单里的，调用方先查原名
fn strip_vendor_prefix(prop: &str) -> String {
    if let Some(rest) = prop.strip_prefix('-') {
        if let Some(idx) = rest.find('-') {
            return rest[idx + 1..].to_string();
        }
    }
    prop.to_string()
}

// 放行 http(s)、file:、data:image 与相对路径（Word 本地图片/同目录资源靠它，剥了会空白）；
// 只剥 url() 里会执行代码的 scheme：javascript:/vbscript:/非图片 data:
fn has_unsafe_css_url(value_lower: &str) -> bool {
    let mut i = 0usize;
    while i < value_lower.len() {
        let Some(pos) = find_ignore_case(&value_lower[i..], "url(") else {
            return false;
        };
        let start = i + pos + 4;
        let mut j = start;
        let mut quote: Option<u8> = None;
        let bytes = value_lower.as_bytes();
        while j < bytes.len() {
            match quote {
                Some(q) => {
                    if bytes[j] == q {
                        quote = None;
                    }
                }
                None => {
                    if bytes[j] == b'"' || bytes[j] == b'\'' {
                        quote = Some(bytes[j]);
                    } else if bytes[j] == b')' {
                        break;
                    }
                }
            }
            j += 1;
        }
        let inner = value_lower[start..j].trim().trim_matches(|c| c == '"' || c == '\'');
        if let Some(scheme) = css_url_scheme(inner) {
            if scheme == "data" {
                return !inner.starts_with("data:image/");
            }
            if scheme == "javascript" || scheme == "vbscript" {
                return true;
            }
        }
        i = j + 1;
    }
    false
}

// 取 url() 内的 scheme；相对路径（./、/、../、无 scheme）返回 None
fn css_url_scheme(inner: &str) -> Option<String> {
    let mut scheme = String::with_capacity(16);
    for c in inner.chars().take(64) {
        if c == ':' {
            return Some(scheme);
        }
        // 浏览器解析 scheme 时忽略空白与控制字符
        if c.is_whitespace() || (c as u32) < 0x20 {
            continue;
        }
        if c == '/' || c == '#' || c == '?' || c == '.' || c == '\\' {
            return None;
        }
        if !(c.is_ascii_alphanumeric() || c == '+' || c == '-') {
            return None;
        }
        scheme.push(c.to_ascii_lowercase());
    }
    None
}

fn is_dangerous_css_text(text: &str) -> bool {
    // 先解实体再解转义：url(javascript&#58;...) 这类混淆必须仍被识别
    let lower = unescape_css(&decode_entities(text)).to_ascii_lowercase();
    lower.contains("expression(")
        || lower.contains("javascript:")
        || lower.contains("vbscript:")
        || lower.contains("data:text/html")
        || has_unsafe_css_url(&lower)
}

// 保留 <style> 但清洗内部规则：@media/@supports 递归处理，@import 等语句丢弃，空规则整条丢掉
fn sanitize_css_stylesheet(css: &str) -> String {
    // 先还原引号实体：&quot; 末尾的 ';' 会被当成语句分隔符，把声明切成半截
    let css = decode_css_quotes(css);
    let css = strip_css_comments(&css);
    let mut out = String::with_capacity(css.len());
    let mut i = 0usize;

    while i < css.len() {
        while i < css.len() {
            let ch = css[i..].chars().next().unwrap();
            if ch.is_whitespace() {
                i += ch.len_utf8();
            } else {
                break;
            }
        }
        if i >= css.len() {
            break;
        }

        let Some((delim_idx, delim)) = find_css_delimiter(&css, i) else {
            break;
        };

        if delim == b';' {
            // 顶层 @import/@charset 等语句（含外链样式表）一律丢弃
            i = delim_idx + 1;
            continue;
        }

        let prelude = css[i..delim_idx].trim();
        let Some(close) = find_matching_brace(&css, delim_idx) else {
            break;
        };
        let body = &css[delim_idx + 1..close];

        let lower_prelude = prelude.to_ascii_lowercase();
        if lower_prelude.starts_with("@import") || lower_prelude.starts_with("@charset") {
            i = close + 1;
            continue;
        }

        if has_top_level_brace(body) {
            // @media/@supports 这类块：递归清洗内部规则，保留外层条件
            let inner = sanitize_css_stylesheet(body);
            if !inner.is_empty() && !is_dangerous_css_text(prelude) {
                out.push_str(prelude);
                out.push('{');
                out.push_str(&inner);
                out.push('}');
            }
        } else {
            let decls = sanitize_css_declarations(body);
            if !decls.is_empty() && !is_dangerous_css_text(prelude) {
                out.push_str(prelude);
                out.push('{');
                out.push_str(&decls);
                out.push('}');
            }
        }

        i = close + 1;
    }

    out
}

fn strip_css_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            match css[i + 2..].find("*/") {
                Some(pos) => {
                    i = i + 2 + pos + 2;
                    continue;
                }
                None => break,
            }
        }
        let ch = css[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    out
}

// 找到顶层第一个 '{' 或 ';'（忽略引号与括号里的内容）
fn find_css_delimiter(css: &str, from: usize) -> Option<(usize, u8)> {
    let bytes = css.as_bytes();
    let mut quote: Option<u8> = None;
    let mut depth = 0usize;
    let mut i = from;

    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'(' => depth += 1,
                b')' => depth = depth.saturating_sub(1),
                b'{' | b';' if depth == 0 => return Some((i, b)),
                _ => {}
            },
        }
        i += 1;
    }

    None
}

fn find_matching_brace(css: &str, open: usize) -> Option<usize> {
    let bytes = css.as_bytes();
    let mut quote: Option<u8> = None;
    let mut depth = 0usize;
    let mut i = open;

    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }

    None
}

// 判断块体里是否还有嵌套规则（@media 等），有则递归当样式表处理，否则当声明块处理
fn has_top_level_brace(body: &str) -> bool {
    matches!(find_css_delimiter(body, 0), Some((_, b'{')))
}

fn split_style_declarations(style: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0usize;
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;

    for (idx, c) in style.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' => depth += 1,
                ')' => depth = (depth - 1).max(0),
                ';' if depth == 0 => {
                    result.push(&style[start..idx]);
                    start = idx + 1;
                }
                _ => {}
            },
        }
    }

    result.push(&style[start..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::database::ClipboardDataSeed;

    const SAMPLE_BODY: &str = r#"<p style="color: rgb(255, 0, 0); font-weight: bold; font-size: 18px; font-family: 'Microsoft YaHei'; line-height: 1.8; text-align: center; position: fixed; top: 0; z-index: 9999">第一段 <span style="font-style: italic; text-decoration: underline; background-color: #ffff00">斜体下划线</span><br>第二行 &amp; 实体</p><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==" onerror="alert(1)" width="40" height="40"><a href="https://example.com/a?b=1&amp;c=2" onclick="alert(1)">安全链接</a><a href="javascript:alert(1)">危险链接</a><a href="&#106;avascript:alert(1)">实体混淆</a><img src="https://example.com/x.png"><iframe src="https://evil.example"></iframe><div style="white-space: pre-wrap; margin-left: 12px">保留空白</div><object data="x"></object><script>alert(2)</script>"#;

    // Word/Outlook 风格样式表 + 各类危险变体（@import/expression/position/转义）
    const SAMPLE_STYLE: &str = r#"p.MsoNormal {font-family:"Microsoft YaHei"; font-size:11.0pt; line-height:1.5} @page { margin: 2cm; } @media screen { p { color:#333; position:absolute } } @import url("http://blocked.example/x.css"); p.Bad { background:url(javascript:alert(1)); width:expression(alert(2)); color:red } p.Local { background:url(file:///C:/Users/x/a.png) } p.Rel { background:url(./img/a.png) } p.Remote { background:url(http://evil/a.png) }"#;

    fn sample_document() -> String {
        format!(
            "<!DOCTYPE html>\n<html><head><style>{}</style><script>alert(0)</script></head><body>\n<!--StartFragment-->{}\n<!--EndFragment-->\n</body></html>",
            SAMPLE_STYLE, SAMPLE_BODY
        )
    }

    fn build_sample_cf_html() -> String {
        let doc = sample_document();
        let header = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
        let start_html = header.len();
        let end_html = start_html + doc.len();
        let start_fragment = start_html + doc.find("<!--StartFragment-->").unwrap();
        let end_fragment = start_html + doc.find("<!--EndFragment-->").unwrap();
        format!(
            "Version:0.9\r\nStartHTML:{:010}\r\nEndHTML:{:010}\r\nStartFragment:{:010}\r\nEndFragment:{:010}\r\n{}",
            start_html, end_html, start_fragment, end_fragment, doc
        )
    }

    #[test]
    fn keeps_formatting_and_strips_danger() {
        let sanitized = sanitize_clipboard_html(&sample_document());

        for keep in [
            "<br",
            "<img src=\"data:image/png;base64,iVBOR",
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
            "<style>",
            "p.MsoNormal{font-family:\"Microsoft YaHei\"; font-size:11.0pt; line-height:1.5}",
            "@page{margin: 2cm}",
            "@media screen{p{color:#333}}",
            // 保真优先：Word 本地图片、相对路径、外链图片的 url() 都要原样保留
            "p.Local{background:url(file:///C:/Users/x/a.png)}",
            "p.Rel{background:url(./img/a.png)}",
            "p.Remote{background:url(http://evil/a.png)}",
        ] {
            assert!(sanitized.contains(keep), "缺少格式属性: {}", keep);
        }

        for forbidden in [
            "<script",
            "<iframe",
            "<object",
            "@import",
            "http://blocked.example",
            "onerror=",
            "onclick=",
            "javascript:",
            "vbscript:",
            "position:",
            "z-index:",
            "top:",
            "expression(",
            "alert(",
        ] {
            assert!(!sanitized.contains(forbidden), "危险内容未清除: {}", forbidden);
        }
    }

    #[test]
    fn rebuilds_raw_html_format_with_valid_offsets() {
        let mut raw = vec![ClipboardDataSeed {
            format_name: "HTML Format".to_string(),
            raw_data: build_sample_cf_html().into_bytes(),
            is_primary: true,
            format_order: 0,
        }];

        sanitize_raw_html_formats(&mut raw, 1024 * 1024);
        assert_eq!(raw.len(), 1);

        let text = String::from_utf8(raw[0].raw_data.clone()).unwrap();
        assert!(text.contains("Version:0.9"));
        assert!(text.contains("<br"));
        assert!(text.contains("href=\"https://example.com/a?b=1&amp;c=2\""));
        assert!(text.contains("data:image/png;base64,iVBOR"));
        assert!(!text.contains("javascript:"));
        assert!(!text.contains("onerror="));
        assert!(!text.contains("<script"));

        let start_html: usize = text[text.find("StartHTML:").unwrap() + "StartHTML:".len()..]
            .chars()
            .take(10)
            .collect::<String>()
            .parse()
            .unwrap();
        let start_fragment: usize =
            text[text.find("StartFragment:").unwrap() + "StartFragment:".len()..]
                .chars()
                .take(10)
                .collect::<String>()
                .parse()
                .unwrap();
        let end_fragment: usize =
            text[text.find("EndFragment:").unwrap() + "EndFragment:".len()..]
                .chars()
                .take(10)
                .collect::<String>()
                .parse()
                .unwrap();

        assert_eq!(&text[start_html..start_html + 1], "<");
        assert_eq!(&text[start_fragment..start_fragment + "<!--StartFragment-->".len()], "<!--StartFragment-->");
        assert_eq!(&text[end_fragment..end_fragment + "<!--EndFragment-->".len()], "<!--EndFragment-->");

        let out = std::env::temp_dir().join("qc_sanitized_html_format.txt");
        std::fs::write(&out, text.as_bytes()).unwrap();
        eprintln!("写出样本: {} 字节 -> {:?}", text.len(), out);
    }

    #[test]
    fn truncates_at_tag_boundary() {
        let mut html = String::new();
        for _ in 0..40 {
            html.push_str("<p style=\"color:#123456\">段落内容</p>");
        }
        let limited = limit_html_bytes(&html, 300);
        assert!(limited.len() <= 300);
        assert!(limited.ends_with("</p>"), "截断点应落在标签边界: {:?}", &limited[limited.len().saturating_sub(30)..]);
    }

    // 截断落在 <style> 正文中间时，未闭合的整块必须丢弃，避免后半段文档样式被吞掉
    #[test]
    fn drops_unclosed_style_block_on_truncation() {
        let html = format!("<p>正文</p><style>{}</style>", "p{color:#333}".repeat(40));
        let limited = limit_html_bytes(&html, 300);
        assert!(limited.len() <= 300);
        assert!(!limited.contains("<style"), "未闭合的 style 块应整块丢弃");
        assert_eq!(limited, "<p>正文</p>", "应回退到 style 之前的完整结构");

        // 文档以 style 开头且截断在块内时，不能留下半个 style
        let only_style = format!("<style>{}</style>", "p{color:#333}".repeat(40));
        assert_eq!(limit_html_bytes(&only_style, 100), "");
    }

    // CSS 正文里的 '<' 不是标签，不参与边界判定；完整闭合的 style 应原样保留
    #[test]
    fn keeps_complete_style_block_and_ignores_angle_brackets_in_css() {
        let css = r#"p::before{content:"<x>"}"#;
        let html = format!("<style>{}</style>{}", css, "<p>段落</p>".repeat(60));
        let limited = limit_html_bytes(&html, 200);
        assert!(limited.len() <= 200);
        assert!(limited.contains("</style>"), "完整闭合的 style 块应保留");
        assert!(
            limited.contains(r#"content:"<x>""#),
            "CSS 内的 '<' 不应被当作标签截断点"
        );
    }

    // <script> 同样按 raw text 处理：截断在正文中间时整块丢弃
    #[test]
    fn drops_unclosed_script_block_on_truncation() {
        let html = format!("<p>前文</p><script>{}", "var a=1;".repeat(60));
        let limited = limit_html_bytes(&html, 200);
        assert!(limited.len() <= 200);
        assert!(!limited.contains("<script"), "未闭合的 script 块应整块丢弃");
        assert_eq!(limited, "<p>前文</p>");
    }

    #[test]
    fn allows_mailto_and_tel_but_removes_data_html() {
        let html = r#"<a href="mailto:a@b.com">m</a><a href="tel:+8613800000000">t</a><a href="data:text/html,<b>x</b>">d</a><img src="data:text/html,<b>x</b>">"#;
        let sanitized = sanitize_clipboard_html(html);
        assert!(sanitized.contains("href=\"mailto:a@b.com\""));
        assert!(sanitized.contains("href=\"tel:+8613800000000\""));
        assert!(!sanitized.contains("data:text/html"));
    }

    #[test]
    fn drops_only_oversized_data_image() {
        // 第一张 src 26 字节（等于上限，保留），第二张 34 字节（超过 26 字节上限，仅丢这张图）
        let html = r#"<p>文字</p><img src="data:image/png;base64,AAAA"><img src="data:image/png;base64,BBBBBBBBBBBB" width="10"><p>后续段落</p>"#;
        let out = strip_oversized_data_images(html, 26);
        assert!(out.contains("data:image/png;base64,AAAA"), "小图应保留");
        assert!(!out.contains("BBBBBBBBBBBB"), "超大图应被剔除");
        assert!(out.contains("<p>文字</p>"), "前段结构应保留");
        assert!(out.contains("<p>后续段落</p>"), "后续内容应保留");
    }

    #[test]
    fn keeps_literal_less_than_in_text() {
        let html = "1 < 2，a <3 元，x < y 均按原文保留";
        assert_eq!(sanitize_clipboard_html(html), html);
    }

    // Word/Outlook 依赖的字体、字号、行距规则必须原样保留
    #[test]
    fn keeps_word_style_rules() {
        let html = r#"<html><head><style>p.MsoNormal {font-family:"Microsoft YaHei"; font-size:11.0pt; line-height:1.5} @page {margin: 2cm;}</style></head><body><p class="MsoNormal">正文</p></body></html>"#;
        let out = sanitize_clipboard_html(html);

        assert!(out.contains("<style>"), "style 元素应保留: {}", out);
        assert!(
            out.contains("p.MsoNormal{font-family:\"Microsoft YaHei\"; font-size:11.0pt; line-height:1.5}"),
            "Word 正文规则应完整保留: {}",
            out
        );
        assert!(out.contains("@page{margin: 2cm}"), "页面设置应保留: {}", out);
        assert!(out.contains("</style>"), "闭合标签应保留: {}", out);
        assert!(out.contains("class=\"MsoNormal\""), "class 引用不应被删除: {}", out);
    }

    // 转义、大小写、注释藏匿、vendor 前缀、危险 url、@import 等变体必须清掉；
    // 但 file:///、相对路径、http 外链图片属于保真代价，必须原样保留
    #[test]
    fn strips_dangerous_css_variants() {
        let html = r#"<style>
            p.Evil { POSITION: fixed; Z-INDEX: 9999; top: 0; left: 0 }
            p.Expr { width: expression(alert(1)); color: red }
            p.Comment { /* expression(alert(9)) */ color: blue }
            p.Escape { \70 osition: absolute }
            p.Vendor { -webkit-position: fixed; -moz-behavior: url(#default#time2); -moz-binding: url(evil.xml) }
            p.ScriptUrl { background: url("javascript:alert(1)") }
            p.DataUrl { background: url(data:text/html,<b>x</b>) }
            p.Local { background: url("file:///C:/x.png") }
            p.Rel { background: url(./img/a.png) }
            p.Remote { background: url(http://evil/a.png) }
            p.Safe { background: url(https://example.com/a.png); color: green }
            @import url("http://blocked.example/x.css");
            @import "http://blocked.example/y.css";
            @media screen { p { color: green; position: absolute } }
        </style>"#;
        let out = sanitize_clipboard_html(html);
        let lower = out.to_ascii_lowercase();

        for bad in [
            "position", "z-index", "top:", "left:", "expression(", "alert(", "javascript:",
            "behavior", "-moz-binding", "-webkit-position", "data:text/html",
            "@import", "http://blocked.example",
        ] {
            assert!(!lower.contains(bad), "危险 CSS 变体未清除: {}", bad);
        }

        for keep in [
            "<style>",
            "p.Expr{color: red}",
            "p.Comment{color: blue}",
            "p.Local{background: url(\"file:///C:/x.png\")}",
            "p.Rel{background: url(./img/a.png)}",
            "p.Remote{background: url(http://evil/a.png)}",
            "p.Safe{background: url(https://example.com/a.png); color: green}",
            "@media screen{p{color: green}}",
        ] {
            assert!(out.contains(keep), "安全 CSS 未保留: {} / {}", keep, out);
        }
        assert!(!out.contains("p.ScriptUrl"), "url(javascript:) 整条应丢弃: {}", out);
        assert!(!out.contains("p.DataUrl"), "url(data:text/html) 整条应丢弃: {}", out);
    }

    // 保真回归：file:///、相对路径、http 外链完整保留；js/data:text 仍被剥
    #[test]
    fn keeps_local_and_remote_css_urls() {
        let html = r#"<style>
            p.A { background:url(file:///C:/Users/x/a.png) }
            p.B { background:url(./img/a.png) }
            p.C { background:url(http://example.com/a.png) }
            p.D { background:url(javascript:alert(1)) }
            p.E { background:url(data:text/html,<script>alert(1)</script>) }
        </style>"#;
        let out = sanitize_clipboard_html(html);

        for keep in [
            "p.A{background:url(file:///C:/Users/x/a.png)}",
            "p.B{background:url(./img/a.png)}",
            "p.C{background:url(http://example.com/a.png)}",
        ] {
            assert!(out.contains(keep), "本地/外链 url 应完整保留: {} / {}", keep, out);
        }
        for bad in ["javascript:", "data:text/html", "p.D", "p.E"] {
            assert!(!out.contains(bad), "危险 url 应剥离: {} / {}", bad, out);
        }
    }

    // 空 style 或清洗后为空时不能留下空壳标签
    #[test]
    fn drops_empty_style_shell() {
        let html = r#"<p>前</p><style></style><style>   </style><style>/* 只剩注释 */</style><style>position:fixed; top:0</style><p>后</p>"#;
        let out = sanitize_clipboard_html(html);

        assert!(!out.contains("<style"), "空壳 style 应整块移除: {}", out);
        assert!(out.contains("<p>前</p>"), "前段应保留: {}", out);
        assert!(out.contains("<p>后</p>"), "后段应保留: {}", out);
    }

    // 缺陷 1 回归：&quot; 实体自带 ';'，不能被当成声明分隔符把 font-family 切成半截
    #[test]
    fn keeps_font_family_with_entity_quotes() {
        let html = r#"<p style="font-family: &quot;Microsoft YaHei&quot;, sans-serif; font-size: 12px; line-height: 1.6">A</p>"#;
        let out = sanitize_clipboard_html(html);

        assert!(
            out.contains("font-family: &quot;Microsoft YaHei&quot;, sans-serif"),
            "font-family 应完整保留: {}",
            out
        );
        assert!(out.contains("font-size: 12px"), "font-size 应保留: {}", out);
        assert!(out.contains("line-height: 1.6"), "line-height 应保留: {}", out);
        // 残片形态：&quot 之后直接就是属性结束引号
        assert!(!out.contains("&quot\""), "不允许 &quot 结尾的残片: {}", out);
        assert!(!out.contains("&#34\""), "不允许 &#34 结尾的残片: {}", out);
        eprintln!("font-family 样本输出: {}", out);
    }

    // 引号内的分号不是声明分隔符（单引号与 url() 都要完整保留）
    #[test]
    fn keeps_quoted_semicolons_in_style() {
        let html = r#"<p style="font-family: 'Arial; B', serif; background: url(&quot;https://example.com/x;y.png&quot;)">B</p>"#;
        let out = sanitize_clipboard_html(html);

        assert!(
            out.contains("font-family: 'Arial; B', serif"),
            "单引号内分号不应切断声明: {}",
            out
        );
        assert!(
            out.contains("url(&quot;https://example.com/x;y.png&quot;)"),
            "url 引号内的分号应完整保留: {}",
            out
        );
    }

    // 引号未闭合 = 半截声明，整条丢弃，不能输出残片
    #[test]
    fn drops_truncated_style_declaration() {
        let html = r#"<p style="font-family: &quot;未闭合; color: red">X</p>"#;
        let out = sanitize_clipboard_html(html);

        assert!(!out.contains("font-family"), "残缺声明应整条丢弃: {}", out);
        assert!(!out.contains("color: red"), "残缺声明的残片不应输出: {}", out);
    }

    // <style> 块内的引号实体与引号内分号同样要完整保留
    #[test]
    fn keeps_quoted_values_in_style_block() {
        let html = r#"<style>p{font-family:&quot;Microsoft YaHei&quot;,sans-serif;content:"a;b"}</style>"#;
        let out = sanitize_clipboard_html(html);

        assert!(out.contains("<style>"), "style 元素应保留: {}", out);
        assert!(
            out.contains("font-family:\"Microsoft YaHei\",sans-serif"),
            "样式表内 font-family 应完整: {}",
            out
        );
        assert!(out.contains("content:\"a;b\""), "引号内分号应保留: {}", out);
    }
}
