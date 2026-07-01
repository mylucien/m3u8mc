//! 轻量级 UCI 配置文件解析器。
//!
//! 只覆盖 UCI 文本格式里实际用到的三种结构：`config`/`option`/`list`，
//! 不依赖 libuci，避免交叉编译时引入 FFI 绑定的复杂度。
//!
//! 支持单引号、双引号、无引号三种取值写法，支持 `#` 行注释。

use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Default)]
pub struct UciSection {
    /// section 类型，如 "channel"、"global"
    pub section_type: String,
    /// section 名（匿名 section 时为 None）
    pub name: Option<String>,
    /// 单值字段
    pub options: HashMap<String, String>,
    /// 多值字段（list），保留插入顺序
    pub lists: HashMap<String, Vec<String>>,
}

impl UciSection {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.options.get(key).map(|s| s.as_str())
    }

    pub fn get_or(&self, key: &str, default: &str) -> String {
        self.options
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.options.get(key).map(|s| s.as_str()) {
            Some("1") | Some("true") | Some("yes") | Some("on") => true,
            Some("0") | Some("false") | Some("no") | Some("off") => false,
            _ => default,
        }
    }

    pub fn get_parsed<T: std::str::FromStr>(&self, key: &str, default: T) -> T {
        self.options
            .get(key)
            .and_then(|s| s.parse::<T>().ok())
            .unwrap_or(default)
    }

    pub fn list(&self, key: &str) -> &[String] {
        self.lists
            .get(key)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Default)]
pub struct UciConfig {
    pub sections: Vec<UciSection>,
}

impl UciConfig {
    pub fn sections_of<'a>(&'a self, section_type: &'a str) -> impl Iterator<Item = &'a UciSection> {
        self.sections
            .iter()
            .filter(move |s| s.section_type == section_type)
    }

    pub fn section_named<'a>(&'a self, section_type: &str, name: &str) -> Option<&'a UciSection> {
        self.sections
            .iter()
            .find(|s| s.section_type == section_type && s.name.as_deref() == Some(name))
    }

    pub fn first_of<'a>(&'a self, section_type: &'a str) -> Option<&'a UciSection> {
        self.sections_of(section_type).next()
    }
}

#[derive(Debug)]
pub enum UciParseError {
    UnexpectedToken { line: usize, content: String },
    OptionBeforeSection { line: usize },
    UnterminatedString { line: usize },
}

impl fmt::Display for UciParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UciParseError::UnexpectedToken { line, content } => {
                write!(f, "line {line}: unexpected token: {content}")
            }
            UciParseError::OptionBeforeSection { line } => {
                write!(f, "line {line}: option/list appears before any config section")
            }
            UciParseError::UnterminatedString { line } => {
                write!(f, "line {line}: unterminated quoted string")
            }
        }
    }
}

impl std::error::Error for UciParseError {}

/// 把一行按空白切分为 token，token 可以是裸词，也可以是 '...' 或 "..." 包裹的字符串
/// （内部不支持转义，UCI 配置里的取值一般不需要）。
fn tokenize_line(line: &str, line_no: usize) -> Result<Vec<String>, UciParseError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' {
            break; // 行内注释，剩余部分忽略
        }
        if c == '\'' || c == '"' {
            let quote = c;
            let mut buf = String::new();
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == quote {
                    closed = true;
                    i += 1;
                    break;
                }
                buf.push(chars[i]);
                i += 1;
            }
            if !closed {
                return Err(UciParseError::UnterminatedString { line: line_no });
            }
            tokens.push(buf);
        } else {
            let mut buf = String::new();
            while i < chars.len() && !chars[i].is_whitespace() {
                buf.push(chars[i]);
                i += 1;
            }
            tokens.push(buf);
        }
    }
    Ok(tokens)
}

pub fn parse(input: &str) -> Result<UciConfig, UciParseError> {
    let mut config = UciConfig::default();
    let mut current: Option<UciSection> = None;

    for (idx, raw_line) in input.lines().enumerate() {
        let line_no = idx + 1;
        let tokens = tokenize_line(raw_line, line_no)?;
        if tokens.is_empty() {
            continue;
        }
        match tokens[0].as_str() {
            "config" => {
                if let Some(sec) = current.take() {
                    config.sections.push(sec);
                }
                let section_type = tokens.get(1).cloned().unwrap_or_default();
                let name = tokens.get(2).cloned();
                current = Some(UciSection {
                    section_type,
                    name,
                    ..Default::default()
                });
            }
            "option" => {
                let sec = current
                    .as_mut()
                    .ok_or(UciParseError::OptionBeforeSection { line: line_no })?;
                let key = tokens.get(1).cloned().unwrap_or_default();
                let val = tokens.get(2).cloned().unwrap_or_default();
                sec.options.insert(key, val);
            }
            "list" => {
                let sec = current
                    .as_mut()
                    .ok_or(UciParseError::OptionBeforeSection { line: line_no })?;
                let key = tokens.get(1).cloned().unwrap_or_default();
                let val = tokens.get(2).cloned().unwrap_or_default();
                sec.lists.entry(key).or_default().push(val);
            }
            other => {
                return Err(UciParseError::UnexpectedToken {
                    line: line_no,
                    content: other.to_string(),
                });
            }
        }
    }
    if let Some(sec) = current.take() {
        config.sections.push(sec);
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_sections() {
        let input = r#"
config global
    option enabled '1'
    option log_level 'warning'

config channel 'sport1'
    option enabled '1'
    option name '澳视体育'
    list url 'http://a/playlist.m3u8'
    list url 'http://b/playlist.m3u8'
"#;
        let cfg = parse(input).unwrap();
        assert_eq!(cfg.sections.len(), 2);
        let global = cfg.first_of("global").unwrap();
        assert_eq!(global.get("enabled"), Some("1"));
        let ch = cfg.section_named("channel", "sport1").unwrap();
        assert_eq!(ch.get("name"), Some("澳视体育"));
        assert_eq!(ch.list("url").len(), 2);
    }

    #[test]
    fn rejects_option_before_section() {
        let input = "option foo 'bar'";
        assert!(parse(input).is_err());
    }
}
