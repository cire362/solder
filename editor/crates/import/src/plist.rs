//! Property lists in XML, the format TextMate keeps its themes and
//! grammars in (`.tmTheme`, `.tmLanguage`), read into the same values a
//! JSON file gives.
//!
//! Only what those files use is read: dictionaries, arrays, strings,
//! numbers and the two truths. A date or a blob is kept as its text.

use serde_json::{Map, Value};

/// Reads a property list. `Err` says what is wrong with it.
pub fn parse(source: &str) -> Result<Value, String> {
    let mut reader = Reader { rest: source };
    // The list is the one thing inside `<plist>`; what comes before it is
    // the file's header.
    loop {
        match reader.tag()? {
            Tag::Open("plist") => break,
            Tag::Open(_) | Tag::Close(_) | Tag::Empty(_) => {
                return Err("not a property list".into());
            }
            Tag::Skipped => {}
        }
    }
    reader.value()
}

struct Reader<'a> {
    rest: &'a str,
}

enum Tag<'a> {
    Open(&'a str),
    Close(&'a str),
    /// `<true/>`, or a string with nothing in it.
    Empty(&'a str),
    /// A comment, a declaration or the document type.
    Skipped,
}

impl<'a> Reader<'a> {
    /// The text up to the next tag, with the names XML has for a few
    /// characters put back.
    fn text(&mut self) -> String {
        let end = self.rest.find('<').unwrap_or(self.rest.len());
        let raw = &self.rest[..end];
        self.rest = &self.rest[end..];
        unescape(raw)
    }

    fn tag(&mut self) -> Result<Tag<'a>, String> {
        let start = self.rest.find('<').ok_or("the list ends too soon")?;
        self.rest = &self.rest[start..];
        for (open, close) in [("<!--", "-->"), ("<?", "?>"), ("<!", ">")] {
            if self.rest.starts_with(open) {
                let end = self.rest.find(close).ok_or("a comment is not closed")?;
                self.rest = &self.rest[end + close.len()..];
                return Ok(Tag::Skipped);
            }
        }
        let end = self.rest.find('>').ok_or("a tag is not closed")?;
        let inner = &self.rest[1..end];
        self.rest = &self.rest[end + 1..];
        let name = |text: &'a str| text.split_whitespace().next().unwrap_or_default();
        Ok(if let Some(closed) = inner.strip_prefix('/') {
            Tag::Close(name(closed))
        } else if let Some(empty) = inner.strip_suffix('/') {
            Tag::Empty(name(empty))
        } else {
            Tag::Open(name(inner))
        })
    }

    /// The next tag that is not a comment.
    fn next(&mut self) -> Result<Tag<'a>, String> {
        loop {
            match self.tag()? {
                Tag::Skipped => {}
                tag => return Ok(tag),
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        let tag = self.next()?;
        self.value_of(tag)
    }

    fn value_of(&mut self, tag: Tag<'a>) -> Result<Value, String> {
        match tag {
            Tag::Empty("true") => Ok(Value::Bool(true)),
            Tag::Empty("false") => Ok(Value::Bool(false)),
            Tag::Empty("dict") => Ok(Value::Object(Map::new())),
            Tag::Empty("array") => Ok(Value::Array(Vec::new())),
            Tag::Empty(_) => Ok(Value::String(String::new())),
            Tag::Open("dict") => {
                let mut dict = Map::new();
                loop {
                    match self.next()? {
                        Tag::Close("dict") => return Ok(Value::Object(dict)),
                        Tag::Open("key") => {
                            let key = self.leaf("key")?;
                            dict.insert(key, self.value()?);
                        }
                        Tag::Empty("key") => {
                            dict.insert(String::new(), self.value()?);
                        }
                        _ => return Err("a dictionary holds something that is no key".into()),
                    }
                }
            }
            Tag::Open("array") => {
                let mut array = Vec::new();
                loop {
                    match self.next()? {
                        Tag::Close("array") => return Ok(Value::Array(array)),
                        tag => array.push(self.value_of(tag)?),
                    }
                }
            }
            Tag::Open(name @ ("integer" | "real")) => {
                let text = self.leaf(name)?;
                let number = text.trim();
                Ok(number
                    .parse::<i64>()
                    .map(Value::from)
                    .or_else(|_| number.parse::<f64>().map(Value::from))
                    .unwrap_or(Value::Null))
            }
            Tag::Open(name) => Ok(Value::String(self.leaf(name)?)),
            Tag::Close(name) => Err(format!("</{name}> closes nothing")),
            Tag::Skipped => self.value(),
        }
    }

    /// The text of a tag that holds only text, up to where it closes.
    fn leaf(&mut self, name: &str) -> Result<String, String> {
        let mut text = String::new();
        loop {
            text.push_str(&self.text());
            // Text given as it is, with nothing in it to put back.
            if let Some(raw) = self.rest.strip_prefix("<![CDATA[") {
                let end = raw.find("]]>").ok_or("a CDATA is not closed")?;
                text.push_str(&raw[..end]);
                self.rest = &raw[end + 3..];
                continue;
            }
            return match self.tag()? {
                Tag::Close(closed) if closed == name => Ok(text),
                Tag::Skipped => continue,
                _ => Err(format!("<{name}> holds more than text")),
            };
        }
    }
}

fn unescape(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let named = rest.find(';').filter(|end| *end <= 10).map(|end| {
            let name = &rest[1..end];
            let character = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => name
                    .strip_prefix("#x")
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|n| n.parse().ok()))
                    .and_then(char::from_u32),
            };
            (character, end)
        });
        match named {
            Some((Some(character), end)) => {
                out.push(character);
                rest = &rest[end + 1..];
            }
            // An `&` that names nothing is itself.
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_property_list_reads_as_the_values_it_holds() {
        let list = parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<!-- a theme -->
<dict>
    <key>name</key> <string>Tom &amp; Jerry &#39;s &lt;best&gt;</string>
    <key>empty</key> <string></string>
    <key>none</key> <string/>
    <key>count</key> <integer> 3 </integer>
    <key>ratio</key> <real>0.5</real>
    <key>on</key> <true/>
    <key>off</key> <false/>
    <key>raw</key> <string><![CDATA[a < b && c]]></string>
    <key>settings</key>
    <array>
        <dict>
            <key>scope</key> <string>comment, string</string>
            <key>settings</key> <dict><key>foreground</key><string>#75715E</string></dict>
        </dict>
        <string>last</string>
        <array/>
    </array>
</dict>
</plist>"#,
        )
        .unwrap();
        assert_eq!(
            list,
            json!({
                "name": "Tom & Jerry 's <best>",
                "empty": "", "none": "", "count": 3, "ratio": 0.5, "on": true, "off": false,
                "raw": "a < b && c",
                "settings": [
                    { "scope": "comment, string", "settings": { "foreground": "#75715E" } },
                    "last",
                    [],
                ],
            })
        );
        // What is no property list, or one that breaks off, says so.
        assert!(parse("{ \"a\": 1 }").is_err());
        assert!(parse("<html><body/></html>").is_err());
        assert!(parse("<plist><dict><key>a</key><string>b</string>").is_err());
        assert!(parse("<plist><dict><string>no key</string></dict></plist>").is_err());
    }
}
