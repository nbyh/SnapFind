//! 内容抽取（文档 §6）：`extract(path, limits) -> Result<ExtractedText, ExtractError>`。
//!
//! 设计约束：
//! * 流式/限量读取，绝不整文件入内存（PDF 除外，需整体解析，受 `max_bytes` 限制）；
//! * 所有解析错误转为 `ExtractError`，**不得 panic**（PDF 解析已用 `catch_unwind` 包裹）；
//! * 加密文档返回 `Encrypted`，不尝试破解。

use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_text_bytes: usize,
    pub timeout_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_text_bytes: 2 * 1024 * 1024,
            timeout_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtractedText {
    pub text: String,
    pub truncated: bool,
    pub encoding: String,
}

#[derive(Debug, Clone)]
pub enum ExtractError {
    Unsupported,
    Encrypted,
    TooLarge,
    Failed(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(f, "unsupported type"),
            Self::Encrypted => write!(f, "encrypted document"),
            Self::TooLarge => write!(f, "document too large"),
            Self::Failed(m) => write!(f, "{m}"),
        }
    }
}

pub fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// 所有可抽取正文的扩展名（UI 的「类型」多选标签也用这份清单）。
///
/// 含老式 OLE2 Office（`doc` / `xls` / `ppt`）：没有 XML 部件，
/// 由 `ole_text` 做可打印文本兜底抽取，能预览也能全文检索。
pub const SUPPORTED_EXTS: &[&str] = &[
    "txt", "text", "log", "csv", "tsv", "ini", "cfg", "conf", "toml", "yaml", "yml", "json",
    "xml", "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "go", "c", "h", "cc", "cpp",
    "hpp", "java", "kt", "cs", "rb", "php", "swift", "sh", "bash", "zsh", "ps1", "sql", "md",
    "markdown", "html", "htm", "xhtml", "rtf", "pdf", "docx", "dotx", "xlsx", "xlsm", "pptx",
    "ppsx", "epub", "doc", "dot", "xls", "xlt", "ppt", "pps", "pot",
];

/// 是否支持对内容做抽取（不支持时仍索引文件名与路径）。
pub fn is_supported(ext: &str) -> bool {
    SUPPORTED_EXTS.contains(&ext)
}

/// 抽取入口。
pub fn extract(path: &Path, limits: &Limits) -> Result<ExtractedText, ExtractError> {
    let ext = ext_of(path);
    if !is_supported(&ext) {
        return Err(ExtractError::Unsupported);
    }
    // 纯文本族最终截断到 max_text_bytes；GBK/UTF-16 解码成 UTF-8 最多放大 2 倍，
    // 读入上限据此收紧，避免把超大日志/CSV 整读进内存。PDF/Office 需整体解析，仍用 max_bytes。
    let plain_ext = !matches!(
        ext.as_str(),
        "pdf" | "docx" | "dotx" | "xlsx" | "xlsm" | "pptx" | "ppsx" | "epub" | "doc" | "dot"
            | "xls" | "xlt" | "ppt" | "pps" | "pot"
    );
    let read_cap = if plain_ext {
        (limits.max_text_bytes.saturating_mul(2) + 64 * 1024).min(limits.max_bytes)
    } else {
        limits.max_bytes
    };
    let (bytes, read_truncated) = read_limited(path, read_cap)?;

    let mut out = match ext.as_str() {
        "html" | "htm" | "xhtml" => {
            let s = decode(&bytes);
            ExtractedText {
                text: strip_html(&s.0),
                truncated: read_truncated,
                encoding: s.1,
            }
        }
        "md" | "markdown" => {
            let s = decode(&bytes);
            ExtractedText {
                text: strip_markdown(&s.0),
                truncated: read_truncated,
                encoding: s.1,
            }
        }
        "rtf" => {
            let s = decode(&bytes);
            ExtractedText {
                text: rtf_to_text(&s.0),
                truncated: read_truncated,
                encoding: s.1,
            }
        }
        "pdf" => {
            let text = pdf_text(&bytes)?;
            ExtractedText {
                text,
                truncated: read_truncated,
                encoding: "pdf-cmap".into(),
            }
        }
        "docx" | "dotx" | "xlsx" | "xlsm" | "pptx" | "ppsx" | "epub" => {
            let text = office_text(&bytes, &ext)?;
            ExtractedText {
                text,
                truncated: read_truncated,
                encoding: "zip+xml".into(),
            }
        }
        "doc" | "dot" | "xls" | "xlt" | "ppt" | "pps" | "pot" => ExtractedText {
            text: ole_text(&bytes),
            truncated: read_truncated,
            encoding: "ole-text".into(),
        },
        _ => {
            let s = decode(&bytes);
            ExtractedText {
                text: s.0,
                truncated: read_truncated,
                encoding: s.1,
            }
        }
    };

    if out.text.len() > limits.max_text_bytes {
        let cut = floor_char_boundary(&out.text, limits.max_text_bytes);
        out.text.truncate(cut);
        out.truncated = true;
    }
    Ok(out)
}

