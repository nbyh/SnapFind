//! 查询引擎（文档 §7）：语法解析 → FTS5 表达式 → 过滤器下推 → 排序 → 高亮片段。

use rusqlite::types::Value;
use snapfind_core::error::{ErrorCode, SnapError, SnapResult};
use snapfind_core::ipc::{Fields, Filters, Hit, HitFlags, HighlightCfg, Mode, QueryRequest, Snippet};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::store::{Store, VolumeStore};
use crate::tokenize;

// ---------------------------------------------------------------------------
// 语法解析
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Term(String, Option<String>),
    Phrase(String, Option<String>),
}

#[derive(Debug, Clone)]
pub enum Node {
    Term(String, Option<String>),
    Phrase(String, Option<String>),
    Not(Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub ast: Option<Node>,
    pub types: Vec<String>,
    pub warnings: Vec<String>,
}

const FIELDS: [&str; 4] = ["name", "path", "content", "ext"];

fn lex(input: &str) -> Result<Vec<Tok>, SnapError> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut pending_not = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' {
            out.push(Tok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            out.push(Tok::RParen);
            i += 1;
            continue;
        }
        if c == '-' || c == '!' {
            while out.len() < pending_not {
                break;
            }
            out.push(Tok::Not);
            pending_not += 1;
            i += 1;
            continue;
        }
        let mut field: Option<String> = None;
        // 字段前缀 name: / path: / content: / ext: / type:
        let rest: String = chars[i..].iter().collect();
        if let Some(colon) = rest.find(':') {
            let head = &rest[..colon];
            let head_ok = FIELDS.contains(&head.to_ascii_lowercase().as_str())
                || head.eq_ignore_ascii_case("type");
            if head_ok && colon > 0 && colon < 16 {
                field = Some(head.to_ascii_lowercase());
                i += head.chars().count() + 1;
            }
        }
        if i >= chars.len() {
            break;
        }
        if chars[i] == '"' {
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            if i < chars.len() {
                i += 1;
            }
            out.push(Tok::Phrase(text, field));
            continue;
        }
        let start = i;
        while i < chars.len()
            && !chars[i].is_whitespace()
            && chars[i] != '('
            && chars[i] != ')'
            && chars[i] != '"'
        {
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        let upper = word.to_ascii_uppercase();
        match upper.as_str() {
            "AND" | "&&" => out.push(Tok::And),
            "OR" | "||" => out.push(Tok::Or),
            "NOT" => out.push(Tok::Not),
            _ => out.push(Tok::Term(word, field)),
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    types: Vec<String>,
    warnings: Vec<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn parse_or(&mut self) -> Result<Option<Node>, SnapError> {
        let mut items = Vec::new();
        if let Some(n) = self.parse_and()? {
            items.push(n);
        }
        while matches!(self.peek(), Some(Tok::Or)) {
            self.pos += 1;
            match self.parse_and()? {
                Some(n) => items.push(n),
                None => {
                    return Err(SnapError::new(
                        ErrorCode::QuerySyntax,
                        "OR 右侧缺少表达式",
                    ))
                }
            }
        }
        Ok(match items.len() {
            0 => None,
            1 => items.pop(),
            _ => Some(Node::Or(items)),
        })
    }

    fn parse_and(&mut self) -> Result<Option<Node>, SnapError> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None | Some(Tok::RParen) | Some(Tok::Or) => break,
                Some(Tok::And) => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            match self.parse_not()? {
                Some(n) => items.push(n),
                None => break,
            }
        }
        Ok(match items.len() {
            0 => None,
            1 => items.pop(),
            _ => Some(Node::And(items)),
        })
    }

    fn parse_not(&mut self) -> Result<Option<Node>, SnapError> {
        let mut negs = 0usize;
        while matches!(self.peek(), Some(Tok::Not)) {
            self.pos += 1;
            negs += 1;
        }
        let mut node = match self.parse_atom()? {
            Some(n) => n,
            None => {
                if negs > 0 {
                    return Err(SnapError::new(
                        ErrorCode::QuerySyntax,
                        "NOT 后缺少表达式",
                    ));
                }
                return Ok(None);
            }
        };
        for _ in 0..negs {
            node = Node::Not(Box::new(node));
        }
        Ok(Some(node))
    }

