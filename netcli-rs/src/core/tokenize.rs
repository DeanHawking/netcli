//! Splitting an input line into words. No dependencies, so it is unit-testable
//! without a terminal.

/// Splits `input` into words, honouring single and double quotes.
///
/// * Quotes group characters (including whitespace) into one word and are
///   removed from it.
/// * An empty quoted string (`""`) yields an empty word, so `set a.b.c ""`
///   can set an empty value.
/// * An unterminated quote is tolerated: the rest of the line becomes part of
///   the current word (useful while the user is still typing).
pub fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes: Option<char> = None;
    // True while a word is in progress, even if it is still empty (it had quotes).
    let mut started = false;

    for c in input.chars() {
        match in_quotes {
            Some(q) => {
                if c == q {
                    in_quotes = None;
                } else {
                    current.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    in_quotes = Some(c);
                    started = true;
                } else if c.is_whitespace() {
                    if started {
                        tokens.push(std::mem::take(&mut current));
                        started = false;
                    }
                } else {
                    current.push(c);
                    started = true;
                }
            }
        }
    }
    if started {
        tokens.push(current);
    }
    tokens
}

/// If `line` asks for help -- a `?` that is a word of its own at the end of
/// the line -- returns the text before it.
///
/// A `?` glued to a word is ordinary text: `set wireless.x.key pass?` sets a
/// password ending in `?`. To set a value that is just `?`, quote it (`"?"`).
pub fn help_request(line: &str) -> Option<&str> {
    let line = line.trim_end();
    let before = line.strip_suffix('?')?;
    if before.is_empty() || before.ends_with(char::is_whitespace) {
        Some(before)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(tokenize("show  interfaces\t"), vec!["show", "interfaces"]);
    }

    #[test]
    fn empty_and_blank_lines_have_no_words() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   \t ").is_empty());
    }

    #[test]
    fn quotes_group_words_and_are_removed() {
        assert_eq!(
            tokenize(r#"set wireless.r.ssid "My  Net""#),
            vec!["set", "wireless.r.ssid", "My  Net"]
        );
        assert_eq!(tokenize("set a.b 'x y'"), vec!["set", "a.b", "x y"]);
    }

    #[test]
    fn the_other_quote_kind_is_literal_inside_quotes() {
        assert_eq!(tokenize(r#"set a.b "it's""#), vec!["set", "a.b", "it's"]);
    }

    #[test]
    fn empty_quoted_string_is_an_empty_word() {
        assert_eq!(tokenize(r#"set a.b.c "" "#), vec!["set", "a.b.c", ""]);
        assert_eq!(tokenize("set a.b.c ''"), vec!["set", "a.b.c", ""]);
    }

    #[test]
    fn quotes_may_be_glued_to_text() {
        assert_eq!(tokenize(r#"a"b c"d"#), vec!["ab cd"]);
    }

    #[test]
    fn unterminated_quote_keeps_the_rest_of_the_line() {
        assert_eq!(
            tokenize(r#"set a.b "half typed"#),
            vec!["set", "a.b", "half typed"]
        );
        assert_eq!(tokenize(r#"set a.b ""#), vec!["set", "a.b", ""]);
    }

    #[test]
    fn question_mark_alone_is_help() {
        assert_eq!(help_request("?"), Some(""));
        assert_eq!(help_request("show ?"), Some("show "));
        assert_eq!(help_request("show ?  "), Some("show "));
    }

    #[test]
    fn question_mark_glued_to_a_word_is_not_help() {
        assert_eq!(help_request("set wireless.x.key pass?"), None);
        assert_eq!(help_request("sh?"), None);
    }

    #[test]
    fn no_question_mark_no_help() {
        assert_eq!(help_request("show interfaces"), None);
        assert_eq!(help_request(""), None);
    }

    #[test]
    fn a_quoted_question_mark_is_a_value() {
        assert_eq!(help_request(r#"set a.b "?""#), None);
    }
}