fn read_limited(path: &Path, max: usize) -> Result<(Vec<u8>, bool), ExtractError> {
    let mut f = std::fs::File::open(path).map_err(|e| ExtractError::Failed(e.to_string()))?;
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut truncated = false;
    loop {
        match f.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() + n > max {
                    let take = max.saturating_sub(buf.len());
                    buf.extend_from_slice(&chunk[..take]);
                    truncated = true;
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ExtractError::Failed(e.to_string())),
        }
    }
    Ok((buf, truncated))
}

/// 老式 OLE2 Office（`doc`/`xls`/`ppt`）没有可解析的 XML 部件，这里做
/// 「可打印文本」兜底抽取：先取连续 ASCII 串（xls 的压缩字符串等），
/// 再取未被覆盖的 UTF-16LE 连续片段（doc/ppt 正文多为 UTF-16LE）。
///
/// 结果是无排版的纯文本，但足以让标题/正文出现在预览与全文检索里。
fn ole_text(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut covered: Vec<(usize, usize)> = Vec::new();

    // 1) 连续可打印 ASCII（≥6 字节），例如 xls 的 BIFF 压缩字符串
    let mut i = 0usize;
    while i < bytes.len() {
        if (0x20..=0x7e).contains(&bytes[i]) {
            let start = i;
            while i < bytes.len() && (0x20..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            if i - start >= 6 {
                out.push_str(std::str::from_utf8(&bytes[start..i]).unwrap_or(""));
                out.push('\n');
                covered.push((start, i));
            }
        } else {
            i += 1;
        }
    }

    // 2) 连续 UTF-16LE 可打印片段（跳过已被 ASCII 覆盖的字节）。
    // covered 区间有序且不重叠，用前向指针推进，避免对每个字节线性扫描全部区间。
    let mut cov = 0usize;
    let mut cur = String::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        while cov < covered.len() && covered[cov].1 <= i {
            cov += 1;
        }
        if cov < covered.len() && i >= covered[cov].0 {
            i += 1;
            continue;
        }
        let ch = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let ok = (0x20..=0x7e).contains(&ch)
            || (0x4e00..=0x9fff).contains(&ch)
            || (0x3000..=0x303f).contains(&ch)
            || (0xff00..=0xffef).contains(&ch);
        if ok {
            if let Some(c) = char::from_u32(ch as u32) {
                cur.push(c);
            }
            i += 2;
        } else {
            if cur.chars().count() >= 3 {
                out.push_str(&cur);
                out.push('\n');
            }
            cur.clear();
            i += 1; // 试探奇数对齐
        }
    }
    if cur.chars().count() >= 3 {
        out.push_str(&cur);
        out.push('\n');
    }
    out
}

/// 编码探测 + 解码（失败时按 latin1 兜底，不报错）。
fn decode(bytes: &[u8]) -> (String, String) {
    // UTF-8 BOM
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (
            String::from_utf8_lossy(&bytes[3..]).into_owned(),
            "utf-8".into(),
        );
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let (cow, enc, _) = encoding_rs::UTF_16LE.decode(bytes);
        return (cow.into_owned(), enc.name().to_lowercase());
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), "utf-8".into());
    }
    let mut det = chardetng::EncodingDetector::new();
    // 编码探测只需采样前 64 KB，避免对大文件做全量字符集分析
    det.feed(&bytes[..bytes.len().min(64 * 1024)], true);
    let enc = det.guess(None, true);
    let (cow, _, _) = enc.decode(bytes);
    (cow.into_owned(), enc.name().to_lowercase())
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// 剥除 HTML 标签与脚本/样式，保留实体原文。
pub fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len() / 2);
    let lower = input.to_ascii_lowercase();
    let bytes = input.as_bytes();
    let mut i = 0usize;
    let mut skip_until: Option<usize> = None;
    while i < bytes.len() {
        if let Some(end) = skip_until {
            if i >= end {
                skip_until = None;
            } else {
                i += 1;
                continue;
            }
        }
        if bytes[i] == b'<' {
            // 跳过 script/style 内容
            for tag in ["<script", "<style"] {
                if lower[i..].starts_with(tag) {
                    let close = format!("</{}", &tag[1..]);
                    if let Some(pos) = lower[i..].find(&close) {
                        skip_until = Some(i + pos);
                    }
                    break;
                }
            }
            if let Some(rel) = lower[i..].find('>') {
                i += rel + 1;
                out.push(' ');
                continue;
            }
        }
        // 按字符边界推进
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&input[i..(i + ch_len).min(input.len())]);
        i += ch_len;
    }
    collapse_ws(&out)
}

fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        let is_space = c.is_whitespace();
        if is_space {
            if !prev_space {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
        prev_space = is_space;
    }
    out.trim().to_string()
}

/// 轻量 Markdown 剥离：去掉标记符号，保留可搜索文本。
pub fn strip_markdown(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.lines() {
        let mut l = line.trim_start();
        if l.starts_with("```") {
            continue;
        }
        while l.starts_with('#') || l.starts_with('>') || l.starts_with('-') || l.starts_with('*')
        {
            if l.starts_with("**") || l.starts_with("__") {
                break;
            }
            l = &l[1..];
            l = l.trim_start();
        }
        let cleaned: String = l
            .chars()
            .filter(|c| !matches!(c, '`' | '*' | '_' | '~' | '|' | '#'))
            .collect();
        let cleaned = cleaned
            .replace("](", " ")
            .replace(['[', ']', '(', ')'], " ");
        out.push_str(&cleaned);
        out.push('\n');
    }
    collapse_ws(&out)
}

/// RTF → 纯文本（跳过控制字、二进制块与图片组）。
pub fn rtf_to_text(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len() / 2);
    let mut i = 0usize;
    let mut depth_skip = 0i32;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'{' => {
                // 跳过 {\*\...} 与图片组
                if input[i..].starts_with("{\\*") || input[i..].starts_with("{\\pict") {
                    depth_skip += 1;
                }
                if depth_skip > 0 && !input[i..].starts_with("{\\*") {
                    depth_skip += 1;
                }
                i += 1;
            }
            b'}' => {
                if depth_skip > 0 {
                    depth_skip -= 1;
                }
                i += 1;
            }
            b'\\' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len()
                    && (bytes[j].is_ascii_alphabetic() || bytes[j] == b'-' || bytes[j].is_ascii_digit())
                {
                    j += 1;
                }
                let word = &input[start..j];
                // 跳过参数
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j].is_ascii_digit() || bytes[j] == b'-') {
                    j += 1;
                }
                if depth_skip == 0 {
                    match word {
                        "par" | "line" | "row" | "sect" | "page" => out.push('\n'),
                        "tab" => out.push('\t'),
                        "u" => {
                            // \uNNNN -> 字符
                            let num: String = input[start + 1..]
                                .chars()
                                .take_while(|c| c.is_ascii_digit() || *c == '-')
                                .collect();
                            if let Ok(code) = num.parse::<i32>() {
                                let code = if code < 0 { code + 65536 } else { code };
                                if let Some(ch) = char::from_u32(code as u32) {
                                    out.push(ch);
                                }
                            }
                        }
                        "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" => {}
                        _ => {}
                    }
                }
                i = j.max(i + 1);
            }
            b'\r' | b'\n' => {
                i += 1;
            }
            _ => {
                if depth_skip == 0 {
                    let ch_len = utf8_len(b);
                    out.push_str(&input[i..(i + ch_len).min(input.len())]);
                    i += ch_len;
                } else {
                    i += 1;
                }
            }
        }
    }
    collapse_ws(&out)
}

fn pdf_text(bytes: &[u8]) -> Result<String, ExtractError> {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    if bytes.windows(5).any(|w| w == b"/Encrypt") {
        return Err(ExtractError::Encrypted);
    }
    let res = catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_from_mem(bytes)));
    match res {
        Ok(Ok(text)) => {
            if text.trim().is_empty() {
                Err(ExtractError::Failed("no text layer".into()))
            } else {
                Ok(text)
            }
        }
        Ok(Err(e)) => Err(ExtractError::Failed(format!("pdf: {e}"))),
        Err(_) => Err(ExtractError::Failed("pdf parser panicked".into())),
    }
}