    fn parse_atom(&mut self) -> Result<Option<Node>, SnapError> {
        let tok = match self.peek().cloned() {
            Some(t) => t,
            None => return Ok(None),
        };
        match tok {
            Tok::LParen => {
                self.pos += 1;
                let inner = self.parse_or()?;
                match self.peek() {
                    Some(Tok::RParen) => {
                        self.pos += 1;
                    }
                    _ => {
                        return Err(SnapError::new(
                            ErrorCode::QuerySyntax,
                            "括号未闭合（unmatched '('）",
                        )
                        .with_detail(serde_json::json!({ "position": self.pos })))
                    }
                }
                Ok(inner)
            }
            Tok::RParen => {
                self.pos += 1;
                Ok(None)
            }
            Tok::Term(text, field) => {
                self.pos += 1;
                if let Some(f) = &field {
                    if f == "ext" || f == "type" {
                        for t in text.split(',') {
                            let t = t.trim().trim_start_matches('.').to_ascii_lowercase();
                            if !t.is_empty() {
                                self.types.push(t);
                            }
                        }
                        return Ok(None);
                    }
                }
                if text.is_empty() {
                    return Ok(None);
                }
                Ok(Some(Node::Term(text, field)))
            }
            Tok::Phrase(text, field) => {
                self.pos += 1;
                if text.trim().is_empty() {
                    return Ok(None);
                }
                if let Some(f) = &field {
                    if f == "ext" || f == "type" {
                        self.warnings.push("短语不能用于类型过滤，已忽略".into());
                        return Ok(None);
                    }
                }
                Ok(Some(Node::Phrase(text, field)))
            }
            Tok::And => {
                self.pos += 1;
                Ok(None)
            }
            Tok::Or => Ok(None),
            Tok::Not => Ok(None),
        }
    }
}

/// 解析查询串。
pub fn parse(query: &str) -> SnapResult<Parsed> {
    let toks = lex(query)?;
    let mut p = Parser {
        toks,
        pos: 0,
        types: Vec::new(),
        warnings: Vec::new(),
    };
    let ast = p.parse_or()?;
    if p.pos < p.toks.len() {
        return Err(SnapError::new(
            ErrorCode::QuerySyntax,
            format!("无法解析的查询片段（位置 {}）", p.pos),
        ));
    }
    Ok(Parsed {
        ast,
        types: p.types,
        warnings: p.warnings,
    })
}

fn columns_for(field: &Option<String>, fields: &Fields) -> Vec<&'static str> {
    match field.as_deref() {
        Some("name") => vec!["name_tok"],
        Some("path") => vec!["path_tok"],
        Some("content") => vec!["content_tok"],
        _ => {
            let mut v = Vec::new();
            if fields.name {
                v.push("name_tok");
            }
            if fields.path {
                v.push("path_tok");
            }
            if fields.content {
                v.push("content_tok");
            }
            v
        }
    }
}

fn phrase_of(text: &str, mode: &Mode) -> (Vec<String>, bool) {
    let mut toks = tokenize::query_tokens(text);
    let mut prefix = false;
    if text.ends_with('*') && !toks.is_empty() {
        prefix = true;
    }
    if mode.case_sensitive {
        toks = toks.into_iter().map(|t| t).collect();
    }
    (toks, prefix)
}

fn clause(text: &str, field: &Option<String>, fields: &Fields, mode: &Mode) -> Result<String, SnapError> {
    let cols = columns_for(field, fields);
    if cols.is_empty() {
        return Err(SnapError::new(
            ErrorCode::QuerySyntax,
            "未选择任何检索域（name/path/content）",
        ));
    }
    let (toks, prefix) = phrase_of(text, mode);
    if toks.is_empty() {
        return Ok(String::new());
    }
    let colspec = if cols.len() == 1 {
        cols[0].to_string()
    } else {
        format!("{{{}}}", cols.join(" "))
    };
    let body = if toks.len() == 1 {
        if prefix {
            format!("{}*", escape_token(&toks[0]))
        } else {
            format!("\"{}\"", toks[0])
        }
    } else {
        let joined = toks
            .iter()
            .map(|t| escape_token(t))
            .collect::<Vec<_>>()
            .join(" ");
        format!("\"{joined}\"")
    };
    Ok(format!("{colspec} : {body}"))
}

