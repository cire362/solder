//! The conditions VS Code extensions put on what they contribute: a menu
//! entry `when` the file is of a language, a key `when` the editor has the
//! keyboard. They are small expressions over named facts about the editor:
//! `editorLangId == rust && !editorReadonly`.
//!
//! A fact the editor does not have is false, and so is a way of comparing
//! it does not know: what is shown or bound by mistake is worse than what
//! is left out.

use serde_json::Value;

/// Whether `clause` holds, with `fact` giving what the editor knows by
/// name. An empty clause holds.
pub fn holds(clause: &str, fact: &dyn Fn(&str) -> Option<Value>) -> bool {
    let tokens = tokens(clause);
    if tokens.is_empty() {
        return true;
    }
    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
        fact,
    };
    let held = parser.or();
    // Something left over is something that was not understood.
    held && parser.at == tokens.len()
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    /// A name, a number or a bare word.
    Word(String),
    /// Text in quotes.
    Text(String),
    Op(&'static str),
}

fn tokens(clause: &str) -> Vec<Token> {
    let chars: Vec<char> = clause.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let rest: String = chars[i..].iter().take(3).collect();
        let op = [
            "===", "!==", "==", "!=", "&&", "||", "=~", "<=", ">=", "!", "(", ")", "<", ">",
        ]
        .into_iter()
        .find(|op| rest.starts_with(op));
        if let Some(op) = op {
            tokens.push(Token::Op(match op {
                "===" => "==",
                "!==" => "!=",
                op => op,
            }));
            i += op.len();
            continue;
        }
        if c == '\'' || c == '"' {
            let end = chars[i + 1..].iter().position(|x| *x == c);
            let end = end.map_or(chars.len(), |end| i + 1 + end);
            tokens.push(Token::Text(chars[i + 1..end].iter().collect()));
            i = end + 1;
            continue;
        }
        // A pattern between slashes is one word, whatever is in it.
        if c == '/' {
            let end = chars[i + 1..].iter().position(|x| *x == '/');
            let mut end = end.map_or(chars.len(), |end| i + 2 + end);
            while end < chars.len() && chars[end].is_alphabetic() {
                end += 1;
            }
            tokens.push(Token::Word(chars[i..end].iter().collect()));
            i = end;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && !"()!=&|<>'\"".contains(chars[i]) {
            i += 1;
        }
        if i == start {
            // A character that is none of the above: not understood.
            tokens.push(Token::Op("?"));
            i += 1;
            continue;
        }
        tokens.push(Token::Word(chars[start..i].iter().collect()));
    }
    tokens
}

struct Parser<'a> {
    tokens: &'a [Token],
    at: usize,
    fact: &'a dyn Fn(&str) -> Option<Value>,
}

/// What a fact is when it is compared with a word: its text.
fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(on) => *on,
        Value::String(text) => !text.is_empty(),
        Value::Number(number) => number.as_f64() != Some(0.),
        _ => true,
    }
}

impl Parser<'_> {
    fn next_is(&mut self, op: &'static str) -> bool {
        let is = self.tokens.get(self.at) == Some(&Token::Op(op));
        if is {
            self.at += 1;
        }
        is
    }

    fn or(&mut self) -> bool {
        let mut held = self.and();
        while self.next_is("||") {
            // Both sides are read, whichever decides.
            let right = self.and();
            held = held || right;
        }
        held
    }

    fn and(&mut self) -> bool {
        let mut held = self.not();
        while self.next_is("&&") {
            let right = self.not();
            held = held && right;
        }
        held
    }

    fn not(&mut self) -> bool {
        if self.next_is("!") {
            return !self.not();
        }
        if self.next_is("(") {
            let held = self.or();
            return self.next_is(")") && held;
        }
        self.compare()
    }

    /// A fact on its own, or compared with a word.
    fn compare(&mut self) -> bool {
        let Some(Token::Word(name)) = self.tokens.get(self.at).cloned() else {
            // Nothing where a name belongs: the rest is not read.
            self.at = usize::MAX;
            return false;
        };
        self.at += 1;
        let known = match name.as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            name => (self.fact)(name),
        };
        let op = match self.tokens.get(self.at) {
            Some(Token::Op(op)) if ["==", "!=", "=~", "<", ">", "<=", ">="].contains(op) => *op,
            // `key in list` and `key not in list` ask about a fact that is
            // a list, which the editor has none of.
            Some(Token::Word(word)) if word == "in" || word == "not" => {
                self.at = usize::MAX;
                return false;
            }
            _ => return known.as_ref().is_some_and(truthy),
        };
        self.at += 1;
        let other = match self.tokens.get(self.at) {
            Some(Token::Word(word)) | Some(Token::Text(word)) => word.clone(),
            _ => {
                self.at = usize::MAX;
                return false;
            }
        };
        self.at += 1;
        // A fact that is not there equals nothing, and differs from all.
        let is = known.as_ref().map(text);
        match op {
            "==" => is.as_deref() == Some(other.as_str()),
            "!=" => is.as_deref() != Some(other.as_str()),
            "<" | ">" | "<=" | ">=" => {
                let (Some(a), Ok(b)) = (
                    is.and_then(|is| is.parse::<f64>().ok()),
                    other.parse::<f64>(),
                ) else {
                    return false;
                };
                match op {
                    "<" => a < b,
                    ">" => a > b,
                    "<=" => a <= b,
                    _ => a >= b,
                }
            }
            // A pattern: not read here.
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_condition_is_read_against_what_the_editor_knows() {
        let fact = |name: &str| match name {
            "editorLangId" => Some(json!("rust")),
            "editorTextFocus" => Some(json!(true)),
            "editorReadonly" => Some(json!(false)),
            "resourceExtname" => Some(json!(".rs")),
            "demo.count" => Some(json!(3)),
            "empty" => Some(json!("")),
            _ => None,
        };
        let holds = |clause: &str| holds(clause, &fact);
        assert!(holds(""));
        assert!(holds("editorTextFocus") && !holds("editorReadonly"));
        assert!(holds("!editorReadonly") && !holds("!editorTextFocus"));
        assert!(holds("editorLangId == rust") && holds("editorLangId == 'rust'"));
        assert!(!holds("editorLangId == go") && holds("editorLangId != go"));
        assert!(holds("editorLangId === rust") && holds("resourceExtname == .rs"));
        assert!(holds(
            "editorTextFocus && editorLangId == rust && !editorReadonly"
        ));
        assert!(!holds("editorTextFocus && editorLangId == go"));
        assert!(holds("editorLangId == go || editorLangId == rust"));
        // `&&` binds closer than `||`, and brackets say otherwise.
        assert!(holds("editorTextFocus || nothing && nothing"));
        assert!(!holds("(editorTextFocus || nothing) && nothing"));
        assert!(holds("!(editorReadonly || nothing)"));
        assert!(holds("true") && !holds("false"));
        assert!(holds("demo.count > 2") && !holds("demo.count >= 4") && holds("demo.count == 3"));
        // What the editor does not know is false, differs from every
        // word, and so is nothing an entry can be shown for.
        assert!(!holds("config.demo.enabled") && holds("!config.demo.enabled"));
        assert!(!holds("unknown == x") && holds("unknown != x"));
        assert!(!holds("empty"));
        // What is not understood does not hold: a pattern, a list, and
        // words that are no condition.
        assert!(!holds("editorLangId =~ /^ru/"));
        assert!(!holds("editorLangId in demo.languages"));
        assert!(!holds("editorTextFocus editorTextFocus"));
        assert!(!holds("editorTextFocus &&") && !holds("== rust") && !holds("(editorTextFocus"));
    }
}
