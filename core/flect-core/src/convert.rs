//! Data-format conversion (CSV / JSON / XML / INI -> JSON / XML / CSV / HTML / MD),
//! ported from the Windows `TextConvert.cs`. Everything goes through a JSON tree.
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use serde_json::{Map, Value};

use crate::text::TextError;

const DETECTION_WINDOW: usize = 64 * 1024;
const DELIMITERS: [char; 4] = [',', ';', '\t', '|'];
/// Deeper documents are refused instead of risking stack exhaustion.
const MAX_DEPTH: usize = 128;

pub struct Parsed {
    pub tree: Value,
    /// Set when a CSV used something other than a comma (shown to the user).
    pub delimiter: Option<char>,
}

pub fn parse(source_ext: &str, text: &str) -> Result<Parsed, TextError> {
    let text = text.trim_start_matches('\u{feff}');
    let plain = |tree| Parsed {
        tree,
        delimiter: None,
    };
    match source_ext.to_uppercase().as_str() {
        "JSON" => crate::text::parse_json(text).map(plain),
        "CSV" => from_csv(text),
        "XML" => from_xml(text).map(plain),
        "INI" | "CONF" | "CFG" => Ok(plain(from_ini(text))),
        other => Err(TextError::Msg("msg.cannotConvertFrom", other.to_owned())),
    }
}

pub fn render(tree: &Value, target: &str) -> Result<String, TextError> {
    match target.to_uppercase().as_str() {
        "JSON" => Ok(crate::text::pretty_json(tree)),
        "XML" => Ok(to_xml(tree)),
        "CSV" => Ok(to_csv(&as_table(tree)?)),
        "HTML" => Ok(to_html(&as_table(tree)?)),
        "MD" => Ok(to_markdown(&as_table(tree)?)),
        other => Err(TextError::Msg("msg.cannotConvertTo", other.to_owned())),
    }
}

// ------------------------------------------------------------------ CSV in

fn split_csv(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), String::new());
    let (mut quoted, mut any) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if quoted {
            if c != '"' {
                field.push(c);
            } else if chars.get(i + 1) == Some(&'"') {
                field.push('"');
                i += 1;
            } else {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
            any = true;
        } else if c == delimiter {
            row.push(std::mem::take(&mut field));
            any = true;
        } else if c == '\r' || c == '\n' {
            if c == '\r' && chars.get(i + 1) == Some(&'\n') {
                i += 1;
            }
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
            any = false;
        } else {
            field.push(c);
            any = true;
        }
        i += 1;
    }
    if any || !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

fn consistent_columns(text: &str, delimiter: char) -> usize {
    let rows: Vec<Vec<String>> = split_csv(text, delimiter)
        .into_iter()
        .filter(|r| !(r.len() == 1 && r[0].is_empty()))
        .take(10)
        .collect();
    match rows.first() {
        Some(first) if rows.iter().all(|r| r.len() == first.len()) => first.len(),
        _ => 0,
    }
}

/// The delimiter that most often follows a closing quote.
fn quote_vote(text: &str) -> Option<char> {
    let chars: Vec<char> = text.chars().collect();
    let mut votes: Vec<(char, usize)> = Vec::new();
    let mut in_quotes = false;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            if !in_quotes {
                in_quotes = true;
            } else if chars.get(i + 1) == Some(&'"') {
                i += 1;
            } else {
                in_quotes = false;
                if let Some(&n) = chars.get(i + 1).filter(|n| DELIMITERS.contains(n)) {
                    match votes.iter_mut().find(|(d, _)| *d == n) {
                        Some(v) => v.1 += 1,
                        None => votes.push((n, 1)),
                    }
                }
            }
        }
        i += 1;
    }
    votes.into_iter().max_by_key(|(_, n)| *n).map(|(d, _)| d)
}