fn escape_token(t: &str) -> String {
    t.replace('"', "")
}

fn to_fts(node: &Node, fields: &Fields, mode: &Mode) -> Result<String, SnapError> {
    match node {
        Node::Term(t, f) => clause(t, f, fields, mode),
        Node::Phrase(t, f) => clause(t, f, fields, mode),
        Node::Not(inner) => Ok(format!("NOT {}", to_fts(inner, fields, mode)?)),
        Node::And(children) => {
            let mut pos = Vec::new();
            let mut neg = Vec::new();
            for c in children {
                match c {
                    Node::Not(inner) => neg.push(to_fts(inner, fields, mode)?),
                    _ => pos.push(to_fts(c, fields, mode)?),
                }
            }
            pos.retain(|s| !s.is_empty());
            if pos.is_empty() {
                return Err(SnapError::new(
                    ErrorCode::QuerySyntax,
                    "NOT 需要至少一个正项（纯排除查询不受支持）",
                ));
            }
            let mut s = format!("({})", pos.join(" AND "));
            for n in neg {
                if !n.is_empty() {
                    s = format!("({s} NOT {n})");
                }
            }
            Ok(s)
        }
        Node::Or(children) => {
            let mut parts = Vec::new();
            for c in children {
                if matches!(c, Node::Not(_)) {
                    return Err(SnapError::new(
                        ErrorCode::QuerySyntax,
                        "NOT 不能直接出现在 OR 中，请使用 (a OR b) AND NOT c",
                    ));
                }
                let s = to_fts(c, fields, mode)?;
                if !s.is_empty() {
                    parts.push(s);
                }
            }
            if parts.is_empty() {
                return Ok(String::new());
            }
            Ok(format!("({})", parts.join(" OR ")))
        }
    }
}

/// 把 AST 编译为 FTS5 表达式；返回 `None` 表示查询为空（例如只给了类型过滤）。
pub fn compile(parsed: &Parsed, fields: &Fields, mode: &Mode) -> SnapResult<Option<String>> {
    let ast = match &parsed.ast {
        Some(a) => a,
        None => return Ok(None),
    };
    if matches!(ast, Node::Not(_)) {
        return Err(SnapError::new(
            ErrorCode::QuerySyntax,
            "NOT 需要至少一个正项（纯排除查询不受支持）",
        ));
    }
    let expr = to_fts(ast, fields, mode)?;
    if expr.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(expr))
    }
}

// ---------------------------------------------------------------------------
// 执行
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SearchOutcome {
    pub hits: Vec<Hit>,
    pub total: usize,
    pub took_ms: u64,
    pub warnings: Vec<String>,
}

fn like_prefix(prefix: &str) -> String {
    let escaped = prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("{escaped}%")
}

fn build_where(filters: &Filters, types: &[String]) -> (String, Vec<Value>) {
    let mut sql = String::new();
    let mut params: Vec<Value> = Vec::new();

    let mut all_types: Vec<String> = types.to_vec();
    all_types.extend(filters.types.iter().map(|t| t.to_ascii_lowercase()));
    all_types.sort();
    all_types.dedup();

    if !all_types.is_empty() {
        sql.push_str(" AND d.ext IN (");
        for (i, t) in all_types.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push('?');
            params.push(Value::Text(t.clone()));
        }
        sql.push(')');
    }
    if !filters.exclude_types.is_empty() {
        sql.push_str(" AND d.ext NOT IN (");
        for (i, t) in filters.exclude_types.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push('?');
            params.push(Value::Text(t.to_ascii_lowercase()));
        }
        sql.push(')');
    }
    if let Some(min) = filters.size_min {
        sql.push_str(" AND d.size >= ?");
        params.push(Value::Integer(min as i64));
    }
    if let Some(max) = filters.size_max {
        sql.push_str(" AND d.size <= ?");
        params.push(Value::Integer(max as i64));
    }
    if let Some(from) = filters.mtime_from {
        sql.push_str(" AND d.mtime >= ?");
        params.push(Value::Integer(from));
    }
    if let Some(to) = filters.mtime_to {
        sql.push_str(" AND d.mtime <= ?");
        params.push(Value::Integer(to));
    }
    if !filters.paths.is_empty() {
        sql.push_str(" AND (");
        for (i, p) in filters.paths.iter().enumerate() {
            if i > 0 {
                sql.push_str(" OR ");
            }
            sql.push_str("d.path LIKE ? ESCAPE '\\'");
            params.push(Value::Text(like_prefix(p)));
        }
        sql.push(')');
    }
    (sql, params)
}

