//! 分词：索引与查询必须使用同一套规则（文档 §7.6）。
//!
//! * 西文：小写折叠，代码标识符按 `snake_case` / `camelCase` 拆分，同时保留整词。
//! * 中日韩：bigram 滑窗（单字串保留单字）。
//! * 输出为空格分隔的 token 串，直接写入 FTS5 的 `*_tok` 列。

const MAX_TOKENS: usize = 400_000;

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF     // 日文假名
        | 0x3400..=0x4DBF   // CJK 扩展 A
        | 0x4E00..=0x9FFF   // CJK 基本区
        | 0xF900..=0xFAFF   // 兼容表意
        | 0xAC00..=0xD7AF   // 韩文音节
        | 0x20000..=0x2FA1F // 扩展 B+
    )
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || (!c.is_ascii() && c.is_alphabetic() && !is_cjk(c))
}

/// 把文本切分为 token 序列。
pub fn tokens_of(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if is_cjk(c) {
            let start = i;
            while i < chars.len() && is_cjk(chars[i]) {
                i += 1;
            }
            let run = &chars[start..i];
            if run.len() == 1 {
                out.push(run[0].to_string());
            } else {
                for w in run.windows(2) {
                    out.push(format!("{}{}", w[0], w[1]));
                }
            }
        } else if is_word_char(c) {
            let start = i;
            while i < chars.len() && is_word_char(chars[i]) {
                i += 1;
            }
            let run: String = chars[start..i].iter().collect();
            push_latin(&mut out, &run);
        } else {
            i += 1;
        }
        if out.len() >= MAX_TOKENS {
            break;
        }
    }
    out.truncate(MAX_TOKENS);
    out
}

fn push_latin(out: &mut Vec<String>, run: &str) {
    let lower = run.to_lowercase();
    out.push(lower.clone());
    // camelCase / PascalCase 拆分子词
    if run.chars().any(|c| c.is_ascii_uppercase()) {
        let chars: Vec<char> = run.chars().collect();
        let mut start = 0usize;
        for idx in 1..chars.len() {
            let prev = chars[idx - 1];
            let cur = chars[idx];
            let boundary = (prev.is_ascii_lowercase() || prev.is_ascii_digit())
                && cur.is_ascii_uppercase();
            if boundary && idx > start {
                let part: String = chars[start..idx].iter().collect();
                if part.to_lowercase() != lower {
                    out.push(part.to_lowercase());
                }
                start = idx;
            }
        }
        if start > 0 {
            let part: String = chars[start..].iter().collect();
            if part.to_lowercase() != lower {
                out.push(part.to_lowercase());
            }
        }
    }
}

/// 生成可写入 FTS5 列的空格分隔 token 串。
///
/// 输出与 `tokens_of(text).join(" ")` 严格一致（一致性由测试保证），
/// 但单遍流式写入：CJK bigram 直接 push 到结果串，
/// 避免为每个 bigram 分配一个 `String`（中文密集文档曾是百万级堆分配）。
pub fn token_string(text: &str) -> String {
    let mut s = String::with_capacity(text.len() / 2 + 16);
    let mut count = 0usize;
    let mut run = String::new();
    let mut prev_cjk: Option<char> = None;
    let mut cjk_paired = false;

    for c in text.chars() {
        if is_cjk(c) {
            if !run.is_empty() {
                push_latin_str(&mut s, &mut count, &run);
                run.clear();
            }
            if let Some(p) = prev_cjk {
                if count > 0 {
                    s.push(' ');
                }
                count += 1;
                s.push(p);
                s.push(c);
                cjk_paired = true;
            }
            prev_cjk = Some(c);
        } else {
            // CJK run 结束：未配对的单字单独成 token（与 windows(2) 语义一致）
            if let Some(p) = prev_cjk {
                if !cjk_paired {
                    if count > 0 {
                        s.push(' ');
                    }
                    count += 1;
                    s.push(p);
                }
                prev_cjk = None;
                cjk_paired = false;
            }
            if is_word_char(c) {
                run.push(c);
            } else if !run.is_empty() {
                push_latin_str(&mut s, &mut count, &run);
                run.clear();
            }
        }
        if count >= MAX_TOKENS {
            break;
        }
    }
    if let Some(p) = prev_cjk {
        if !cjk_paired {
            if count > 0 {
                s.push(' ');
            }
            s.push(p);
        }
    }
    if !run.is_empty() {
        push_latin_str(&mut s, &mut count, &run);
    }
    s
}

fn emit_str(s: &mut String, count: &mut usize, tok: &str) {
    if *count > 0 {
        s.push(' ');
    }
    s.push_str(tok);
    *count += 1;
}

/// `push_latin` 的流式版本：直接写入结果串而非中间 Vec。
fn push_latin_str(s: &mut String, count: &mut usize, run: &str) {
    let lower = run.to_lowercase();
    emit_str(s, count, &lower);
    // camelCase / PascalCase 拆分子词
    if run.chars().any(|c| c.is_ascii_uppercase()) {
        let chars: Vec<char> = run.chars().collect();
        let mut start = 0usize;
        for idx in 1..chars.len() {
            let prev = chars[idx - 1];
            let cur = chars[idx];
            let boundary = (prev.is_ascii_lowercase() || prev.is_ascii_digit())
                && cur.is_ascii_uppercase();
            if boundary && idx > start {
                let part: String = chars[start..idx].iter().collect();
                if part.to_lowercase() != lower {
                    emit_str(s, count, &part.to_lowercase());
                }
                start = idx;
            }
        }
        if start > 0 {
            let part: String = chars[start..].iter().collect();
            if part.to_lowercase() != lower {
                emit_str(s, count, &part.to_lowercase());
            }
        }
    }
}

/// 查询词分词：保持顺序，用于短语/多 bigram 组合。
pub fn query_tokens(term: &str) -> Vec<String> {
    let mut toks = tokens_of(term);
    tok_take(&mut toks, 64);
    toks
}

fn tok_take(v: &mut Vec<String>, n: usize) {
    if v.len() > n {
        v.truncate(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_lowercase_and_split() {
        assert_eq!(tokens_of("Hello"), vec!["hello"]);
        let t = tokens_of("snake_case");
        assert_eq!(t, vec!["snake", "case"]);
        let t = tokens_of("getUserName");
        assert!(t.contains(&"getusername".to_string()));
        assert!(t.contains(&"get".to_string()));
        assert!(t.contains(&"user".to_string()));
    }

    #[test]
    fn cjk_bigrams() {
        assert_eq!(tokens_of("季度报表"), vec!["季度", "度报", "报表"]);
        assert_eq!(tokens_of("表"), vec!["表"]);
    }

    #[test]
    fn token_string_roundtrip() {
        let s = token_string("Hello 世界");
        assert!(s.contains("hello"));
        assert!(s.contains("世界"));
    }

    /// 流式版 `token_string` 必须与 `tokens_of` 严格一致。
    #[test]
    fn token_string_matches_tokens_of() {
        for text in [
            "",
            "表",
            "中文",
            "中文字",
            "a中b文",
            "Hello 世界 mixed getUserName snake_case 中文ABC混排 123abc",
            "季度报表\n第二行\tABC_defGHI",
        ] {
            assert_eq!(token_string(text), tokens_of(text).join(" "), "{text:?}");
        }
    }
}