pub fn detect_delimiter(text: &str) -> char {
    let end = text
        .char_indices()
        .nth(DETECTION_WINDOW)
        .map_or(text.len(), |(i, _)| i);
    let window = &text[..end];
    let consistent: Vec<(char, usize)> = DELIMITERS
        .iter()
        .map(|&d| (d, consistent_columns(window, d)))
        .filter(|(_, n)| *n > 1)
        .collect();
    if let Some(q) = quote_vote(window) {
        if consistent.iter().any(|(d, _)| *d == q) {
            return q;
        }
    }
    consistent
        .iter()
        .max_by_key(|(_, n)| *n)
        .map_or(',', |(d, _)| *d)
}

fn from_csv(text: &str) -> Result<Parsed, TextError> {
    let delimiter = detect_delimiter(text);
    let rows = split_csv(text, delimiter);
    let Some(headers) = rows.first() else {
        return Err(TextError::Empty);
    };
    let mut table = Vec::new();
    for row in rows.iter().skip(1) {
        if row.len() == 1 && row[0].is_empty() {
            continue;
        }
        let mut record = Map::new();
        for (i, h) in headers.iter().enumerate() {
            let name = if h.is_empty() {
                format!("column{}", i + 1)
            } else {
                h.clone()
            };
            record.insert(name, Value::String(row.get(i).cloned().unwrap_or_default()));
        }
        table.push(Value::Object(record));
    }
    if table.is_empty() {
        return Err(TextError::Msg("msg.noDataRows", String::new()));
    }
    Ok(Parsed {
        tree: Value::Array(table),
        delimiter: (delimiter != ',').then_some(delimiter),
    })
}

// ------------------------------------------------------------------ INI in

fn from_ini(text: &str) -> Value {
    let mut root = Map::new();
    let mut section: Option<String> = None;
    for raw in text.split('\n') {
        let line = raw.trim();
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_owned();
            if !matches!(root.get(&name), Some(Value::Object(_))) {
                root.insert(name.clone(), Value::Object(Map::new()));
            }
            section = Some(name);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let target = match &section {
            Some(s) => root
                .get_mut(s)
                .and_then(Value::as_object_mut)
                .expect("section exists"),
            None => &mut root,
        };
        target.insert(key.to_owned(), Value::String(value.trim().to_owned()));
    }
    Value::Object(root)
}

// ------------------------------------------------------------------ XML in

struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<(String, Value)>,
    text: String,
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    s.rsplit(':').next().unwrap_or("").to_owned()
}

fn open_node(e: &BytesStart<'_>) -> Result<Node, TextError> {
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|e| TextError::Json(e.to_string()))?;
        let value = a
            .unescape_value()
            .map_err(|e| TextError::Json(e.to_string()))?
            .into_owned();
        attrs.push((local(a.key.as_ref()), value));
    }
    Ok(Node {
        name: local(e.name().as_ref()),
        attrs,
        children: vec![],
        text: String::new(),
    })
}