fn order_by(sort_key: &str, order: &str) -> String {
    let dir = if order.eq_ignore_ascii_case("asc") {
        "ASC"
    } else {
        "DESC"
    };
    match sort_key {
        "name" => format!("d.name COLLATE NOCASE {dir}"),
        "size" => format!("d.size {dir}"),
        "mtime" => format!("d.mtime {dir}"),
        _ => format!("rank {dir}"),
    }
}

/// 执行查询（跨卷合并）。
pub fn search(
    store: &Store,
    req: &QueryRequest,
    cancel: &AtomicBool,
) -> SnapResult<SearchOutcome> {
    let started = std::time::Instant::now();
    let mut warnings: Vec<String> = Vec::new();

    if req.mode.regex || req.mode.fuzzy {
        warnings.push("v0.1 未实现正则/模糊匹配，已按普通关键词检索".into());
    }

    let parsed = parse(&req.query)?;
    warnings.extend(parsed.warnings.clone());
    let expr = compile(&parsed, &req.fields, &req.mode)?;
    let (filter_sql, filter_params) = build_where(&req.filters, &parsed.types);
    // 只有过滤条件、没有关键词（如 `ext:txt`）：走「仅过滤」查询；
    // 关键词与过滤都没有才返回空。
    let filter_only = expr.is_none() && !filter_sql.trim().is_empty();
    if expr.is_none() && !filter_only {
        return Ok(SearchOutcome {
            hits: Vec::new(),
            total: 0,
            took_ms: started.elapsed().as_millis() as u64,
            warnings,
        });
    }

    let target_volumes: Vec<&VolumeStore> = store
        .volumes
        .values()
        .filter(|v| {
            req.filters.volumes.is_empty() || req.filters.volumes.contains(&v.info.volume_id)
        })
        .collect();

    if target_volumes.is_empty() {
        warnings.push("没有可查询的卷".into());
        return Ok(SearchOutcome {
            hits: Vec::new(),
            total: 0,
            took_ms: started.elapsed().as_millis() as u64,
            warnings,
        });
    }

    let per_volume = req.paging.offset + req.paging.limit;
    let order = order_by(&req.sort.key, &req.sort.order);

    let mut all: Vec<Hit> = Vec::new();
    let mut total = 0usize;

    for vol in target_volumes {
        if cancel.load(Ordering::Relaxed) {
            warnings.push("查询已取消".into());
            break;
        }
        let sql = match &expr {
            Some(_) => format!(
                "SELECT d.doc_id, d.path, d.name, d.ext, d.size, d.mtime,
                        bm25(fts, 2.0, 1.2, 1.0) AS rank
                 FROM fts JOIN docs d ON d.doc_id = fts.rowid
                 WHERE fts MATCH ? {filter_sql}
                 ORDER BY {order}
                 LIMIT {per_volume}"
            ),
            None => format!(
                "SELECT d.doc_id, d.path, d.name, d.ext, d.size, d.mtime,
                        0.0 AS rank
                 FROM docs d
                 WHERE 1=1 {filter_sql}
                 ORDER BY {order}
                 LIMIT {per_volume}"
            ),
        };
        let mut stmt = match vol.conn.prepare(&sql) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!("{} 查询失败: {e}", vol.info.mount));
                continue;
            }
        };
        let mut params: Vec<Value> = match &expr {
            Some(e) => vec![Value::Text(e.clone())],
            None => Vec::new(),
        };
        params.extend(filter_params.iter().cloned());

        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            let size: i64 = r.get(4)?;
            let rank: f64 = r.get(6)?;
            Ok(Hit {
                doc_id: r.get(0)?,
                volume_id: vol.info.volume_id.clone(),
                path: r.get(1)?,
                name: r.get(2)?,
                ext: r.get(3)?,
                size: size as u64,
                mtime: r.get(5)?,
                score: -rank,
                matched_fields: Vec::new(),
                snippets: Vec::new(),
                flags: HitFlags::default(),
            })
        });

        match rows {
            Ok(iter) => {
                for hit in iter.flatten() {
                    all.push(hit);
                }
            }
            Err(e) => warnings.push(format!("{} 查询失败: {e}", vol.info.mount)),
        }

        // 统计总数（用于 UI 的 totalEstimate）。
        // 完整 COUNT 需要遍历全部命中，是宽查询的主要开销之一；
        // 用 LIMIT 封顶的子查询估算，超限按上限计（UI 显示「约 N 个结果」）。
        const TOTAL_CAP: i64 = 20_000;
        let count_sql = match &expr {
            Some(_) => format!(
                "SELECT COUNT(*) FROM (
                   SELECT fts.rowid FROM fts JOIN docs d ON d.doc_id = fts.rowid
                   WHERE fts MATCH ? {filter_sql} LIMIT {TOTAL_CAP}
                 )"
            ),
            None => format!(
                "SELECT COUNT(*) FROM (
                   SELECT d.doc_id FROM docs d WHERE 1=1 {filter_sql} LIMIT {TOTAL_CAP}
                 )"
            ),
        };
        if let Ok(mut cs) = vol.conn.prepare(&count_sql) {
            let mut cparams: Vec<Value> = match &expr {
                Some(e) => vec![Value::Text(e.clone())],
                None => Vec::new(),
            };
            cparams.extend(filter_params.iter().cloned());
            if let Ok(n) = cs.query_row(rusqlite::params_from_iter(cparams.iter()), |r| {
                r.get::<_, i64>(0)
            }) {
                total += n.min(TOTAL_CAP) as usize;
            }
        }
    }

    // 全局排序（跨卷合并）
    sort_hits(&mut all, &req.sort.key, &req.sort.order);

    // 排除路径（glob，后置过滤）
    if !req.filters.exclude_paths.is_empty() {
        if let Ok(set) = build_globset(&req.filters.exclude_paths) {
            all.retain(|h| !set.is_match(&h.path));
        }
    }

    // 分页窗口
    let window: Vec<Hit> = all
        .into_iter()
        .skip(req.paging.offset)
        .take(req.paging.limit)
        .collect();

    // 相关度、高亮与片段：仅窗口内文档按需读取正文（UI 侧无需回表）
    let needles = query_needles(&req.query);
    let mut hits = Vec::with_capacity(window.len());
    for mut hit in window {
        if let Some(vol) = store.volume(&hit.volume_id) {
            if let Ok(Some((path, name, content, flags))) = vol.doc_content(hit.doc_id) {
                hit.path = path;
                hit.name = name;
                hit.flags.truncated = flags & 1 != 0;
                hit.flags.encrypted = flags & 2 != 0;
                // 精排与片段生成只基于正文前 256KB：命中词几乎都在前部，
                // 避免对最大 2MB 的正文做重复全文扫描（宽查询的主要开销之一）
                let head = &content[..floor_boundary(&content, 256 * 1024)];
                // 正文小写化一次，相关度与片段生成共用（避免重复全量分配）
                let content_lower = head.to_lowercase();
                let (score, fields) = relevance(
                    &hit.name,
                    &hit.path,
                    &content_lower,
                    &needles,
                    hit.size,
                );
                hit.score = score;
                hit.matched_fields = fields;
                if req.highlight.enable {
                    hit.snippets = make_snippets(
                        &hit.name,
                        &hit.path,
                        head,
                        &content_lower,
                        &needles,
                        &req.highlight,
                    );
                }
            }
        }
        hits.push(hit);
    }
    // 按自算相关度重排（跨卷合并后统一口径）
    if req.sort.key == "relevance" {
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    let took_ms = started.elapsed().as_millis() as u64;
    if let Some(limit) = req.timeout_ms {
        if took_ms > limit {
            warnings.push("timeout_partial".into());
        }
    }

    Ok(SearchOutcome {
        hits,
        total: total.max(0),
        took_ms,
        warnings,
    })
}

