//! Minimal UCI (`/etc/config/*`) reader/writer: `config`, `option`, `list`, quotes and comments.
//! Unknown sections and options are preserved when a file is rewritten.
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Single(String),
    List(Vec<String>),
}
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub kind: String,
    pub name: Option<String>,
    pub options: Vec<(String, Value)>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Package {
    pub sections: Vec<Section>,
}

fn words(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&c) = chars.peek() else { break };
        if c == '#' {
            break;
        }
        let mut w = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            chars.next();
            match c {
                '\'' => loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => w.push(c),
                        None => return Err("unterminated quote".into()),
                    }
                },
                '"' => loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            if let Some(c) = chars.next() {
                                w.push(c)
                            }
                        }
                        Some(c) => w.push(c),
                        None => return Err("unterminated quote".into()),
                    }
                },
                '\\' => {
                    if let Some(c) = chars.next() {
                        w.push(c)
                    }
                }
                c => w.push(c),
            }
        }
        out.push(w);
    }
    Ok(out)
}
fn quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

impl Package {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut p = Package::default();
        for (n, line) in text.lines().enumerate() {
            let w = words(line).map_err(|e| format!("line {}: {e}", n + 1))?;
            match w.first().map(String::as_str) {
                None => {}
                Some("package") => {}
                Some("config") => {
                    let kind = w.get(1).ok_or(format!("line {}: config without type", n + 1))?.clone();
                    p.sections.push(Section { kind, name: w.get(2).cloned(), options: Vec::new() });
                }
                Some(k @ ("option" | "list")) => {
                    let s = p.sections.last_mut().ok_or(format!("line {}: {k} outside a section", n + 1))?;
                    let (Some(name), Some(value)) = (w.get(1), w.get(2)) else {
                        return Err(format!("line {}: incomplete {k}", n + 1));
                    };
                    if k == "option" {
                        s.options.retain(|(o, _)| o != name);
                        s.options.push((name.clone(), Value::Single(value.clone())));
                    } else {
                        match s.options.iter_mut().find(|(o, _)| o == name) {
                            Some((_, Value::List(l))) => l.push(value.clone()),
                            _ => {
                                s.options.retain(|(o, _)| o != name);
                                s.options.push((name.clone(), Value::List(vec![value.clone()])));
                            }
                        }
                    }
                }
                Some(other) => return Err(format!("line {}: unknown keyword {other}", n + 1)),
            }
        }
        Ok(p)
    }
    pub fn read(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => Self::parse(&t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.to_string()),
        }
    }
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for s in &self.sections {
            out.push_str("config ");
            out.push_str(&s.kind);
            if let Some(n) = &s.name {
                out.push(' ');
                out.push_str(&quote(n));
            }
            out.push('\n');
            for (k, v) in &s.options {
                match v {
                    Value::Single(v) => out.push_str(&format!("\toption {k} {}\n", quote(v))),
                    Value::List(l) => {
                        for v in l {
                            out.push_str(&format!("\tlist {k} {}\n", quote(v)));
                        }
                    }
                }
            }
            out.push('\n');
        }
        out
    }
    /// Atomic, durable replace, keeping the file world-readable like other UCI files.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        f50_server::settings::replace(path, self.serialize().as_bytes(), 0o644)
    }
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name.as_deref() == Some(name))
    }
    pub fn get(&self, section: &str, option: &str) -> Option<&str> {
        self.section(section)?.get(option)
    }
    /// Set an option, creating the named section with `kind` when missing.
    pub fn set(&mut self, kind: &str, section: &str, option: &str, value: &str) {
        let s = match self.sections.iter().position(|s| s.name.as_deref() == Some(section)) {
            Some(i) => &mut self.sections[i],
            None => {
                self.sections.push(Section { kind: kind.into(), name: Some(section.into()), options: Vec::new() });
                self.sections.last_mut().unwrap()
            }
        };
        match s.options.iter_mut().find(|(o, _)| o == option) {
            Some((_, v)) => *v = Value::Single(value.into()),
            None => s.options.push((option.into(), Value::Single(value.into()))),
        }
    }
}
impl Section {
    pub fn get(&self, option: &str) -> Option<&str> {
        self.options.iter().find_map(|(o, v)| match v {
            Value::Single(v) if o == option => Some(v.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_quotes_lists_comments_and_round_trip() {
        let text = "# comment\nconfig cowplay 'main'\n\toption port '18443'\n\toption domain \"car.example\" # trailing\n\tlist dns 1.1.1.1\n\tlist dns '8.8.8.8'\nconfig wifi-iface\n\toption ssid 'it'\\''s'\n";
        let p = Package::parse(text).unwrap();
        assert_eq!(p.get("main", "port"), Some("18443"));
        assert_eq!(p.get("main", "domain"), Some("car.example"));
        assert_eq!(p.sections[0].options[2].1, Value::List(vec!["1.1.1.1".into(), "8.8.8.8".into()]));
        assert_eq!(p.sections[1].get("ssid"), Some("it's"));
        let again = Package::parse(&p.serialize()).unwrap();
        assert_eq!(again, p);
    }
    #[test]
    fn set_creates_sections_and_rejects_garbage() {
        let mut p = Package::default();
        p.set("cowplay", "main", "ask_when_multi", "0");
        p.set("cowplay", "main", "ask_when_multi", "1");
        assert_eq!(p.get("main", "ask_when_multi"), Some("1"));
        assert_eq!(p.sections.len(), 1);
        assert!(Package::parse("option x 'y'").is_err());
        assert!(Package::parse("config a\n\toption x 'y").is_err());
        assert!(Package::parse("frobnicate").is_err());
    }
}
