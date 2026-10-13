//! A small, dependency-free JSON reader: just enough to turn the output of
//! `ubus call` into something `render.rs` can lay out as tables.
//!
//! Input is untrusted (it comes from other programs), so nesting depth is
//! limited and nothing here can panic on malformed text.

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    /// Key order is preserved.
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// A non-negative whole number.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Num(n) if *n >= 0.0 && n.fract() == 0.0 && *n < 1.8e19 => Some(*n as u64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(v) => Some(v),
            _ => None,
        }
    }
}

/// Parses a complete JSON document. Trailing non-whitespace is an error.
pub fn parse(text: &str) -> Option<Value> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i == p.s.len() {
        Some(v)
    } else {
        None
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        if self.peek() == Some(b) {
            self.i += 1;
            Some(())
        } else {
            None
        }
    }

    fn lit(&mut self, word: &str, v: Value) -> Option<Value> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Some(v)
        } else {
            None
        }
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Value::Str),
            b't' => self.lit("true", Value::Bool(true)),
            b'f' => self.lit("false", Value::Bool(false)),
            b'n' => self.lit("null", Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self, depth: usize) -> Option<Value> {
        self.eat(b'{')?;
        let mut items = Vec::new();
        self.ws();
        if self.eat(b'}').is_some() {
            return Some(Value::Obj(items));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let v = self.value(depth + 1)?;
            items.push((key, v));
            self.ws();
            if self.eat(b',').is_some() {
                continue;
            }
            self.eat(b'}')?;
            return Some(Value::Obj(items));
        }
    }

    fn array(&mut self, depth: usize) -> Option<Value> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.eat(b']').is_some() {
            return Some(Value::Arr(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            if self.eat(b',').is_some() {
                continue;
            }
            self.eat(b']')?;
            return Some(Value::Arr(items));
        }
    }

    fn number(&mut self) -> Option<Value> {
        let start = self.i;
        self.eat(b'-');
        while matches!(
            self.peek(),
            Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
        ) {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        text.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(Value::Num)
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = self.s.get(self.i..self.i + 4)?;
        let h = std::str::from_utf8(h).ok()?;
        let n = u32::from_str_radix(h, 16).ok()?;
        self.i += 4;
        Some(n)
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let b = self.peek()?;
            self.i += 1;
            match b {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = self.peek()?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) {
                                // surrogate pair
                                self.eat(b'\\')?;
                                self.eat(b'u')?;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return None;
                                }
                                char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))?
                            } else {
                                char::from_u32(hi)?
                            }
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                // raw control characters are not allowed inside strings
                0..=0x1f => return None,
                other => out.push(other),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars() {
        assert_eq!(parse("null"), Some(Value::Null));
        assert_eq!(parse(" true "), Some(Value::Bool(true)));
        assert_eq!(parse("false"), Some(Value::Bool(false)));
        assert_eq!(parse("42"), Some(Value::Num(42.0)));
        assert_eq!(parse("-1.5e2"), Some(Value::Num(-150.0)));
        assert_eq!(parse("\"hi\""), Some(Value::Str("hi".into())));
    }

    #[test]
    fn containers_keep_order() {
        let v = parse(r#"{"b": [1, 2, {"c": null}], "a": {}, "z": []}"#).unwrap();
        match &v {
            Value::Obj(items) => {
                let keys: Vec<&str> = items.iter().map(|(k, _)| k.as_str()).collect();
                assert_eq!(keys, ["b", "a", "z"]);
            }
            _ => panic!("not an object"),
        }
        assert_eq!(v.get("b").unwrap().as_arr().unwrap().len(), 3);
        assert_eq!(v.get("z").unwrap().as_arr().unwrap().len(), 0);
        assert!(v.get("missing").is_none());
    }

    #[test]
    fn ubus_style_pretty_printed_output_with_tabs() {
        let text = "{\n\t\"interface\": [\n\t\t{\n\t\t\t\"up\": true,\n\t\t\t\"uptime\": 123,\n\t\t\t\"route\": [\n\n\t\t\t]\n\t\t}\n\t]\n}\n";
        let v = parse(text).unwrap();
        let first = &v.get("interface").unwrap().as_arr().unwrap()[0];
        assert_eq!(first.get("up").unwrap().as_bool(), Some(true));
        assert_eq!(first.get("uptime").unwrap().as_u64(), Some(123));
    }

    #[test]
    fn string_escapes() {
        assert_eq!(
            parse(r#""a\"b\\c\/d\n\t\u0041\u00e9""#),
            Some(Value::Str("a\"b\\c/d\n\tAé".into()))
        );
        // surrogate pair: U+1F980
        assert_eq!(
            parse(r#""\ud83e\udd80""#),
            Some(Value::Str("\u{1F980}".into()))
        );
        // raw UTF-8 passes through
        assert_eq!(parse("\"Ü€\""), Some(Value::Str("Ü€".into())));
    }

    #[test]
    fn rejects_malformed_input_without_panicking() {
        for bad in [
            "",
            "{",
            "}",
            "[1,]",
            "{\"a\"}",
            "{\"a\":}",
            "[1 2]",
            "tru",
            "nul",
            "\"abc",
            "\"\\x\"",
            "\"\\u12\"",
            "\"\\ud83e\"",
            "\"\\ud83e\\u0041\"",
            "01x",
            "-",
            "1e",
            "{\"a\":1,}",
            "[1] x",
            "\"a\nb\"",
            "\u{0}",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn nesting_depth_is_limited() {
        let ok = format!("{}1{}", "[".repeat(60), "]".repeat(60));
        assert!(parse(&ok).is_some());
        let deep = format!("{}1{}", "[".repeat(5000), "]".repeat(5000));
        assert_eq!(parse(&deep), None);
    }

    #[test]
    fn accessors_are_strict() {
        let v = parse(r#"{"n": 3, "f": 3.5, "neg": -1, "s": "x", "b": true}"#).unwrap();
        assert_eq!(v.get("n").unwrap().as_u64(), Some(3));
        assert_eq!(v.get("f").unwrap().as_u64(), None);
        assert_eq!(v.get("neg").unwrap().as_u64(), None);
        assert_eq!(v.get("f").unwrap().as_f64(), Some(3.5));
        assert_eq!(v.get("s").unwrap().as_str(), Some("x"));
        assert_eq!(v.get("s").unwrap().as_u64(), None);
        assert_eq!(v.get("b").unwrap().as_bool(), Some(true));
        assert_eq!(Value::Null.get("x"), None);
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // dependency-free fuzz: random strings over JSON's own alphabet
        let alphabet: Vec<&str> = vec![
            "{",
            "}",
            "[",
            "]",
            ",",
            ":",
            "\"",
            "\\",
            "u",
            "0",
            "9",
            "-",
            "e",
            ".",
            "t",
            "n",
            " ",
            "\u{1F980}",
            "\\u",
            "d83e",
            "\n",
        ];
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20000 {
            let n = (next() % 16) as usize;
            let s: String = (0..n)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect();
            let _ = parse(&s);
        }
    }
}