fn sort_hits(hits: &mut [Hit], key: &str, order: &str) {
    let asc = order.eq_ignore_ascii_case("asc");
    match key {
        "name" => hits.sort_by(|a, b| {
            let o = a.name.to_lowercase().cmp(&b.name.to_lowercase());
            if asc {
                o
            } else {
                o.reverse()
            }
        }),
        "size" => hits.sort_by(|a, b| {
            let o = a.size.cmp(&b.size);
            if asc {
                o
            } else {
                o.reverse()
            }
        }),
        "mtime" => hits.sort_by(|a, b| {
            let o = a.mtime.cmp(&b.mtime);
            if asc {
                o
            } else {
                o.reverse()
            }
        }),
        _ => hits.sort_by(|a, b| {
            let o = b
                .score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal);
            if asc {
                o.reverse()
            } else {
                o
            }
        }),
    }
}

fn build_globset(patterns: &[String]) -> Result<globset::GlobSet, globset::Error> {
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        b.add(globset::Glob::new(p)?);
    }
    b.build()
}

/// 从查询串提取用于高亮的词（去语法符号）。
pub fn query_needles(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in query.split_whitespace() {
        let mut w = raw.trim_matches(|c| matches!(c, '(' | ')' | '"'));
        if let Some(pos) = w.find(':') {
            if FIELDS.contains(&w[..pos].to_ascii_lowercase().as_str()) {
                w = &w[pos + 1..];
            }
        }
        let w = w.trim_start_matches(['-', '!']).trim_matches('"');
        if w.is_empty() || matches!(w.to_ascii_uppercase().as_str(), "AND" | "OR" | "NOT") {
            continue;
        }
        let cleaned = w.trim_end_matches('*');
        for t in tokenize::query_tokens(cleaned) {
            if t.chars().count() >= 1 && !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// 相关度：字段权重 × 词频（上限 5）× 短文档轻微加成。
/// SQLite 的 `bm25()` 仅用于 SQL 侧的粗排序，最终展示分由本函数统一计算。
///
/// `content` 须传入已小写的正文（调用方与片段生成共用同一次小写化）。
pub fn relevance(
    name: &str,
    path: &str,
    content_lower: &str,
    needles: &[String],
    size: u64,
) -> (f64, Vec<String>) {
    if needles.is_empty() {
        return (1.0, Vec::new());
    }
    let mut score = 0.0f64;
    let mut fields: Vec<String> = Vec::new();
    for (field, weight, text) in [("name", 3.0, name), ("path", 1.5, path)] {
        let tf = count_matches(text, needles);
        if tf > 0 {
            score += weight * (tf.min(5) as f64);
            fields.push(field.to_string());
        }
    }
    let tf = count_matches_lower(content_lower, needles);
    if tf > 0 {
        score += 1.0 * (tf.min(5) as f64);
        fields.push("content".to_string());
    }
    // 文档长度归一化：短文档命中密度更高，轻微加成
    let len_norm = 1.0 / (1.0 + (content_lower.chars().count() as f64 / 2000.0).ln_1p());
    let size_norm = 1.0 / (1.0 + (size as f64 / 1_048_576.0).ln_1p());
    score *= 0.7 + 0.2 * len_norm + 0.1 * size_norm;
    (score, fields)
}

/// 不跨字符边界的截断长度。
fn floor_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn count_matches(text: &str, needles: &[String]) -> usize {
    if text.is_empty() {
        return 0;
    }
    count_matches_lower(&text.to_lowercase(), needles)
}

fn count_matches_lower(lower: &str, needles: &[String]) -> usize {
    if lower.is_empty() {
        return 0;
    }
    let mut total = 0usize;
    for n in needles {
        let needle = n.to_lowercase();
        if needle.is_empty() {
            continue;
        }
        let mut pos = 0usize;
        while let Some(idx) = lower[pos..].find(&needle) {
            total += 1;
            pos += idx + needle.len();
            if total >= 64 {
                break;
            }
        }
    }
    total
}

/// 生成命中片段（含高亮标记），按 name → path → content 顺序。
/// `content_lower` 是已小写的正文，与相关度计算共用同一次小写化。
fn make_snippets(
    name: &str,
    path: &str,
    content: &str,
    content_lower: &str,
    needles: &[String],
    cfg: &HighlightCfg,
) -> Vec<Snippet> {
    let mut snippets = Vec::new();
    for (field, text, pre_lower) in [
        ("name", name, None),
        ("path", path, None),
        ("content", content, Some(content_lower)),
    ] {
        if text.is_empty() {
            continue;
        }
        if let Some(s) = snippet_one(field, text, pre_lower, needles, cfg) {
            snippets.push(s);
        }
    }
    snippets.truncate(cfg.max_snippets.max(1));
    snippets
}

fn snippet_one(
    field: &str,
    text: &str,
    pre_lower: Option<&str>,
    needles: &[String],
    cfg: &HighlightCfg,
) -> Option<Snippet> {
    if text.is_empty() || needles.is_empty() {
        return None;
    }
    let lower: std::borrow::Cow<str> = match pre_lower {
        Some(l) => std::borrow::Cow::Borrowed(l),
        None => std::borrow::Cow::Owned(text.to_lowercase()),
    };
    let same_len = lower.len() == text.len();

    let mut best: Option<(usize, usize, String)> = None; // (start_byte, end_byte, needle_lower)
    for n in needles {
        let nl = n.to_lowercase();
        if nl.is_empty() {
            continue;
        }
        let hay: &str = if same_len { &lower } else { text };
        let needle = if same_len { nl.as_str() } else { n.as_str() };
        if let Some(pos) = hay.find(needle) {
            let end = pos + needle.len();
            if best.as_ref().map(|b| pos < b.0).unwrap_or(true) {
                best = Some((pos, end, n.clone()));
            }
        }
    }
    let (start, end, _) = best?;

    // 取上下文窗口（按字符边界扩展）
    let half = cfg.snippet_len / 2;
    let mut s = start;
    let mut count = 0usize;
    while s > 0 && count < half {
        s -= 1;
        while s > 0 && !text.is_char_boundary(s) {
            s -= 1;
        }
        count = start - s;
    }
    let mut e = end;
    let mut count = 0usize;
    while e < text.len() && count < half {
        e += 1;
        while e < text.len() && !text.is_char_boundary(e) {
            e += 1;
        }
        count = e - end;
    }

    let raw = &text[s..e];
    // 转义 + 高亮
    let mut out = String::with_capacity(raw.len() + 32);
    let mut offsets: Vec<(u32, u32)> = Vec::new();
    let mut i = 0usize;
    let raw_lower = raw.to_lowercase();
    let raw_same = raw_lower.len() == raw.len();
    let mut sorted: Vec<&String> = needles.iter().collect();
    sorted.sort_by_key(|n| std::cmp::Reverse(n.len()));

    while i < raw.len() {
        let mut matched: Option<(usize, &String)> = None;
        if raw_same {
            for n in &sorted {
                let nl = n.to_lowercase();
                if raw_lower[i..].starts_with(&nl) {
                    matched = Some((nl.len(), n));
                    break;
                }
            }
        }
        if let Some((len, _)) = matched {
            let piece = &raw[i..i + len];
            let esc_start = out.len();
            out.push_str(&cfg.pre);
            out.push_str(&escape_html(piece));
            let esc_end = out.len();
            out.push_str(&cfg.post);
            let _ = esc_end;
            let u16_start = utf16_len(&out[..esc_start]);
            let u16_end = u16_start + utf16_len(piece);
            offsets.push((u16_start, u16_end));
            i += len;
        } else {
            let ch_len = utf8_char_len(raw, i);
            out.push_str(&escape_html(&raw[i..i + ch_len]));
            i += ch_len;
        }
    }

    let prefix = if s > 0 { "…" } else { "" };
    let suffix = if e < text.len() { "…" } else { "" };
    Some(Snippet {
        field: field.to_string(),
        text: format!("{prefix}{out}{suffix}"),
        offsets,
    })
}

fn utf8_char_len(s: &str, i: usize) -> usize {
    let b = s.as_bytes()[i];
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

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_boolean() {
        let p = parse("foo AND (bar OR \"baz qux\") NOT *.log").unwrap();
        let expr = compile(&p, &Fields::default(), &Mode::default())
            .unwrap()
            .unwrap();
        assert!(expr.contains("AND"));
        assert!(expr.contains("OR"));
        assert!(expr.contains("NOT"));
    }

    #[test]
    fn parse_field_and_type() {
        let p = parse("name:report ext:pdf").unwrap();
        assert_eq!(p.types, vec!["pdf".to_string()]);
        let expr = compile(&p, &Fields::default(), &Mode::default())
            .unwrap()
            .unwrap();
        assert!(expr.contains("name_tok"));
    }

    #[test]
    fn syntax_error_unmatched_paren() {
        let err = parse("(foo AND bar").unwrap_err();
        assert_eq!(err.code, ErrorCode::QuerySyntax);
    }

    #[test]
    fn pure_negative_rejected() {
        let p = parse("NOT foo").unwrap();
        let err = compile(&p, &Fields::default(), &Mode::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::QuerySyntax);
    }

    #[test]
    fn cjk_query_becomes_phrase() {
        let p = parse("季度报表").unwrap();
        let expr = compile(&p, &Fields::default(), &Mode::default())
            .unwrap()
            .unwrap();
        assert!(expr.contains("\"季度 度报 报表\""));
    }

    #[test]
    fn snippet_highlights_and_escapes() {
        let cfg = HighlightCfg::default();
        let s = snippet_one(
            "content",
            "前面 <script> 危险 foo 后面内容",
            None,
            &["foo".into()],
            &cfg,
        )
        .unwrap();
        assert!(s.text.contains("<mark>foo</mark>"));
        assert!(s.text.contains("&lt;script&gt;"));
    }

    /// 只给类型过滤、没有关键词（`ext:txt`）也要返回结果，不能直接空。
    #[test]
    fn type_only_query_uses_filter_path() {
        use crate::store::{DocRecord, Store};
        let dir = std::env::temp_dir().join(format!(
            "snapfind-q-ext-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = snapfind_core::config::resolve_index_root(Some(&dir));
        let mut store = Store::open(&root).unwrap();
        let info = snapfind_core::volume::VolumeInfo {
            volume_id: "vol-test".into(),
            mount: "D:\\".into(),
            fs: "NTFS".into(),
            total_bytes: 0,
            free_bytes: 0,
            removable: false,
            network: false,
            enabled: true,
        };
        store.register_volume(&info).unwrap();
        store
            .volume_mut(&info)
            .unwrap()
            .upsert_doc(&DocRecord {
                path: "D:\\a.txt".into(),
                parent: "D:\\".into(),
                name: "a.txt".into(),
                ext: "txt".into(),
                size: 3,
                mtime: 0,
                encoding: "utf-8".into(),
                flags: 0,
                content: "abc".into(),
                name_tok: "a txt".into(),
                path_tok: "d a txt".into(),
                content_tok: "abc".into(),
            })
            .unwrap();

        let req = QueryRequest {
            request_id: "t".into(),
            query: "ext:txt".into(),
            ..Default::default()
        };
        let out = search(&store, &req, &AtomicBool::new(false)).unwrap();
        assert_eq!(out.hits.len(), 1, "ext:txt 应命中 1 条");
        assert_eq!(out.hits[0].name, "a.txt");
        assert_eq!(out.total, 1);

        // 关键词 + 类型同时给出仍然走 FTS
        let req2 = QueryRequest {
            request_id: "t2".into(),
            query: "abc ext:txt".into(),
            ..Default::default()
        };
        let out2 = search(&store, &req2, &AtomicBool::new(false)).unwrap();
        assert_eq!(out2.hits.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