fn office_text(bytes: &[u8], ext: &str) -> Result<String, ExtractError> {
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| {
        let msg = e.to_string();
        if msg.to_lowercase().contains("password") || msg.to_lowercase().contains("encrypt") {
            ExtractError::Encrypted
        } else {
            ExtractError::Failed(format!("zip: {msg}"))
        }
    })?;

    let mut out = String::new();
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .collect();

    let wanted: Vec<String> = names
        .into_iter()
        .filter(|n| match ext {
            "docx" | "dotx" => n.starts_with("word/") && n.ends_with(".xml") && !n.contains("theme"),
            "xlsx" | "xlsm" => {
                (n.starts_with("xl/sharedStrings") || n.starts_with("xl/worksheets/"))
                    && n.ends_with(".xml")
            }
            "pptx" | "ppsx" => n.starts_with("ppt/slides/") && n.ends_with(".xml"),
            "epub" => n.ends_with(".xhtml") || n.ends_with(".html") || n.ends_with(".htm"),
            _ => false,
        })
        .collect();

    if wanted.is_empty() {
        return Err(ExtractError::Failed("no content parts".into()));
    }

    for name in wanted {
        let mut file = match zip.by_name(&name) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            continue;
        }
        let text = String::from_utf8_lossy(&buf);
        if ext == "epub" {
            out.push_str(&strip_html(&text));
        } else {
            out.push_str(&xml_text(&text));
        }
        out.push('\n');
        if out.len() > 8 * 1024 * 1024 {
            break;
        }
    }
    let collapsed = collapse_ws(&out);
    if collapsed.is_empty() {
        Err(ExtractError::Failed("empty document".into()))
    } else {
        Ok(collapsed)
    }
}

/// 从 OOXML 片段中收集文本节点（`w:t`、`a:t`、`t`，以及 XLSX 数字值 `v`）。
fn xml_text(xml: &str) -> String {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = String::new();
    let mut capture = false;
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let local = name.local_name();
                capture = matches!(local.as_ref(), b"t" | b"v");
            }
            Ok(Event::Text(t)) => {
                if capture {
                    if let Ok(s) = t.unescape() {
                        out.push_str(&s);
                        out.push(' ');
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                let local = name.local_name();
                if matches!(local.as_ref(), b"t" | b"v") {
                    capture = false;
                }
                if local.as_ref() == b"p" {
                    out.push('\n');
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_strip() {
        let html = "<html><head><style>x{}</style></head><body><h1>Hello</h1><script>bad()</script>世界</body></html>";
        let t = strip_html(html);
        assert!(t.contains("Hello"));
        assert!(t.contains("世界"));
        assert!(!t.contains("bad"));
    }

    #[test]
    fn rtf_strip() {
        let rtf = r"{\rtf1\ansi{\fonttbl{\f0 Arial;}}\f0 Hello \b World\b0\par 第二行}";
        let t = rtf_to_text(rtf);
        assert!(t.contains("Hello"));
        assert!(t.contains("World"));
        assert!(t.contains("第二行"));
    }

    #[test]
    fn markdown_strip() {
        let md = "# Title\n\n- item **bold**\n```\ncode\n```\n";
        let t = strip_markdown(md);
        assert!(t.contains("Title"));
        assert!(t.contains("item"));
    }

    #[test]
    fn legacy_office_is_previewable() {
        for ext in ["doc", "dot", "xls", "xlt", "ppt", "pps", "pot", "md"] {
            assert!(is_supported(ext), "{ext} 应可预览");
        }
        assert!(!is_supported("exe"));
        assert!(!is_supported("dll"));
    }

    /// 老式 OLE 正文多存为 UTF-16LE，xls 也常有纯 ASCII 串。
    #[test]
    fn ole_text_picks_up_printable_runs() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0x00, 0x01, 0x02]); // 二进制前缀
        for u in "Hello 世界".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        bytes.extend_from_slice(&[0x00, 0x00]);
        bytes.extend_from_slice(b"BudgetReport2024"); // BIFF 压缩字符串
        let t = ole_text(&bytes);
        assert!(t.contains("Hello"), "{t:?}");
        assert!(t.contains("世界"), "{t:?}");
        assert!(t.contains("BudgetReport2024"), "{t:?}");
    }
}