fn close_node(n: Node) -> Value {
    if n.children.is_empty() && n.attrs.is_empty() {
        return Value::String(n.text);
    }
    let mut obj = Map::new();
    for (k, v) in n.attrs {
        obj.insert(format!("@{k}"), Value::String(v));
    }
    // children with the same name collapse into an array, in first-seen order
    let mut order: Vec<String> = Vec::new();
    for (name, _) in &n.children {
        if !order.contains(name) {
            order.push(name.clone());
        }
    }
    for name in order {
        let mut items: Vec<Value> = n
            .children
            .iter()
            .filter(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
            .collect();
        obj.insert(
            name,
            if items.len() == 1 {
                items.remove(0)
            } else {
                Value::Array(items)
            },
        );
    }
    let own = n.text.trim();
    if !own.is_empty() {
        obj.insert("#text".into(), Value::String(own.to_owned()));
    }
    Value::Object(obj)
}

/// DTDs and entity declarations are never expanded (no XXE / billion laughs):
/// an unknown entity reference is a parse error.
fn from_xml(text: &str) -> Result<Value, TextError> {
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<Node> = Vec::new();
    let mut result: Option<Value> = None;
    loop {
        match reader
            .read_event()
            .map_err(|e| TextError::Json(e.to_string()))?
        {
            Event::Start(e) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(TextError::Json("nesting too deep".into()));
                }
                stack.push(open_node(&e)?);
            }
            Event::Empty(e) => {
                let node = open_node(&e)?;
                let (name, value) = (node.name.clone(), close_node(node));
                match stack.last_mut() {
                    Some(parent) => parent.children.push((name, value)),
                    None => result = Some(value),
                }
            }
            Event::End(_) => {
                let node = stack
                    .pop()
                    .ok_or_else(|| TextError::Json("unbalanced tag".into()))?;
                let (name, value) = (node.name.clone(), close_node(node));
                match stack.last_mut() {
                    Some(parent) => parent.children.push((name, value)),
                    None => result = Some(value),
                }
            }
            Event::Text(t) => {
                if let Some(n) = stack.last_mut() {
                    n.text.push_str(
                        &t.xml_content()
                            .map_err(|e| TextError::Json(e.to_string()))?,
                    );
                }
            }
            Event::GeneralRef(r) => {
                let resolved = match r
                    .resolve_char_ref()
                    .map_err(|e| TextError::Json(e.to_string()))?
                {
                    Some(c) => c.to_string(),
                    None => match r
                        .decode()
                        .map_err(|e| TextError::Json(e.to_string()))?
                        .as_ref()
                    {
                        "amp" => "&".into(),
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "quot" => "\"".into(),
                        "apos" => "'".into(),
                        other => return Err(TextError::Json(format!("unknown entity &{other};"))),
                    },
                };
                if let Some(n) = stack.last_mut() {
                    n.text.push_str(&resolved);
                }
            }
            Event::CData(c) => {
                if let Some(n) = stack.last_mut() {
                    n.text.push_str(&String::from_utf8_lossy(&c));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    result.ok_or_else(|| TextError::Msg("msg.noRootElement", String::new()))
}

// ------------------------------------------------------------------ tables out

fn as_table(tree: &Value) -> Result<Vec<&Map<String, Value>>, TextError> {
    match tree {
        Value::Array(items) if items.iter().all(Value::is_object) => {
            Ok(items.iter().filter_map(Value::as_object).collect())
        }
        _ => Err(TextError::Msg("msg.notATable", String::new())),
    }
}

fn columns(rows: &[&Map<String, Value>]) -> Vec<String> {
    let mut cols: Vec<String> = Vec::new();
    for row in rows {
        for k in row.keys() {
            if !cols.contains(k) {
                cols.push(k.clone());
            }
        }
    }
    cols
}

fn cell(row: &Map<String, Value>, col: &str) -> String {
    match row.get(col) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

fn csv_quote(v: &str) -> String {
    if v.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_owned()
    }
}

fn to_csv(rows: &[&Map<String, Value>]) -> String {
    let cols = columns(rows);
    let mut out = cols
        .iter()
        .map(|c| csv_quote(c))
        .collect::<Vec<_>>()
        .join(",");
    out.push('\n');
    for row in rows {
        out.push_str(
            &cols
                .iter()
                .map(|c| csv_quote(&cell(row, c)))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

fn html_escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn to_html(rows: &[&Map<String, Value>]) -> String {
    let cols = columns(rows);
    let mut out = String::from("<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>Table</title></head><body>\n<table>\n");
    out.push_str(&format!(
        "  <thead><tr>{}</tr></thead>\n  <tbody>\n",
        cols.iter()
            .map(|c| format!("<th>{}</th>", html_escape(c)))
            .collect::<String>()
    ));
    for row in rows {
        out.push_str(&format!(
            "    <tr>{}</tr>\n",
            cols.iter()
                .map(|c| format!("<td>{}</td>", html_escape(&cell(row, c))))
                .collect::<String>()
        ));
    }
    out.push_str("  </tbody>\n</table>\n</body></html>\n");
    out
}

fn to_markdown(rows: &[&Map<String, Value>]) -> String {
    let esc = |v: &str| v.replace('|', "\\|").replace(['\r', '\n'], " ");
    let cols = columns(rows);
    let mut out = format!(
        "| {} |\n| {} |\n",
        cols.iter().map(|c| esc(c)).collect::<Vec<_>>().join(" | "),
        cols.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
    );
    for row in rows {
        out.push_str(&format!(
            "| {} |\n",
            cols.iter()
                .map(|c| esc(&cell(row, c)))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    out
}

// ------------------------------------------------------------------ XML out

fn safe_name(key: &str) -> String {
    let mut name: String = key
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
    {
        name.insert(0, '_');
    }
    name
}

fn xml_escape(v: &str) -> String {
    // Characters XML 1.0 cannot carry at all are dropped rather than producing an unreadable file.
    v.chars()
        .filter(|c| matches!(*c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}'))
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn write_element(out: &mut String, name: &str, node: &Value, depth: usize) {
    let pad = "  ".repeat(depth);
    let children: Vec<(String, &Value)> = match node {
        Value::Object(o) => o.iter().map(|(k, v)| (safe_name(k), v)).collect(),
        Value::Array(a) => a.iter().map(|v| ("item".to_owned(), v)).collect(),
        _ => vec![],
    };
    if matches!(node, Value::Object(_) | Value::Array(_)) {
        if children.is_empty() {
            out.push_str(&format!("{pad}<{name} />\n"));
            return;
        }
        out.push_str(&format!("{pad}<{name}>\n"));
        for (n, v) in children {
            write_element(out, &n, v, depth + 1);
        }
        out.push_str(&format!("{pad}</{name}>\n"));
    } else {
        let text = if node.is_null() {
            String::new()
        } else {
            xml_escape(&scalar(node))
        };
        if text.is_empty() {
            out.push_str(&format!("{pad}<{name} />\n"));
        } else {
            out.push_str(&format!("{pad}<{name}>{text}</{name}>\n"));
        }
    }
}

fn to_xml(tree: &Value) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    write_element(&mut out, "root", tree, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table(csv: &str) -> Value {
        parse("csv", csv).unwrap().tree
    }

    #[test]
    fn csv_quotes_newlines_and_headers() {
        let t = table("name,note,\n\"Smith, J\",\"line1\nline2 \"\"q\"\"\",x\n\nbob,,\n");
        assert_eq!(t[0]["name"], "Smith, J");
        assert_eq!(t[0]["note"], "line1\nline2 \"q\"");
        assert_eq!(t[0]["column3"], "x");
        assert_eq!(t.as_array().unwrap().len(), 2); // blank line skipped
        assert_eq!(t[1]["note"], "");
    }

    #[test]
    fn delimiter_detection() {
        assert_eq!(detect_delimiter("a;b;c\n1;2;3\n"), ';');
        assert_eq!(detect_delimiter("a\tb\n1\t2\n"), '\t');
        assert_eq!(detect_delimiter("a|b|c\n1|2|3\n"), '|');
        assert_eq!(detect_delimiter("just one column\nrow\n"), ',');
        assert_eq!(parse("csv", "a;b\n1;2\n").unwrap().delimiter, Some(';'));
        assert_eq!(parse("csv", "a,b\n1,2\n").unwrap().delimiter, None);
        // a comma inside quotes must not beat the real separator
        assert_eq!(detect_delimiter("\"a,b\";c\n\"1,2\";3\n"), ';');
    }

    #[test]
    fn csv_errors() {
        assert_eq!(parse("csv", "").err(), Some(TextError::Empty));
        assert_eq!(parse("csv", "a,b\n").err().unwrap().key(), "msg.noDataRows");
    }

    #[test]
    fn csv_to_every_table_format() {
        let t = table("name,qty\nA&B,\"1|2\"\nc,3\n");
        assert_eq!(render(&t, "CSV").unwrap(), "name,qty\nA&B,1|2\nc,3\n");
        assert_eq!(
            render(&t, "MD").unwrap(),
            "| name | qty |\n| --- | --- |\n| A&B | 1\\|2 |\n| c | 3 |\n"
        );
        let h = render(&t, "HTML").unwrap();
        assert!(h.contains("<td>A&amp;B</td>") && h.contains("<th>qty</th>"));
    }

    #[test]
    fn csv_output_quotes_when_needed() {
        let t = json!([{"a": "x,y", "b": "q\"r", "c": null, "d": 5}]);
        assert_eq!(
            render(&t, "CSV").unwrap(),
            "a,b,c,d\n\"x,y\",\"q\"\"r\",,5\n"
        );
    }

    #[test]
    fn non_table_json_cannot_become_csv() {
        assert_eq!(
            render(&json!({"a": 1}), "CSV").err().unwrap().key(),
            "msg.notATable"
        );
        assert_eq!(
            render(&json!([1, 2]), "MD").err().unwrap().key(),
            "msg.notATable"
        );
        assert_eq!(render(&json!([]), "CSV").unwrap(), "\n"); // empty table: header only
    }

    #[test]
    fn ini_sections_comments_and_duplicates() {
        let t = parse(
            "ini",
            "top=1\n; c\n[a]\nk = v \n[b]\n# x\nz=2\n[a]\nk2=3\nnoequals\n=bad\n",
        )
        .unwrap()
        .tree;
        assert_eq!(
            t,
            json!({"top": "1", "a": {"k": "v", "k2": "3"}, "b": {"z": "2"}})
        );
        assert_eq!(
            parse("CFG", "[s]\nx=1").unwrap().tree,
            json!({"s": {"x": "1"}})
        );
    }

    #[test]
    fn xml_to_json_shape() {
        let x = "<?xml version=\"1.0\"?><lib id=\"7\"><book>A</book><book>B</book><note>t</note><empty/><n a=\"1\">x</n></lib>";
        let t = parse("xml", x).unwrap().tree;
        assert_eq!(
            t,
            json!({"@id": "7", "book": ["A", "B"], "note": "t", "empty": "", "n": {"@a": "1", "#text": "x"}})
        );
        assert_eq!(
            parse("xml", "<a>x &amp; y &lt;</a>").unwrap().tree,
            json!("x & y <")
        );
        assert_eq!(
            parse("xml", "<ns:a xmlns:ns=\"u\"><ns:b>1</ns:b></ns:a>")
                .unwrap()
                .tree["b"],
            "1"
        );
        assert_eq!(parse("xml", "<a><![CDATA[1<2]]></a>").unwrap().tree, "1<2");
    }

    #[test]
    fn xml_hostile_input_is_rejected_not_expanded() {
        let xxe = "<?xml version=\"1.0\"?><!DOCTYPE r [<!ENTITY e SYSTEM \"file:///etc/passwd\">]><r>&e;</r>";
        assert!(parse("xml", xxe).is_err());
        let laughs = "<!DOCTYPE l [<!ENTITY a \"aaaa\"><!ENTITY b \"&a;&a;&a;&a;\">]><l>&b;</l>";
        assert!(parse("xml", laughs).is_err());
        let deep = format!("{}{}", "<a>".repeat(200), "</a>".repeat(200));
        assert!(parse("xml", &deep).is_err());
        assert!(parse("xml", "<a><b></a>").is_err());
        assert_eq!(parse("xml", "").err().unwrap().key(), "msg.noRootElement");
    }

    #[test]
    fn json_to_xml() {
        let t = json!({"a b": 1, "list": [1, "x<y"], "o": {}, "n": null, "1st": true});
        let x = render(&t, "XML").unwrap();
        assert!(
            x.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<root>\n  <a_b>1</a_b>\n")
        );
        assert!(
            x.contains("    <item>x&lt;y</item>")
                && x.contains("<o />")
                && x.contains("<n />")
                && x.contains("<_1st>true</_1st>")
        );
        // the result must itself be parseable
        assert!(parse("xml", &x).is_ok());
        assert!(!render(&json!({"a": "bad\u{1}char"}), "XML")
            .unwrap()
            .contains('\u{1}'));
    }

    #[test]
    fn unsupported_directions() {
        assert_eq!(
            parse("exe", "").err().unwrap().key(),
            "msg.cannotConvertFrom"
        );
        assert_eq!(
            render(&json!({}), "PDF").err().unwrap().key(),
            "msg.cannotConvertTo"
        );
    }
}
