//! The bounded Atelier source grammar: KDL-style nodes, scalar properties,
//! child blocks, comments, and multiline strings. This deliberately supports
//! the documented domain profile, rather than KDL annotations or extensions.
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(super) struct Node {
    pub name: String,
    pub args: Vec<Value>,
    pub props: BTreeMap<String, Value>,
    pub children: Vec<Node>,
    pub start: usize,
    pub end: usize,
    pub body: Option<(usize, usize)>,
}
impl Node {
    pub fn same(&self, other: &Self) -> bool {
        self.header(other)
            && self.children.len() == other.children.len()
            && self
                .children
                .iter()
                .zip(&other.children)
                .all(|(a, b)| a.same(b))
            && self.body.is_some() == other.body.is_some()
    }
    pub fn header(&self, other: &Self) -> bool {
        self.name == other.name && self.args == other.args && self.props == other.props
    }
}
pub(super) fn parse(text: &str) -> Result<Vec<Node>, String> {
    if text.len() as u64 > atelier_core::source::MAX_SOURCE_BYTES {
        return Err("recipe exceeds 16 MiB".into());
    }
    let mut reader = Reader {
        text,
        at: 0,
        nodes: 0,
    };
    reader.document(0, false).map_err(|e| {
        let prefix = &text[..reader.at.min(text.len())];
        let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        format!("source line {line}, column {column}: {e}")
    })
}
struct Reader<'a> {
    text: &'a str,
    at: usize,
    nodes: usize,
}
impl Reader<'_> {
    fn rest(&self) -> &str {
        &self.text[self.at..]
    }
    fn skip(&mut self, newlines: bool) -> Result<(), String> {
        loop {
            if let Some(c) = self.rest().chars().next()
                && (c == ' ' || c == '\t' || c == '\r' || (newlines && (c == '\n' || c == ';')))
            {
                self.at += c.len_utf8();
                continue;
            }
            if self.rest().starts_with("/*") {
                self.at += 2;
                let mut depth = 1usize;
                while depth != 0 {
                    if self.rest().starts_with("/*") {
                        depth += 1;
                        self.at += 2;
                    } else if self.rest().starts_with("*/") {
                        depth -= 1;
                        self.at += 2;
                    } else if let Some(c) = self.rest().chars().next() {
                        self.at += c.len_utf8();
                    } else {
                        return Err("unterminated block comment".into());
                    }
                    if depth > 32 {
                        return Err("comment nesting exceeds 32".into());
                    }
                }
                continue;
            }
            if self.rest().starts_with("//") {
                self.at += self.rest().find('\n').unwrap_or(self.rest().len());
                continue;
            }
            return Ok(());
        }
    }
    fn document(&mut self, depth: usize, nested: bool) -> Result<Vec<Node>, String> {
        if depth > 32 {
            return Err("child nesting exceeds 32".into());
        }
        let mut nodes = vec![];
        loop {
            let start = self.at;
            self.skip(true)?;
            if self.rest().is_empty() {
                if nested {
                    return Err("unclosed child block".into());
                }
                return Ok(nodes);
            }
            if self.rest().starts_with('}') {
                if !nested {
                    return Err("unexpected closing brace".into());
                }
                return Ok(nodes);
            }
            self.nodes += 1;
            if self.nodes > 200_000 {
                return Err("too many source nodes".into());
            }
            let name = self.name()?;
            let mut node = Node {
                name,
                args: vec![],
                props: BTreeMap::new(),
                children: vec![],
                start,
                end: 0,
                body: None,
            };
            loop {
                self.skip(false)?;
                match self.rest().chars().next() {
                    Some('{') => {
                        self.at += 1;
                        let begin = self.at;
                        node.children = self.document(depth + 1, true)?;
                        node.body = Some((begin, self.at));
                        self.at += 1;
                        self.skip(false)?;
                        match self.rest().chars().next() {
                            Some('\n' | ';') => self.at += 1,
                            Some('}') | None => {}
                            _ => return Err("expected a newline after child block".into()),
                        }
                        break;
                    }
                    Some('\n' | ';') => {
                        self.at += 1;
                        break;
                    }
                    Some('}') | None => break,
                    _ => {
                        let saved = self.at;
                        if let Ok(key) = self.name() {
                            self.skip(false)?;
                            if self.rest().starts_with('=') {
                                self.at += 1;
                                self.skip(false)?;
                                let value = self.value()?;
                                if node.props.insert(key.clone(), value).is_some() {
                                    return Err(format!("duplicate property '{key}'"));
                                }
                                continue;
                            }
                        }
                        self.at = saved;
                        node.args.push(self.value()?);
                    }
                }
            }
            node.end = self.at;
            nodes.push(node);
        }
    }
    fn name(&mut self) -> Result<String, String> {
        let length = self
            .rest()
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'-'))
            .count();
        if length == 0 || !self.rest().as_bytes()[0].is_ascii_alphabetic() {
            return Err("expected a node or property name".into());
        }
        let result = self.rest()[..length].to_owned();
        self.at += length;
        Ok(result)
    }
    fn value(&mut self) -> Result<Value, String> {
        if self.rest().starts_with('"') {
            return self.string().map(Value::String);
        }
        let length = self
            .rest()
            .bytes()
            .take_while(|b| !b.is_ascii_whitespace() && !matches!(*b, b';' | b'{' | b'}' | b'='))
            .count();
        if length == 0 {
            return Err("expected a scalar value".into());
        }
        let token = &self.rest()[..length];
        let value = match token {
            "#true" => Value::Bool(true),
            "#false" => Value::Bool(false),
            "#null" => Value::Null,
            _ => {
                let v: Value =
                    serde_json::from_str(token).map_err(|_| format!("invalid scalar '{token}'"))?;
                if !v.is_number() {
                    return Err("strings must be quoted; use #true and #false for booleans".into());
                }
                v
            }
        };
        self.at += length;
        Ok(value)
    }
    fn string(&mut self) -> Result<String, String> {
        if self.rest().starts_with("\"\"\"") {
            self.at += 3;
            if self.rest().starts_with("\r\n") {
                self.at += 2;
            } else if self.rest().starts_with('\n') {
                self.at += 1;
            } else {
                return Err("multiline strings start on the next line".into());
            }
            let end = self
                .rest()
                .find("\"\"\"")
                .ok_or("unclosed multiline string")?;
            let raw = &self.rest()[..end];
            let (content, indent) = raw
                .rsplit_once('\n')
                .ok_or("multiline closing quotes need their own line")?;
            if !indent.bytes().all(|b| matches!(b, b' ' | b'\t')) {
                return Err("multiline closing quotes need their own line".into());
            }
            let mut result = String::new();
            for line in content.split('\n') {
                if !line.trim().is_empty() && !line.starts_with(indent) {
                    return Err("multiline row is less indented than its closing quotes".into());
                }
                result.push_str(line.strip_prefix(indent).unwrap_or(""));
                result.push('\n');
            }
            self.at += end + 3;
            return Ok(result);
        }
        self.at += 1;
        let mut out = String::new();
        while let Some(c) = self.rest().chars().next() {
            self.at += c.len_utf8();
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = self.rest().chars().next().ok_or("unterminated escape")?;
                    self.at += e.len_utf8();
                    out.push(match e {
                        '"' => '"',
                        '\\' => '\\',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        'u' => {
                            if !self.rest().starts_with('{') {
                                return Err("Unicode escape requires braces".into());
                            }
                            self.at += 1;
                            let end = self.rest().find('}').ok_or("unclosed Unicode escape")?;
                            let digits = &self.rest()[..end];
                            if digits.is_empty()
                                || digits.len() > 6
                                || !digits.bytes().all(|b| b.is_ascii_hexdigit())
                            {
                                return Err("invalid Unicode escape".into());
                            }
                            let code = u32::from_str_radix(digits, 16)
                                .map_err(|_| "invalid Unicode escape")?;
                            self.at += end + 1;
                            char::from_u32(code).ok_or("invalid Unicode scalar")?
                        }
                        _ => return Err("unsupported string escape".into()),
                    });
                }
                '\n' | '\r' => return Err("use a multiline string for line breaks".into()),
                c if c.is_control() => return Err("unescaped control character".into()),
                c => out.push(c),
            }
        }
        Err("unclosed quoted string".into())
    }
}
pub(super) fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
