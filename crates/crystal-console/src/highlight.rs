//! Colors for a log line's parts, as the `tracing` fmt layer writes them:
//! `span{field=value}: target: message key=value key="text"`. The connection's span is dimmed,
//! the target muted, field names quiet, and the values that matter stand out: who (characters,
//! accounts), GM levels and commands, actor ids and opcodes, chat text.

use ratatui::style::{Color, Modifier, Style};

const SPAN: Color = Color::Rgb(72, 98, 140);
const TARGET: Color = Color::Rgb(86, 128, 190);
const KEY: Color = Color::Rgb(110, 118, 129);
const VALUE: Color = Color::Rgb(201, 209, 217);
const NUMBER: Color = Color::Rgb(121, 192, 255);
const WHO: Color = Color::Rgb(255, 184, 108);
const GM: Color = Color::Rgb(255, 215, 64);
const COMMAND: Color = Color::Rgb(255, 123, 196);
const ID: Color = Color::Rgb(188, 140, 255);
const CHAT: Color = Color::Rgb(126, 231, 135);
const PLACE: Color = Color::Rgb(57, 197, 207);

/// How a field's value is shown, by its name.
fn value_style(key: &str, value: &str) -> Style {
    let bold = |c: Color| Style::new().fg(c).add_modifier(Modifier::BOLD);
    match key {
        "character" | "name" | "account" | "username" => bold(WHO),
        "gm_level" | "level" | "on" => bold(GM),
        "command" | "args" => bold(COMMAND),
        "actor_id" | "actor" | "content_id" | "opcode" | "session" => Style::new().fg(ID),
        "text" => Style::new().fg(CHAT),
        "peer" | "addr" | "zone" | "territory" | "service" | "connection" | "from" | "to" => Style::new().fg(PLACE),
        _ if looks_numeric(value) => Style::new().fg(NUMBER),
        _ => Style::new().fg(VALUE),
    }
}

fn looks_numeric(v: &str) -> bool {
    let v = v.trim_matches('"');
    !v.is_empty() && (v.starts_with("0x") || v.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-')))
}

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// `text` as styled pieces; `message` is the style of the free text (the level's color).
pub fn segments(text: &str, message: Style) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = Vec::new();
    let mut rest = text;

    // span context: `conn{service="zone" peer=...}: ` (possibly several, joined by ':')
    let span_name = rest.split_once('{').map(|(name, _)| name);
    if span_name.is_some_and(|n| !n.is_empty() && n.chars().all(|c| is_key_char(c) || c == ':'))
        && let Some(end) = rest.find("}: ")
    {
        out.push((rest[..end + 3].to_owned(), Style::new().fg(SPAN)));
        rest = &rest[end + 3..];
    }
    // the target: `crystal_zone: ` or `crystal_world::zone: `
    if let Some(end) = rest.find(": ")
        && !rest[..end].is_empty()
        && rest[..end].chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
    {
        out.push((rest[..end + 2].to_owned(), Style::new().fg(TARGET)));
        rest = &rest[end + 2..];
    }

    // the message, then `key=value` fields
    let b: Vec<char> = rest.chars().collect();
    let mut i = 0;
    let mut plain = String::new();
    while i < b.len() {
        // a field starts at a word boundary with `key=`
        let at_word = i == 0 || b[i - 1] == ' ';
        if at_word && is_key_char(b[i]) {
            let mut j = i;
            while j < b.len() && is_key_char(b[j]) {
                j += 1;
            }
            if j < b.len() && b[j] == '=' && j > i {
                let key: String = b[i..j].iter().collect();
                let mut k = j + 1;
                if k < b.len() && b[k] == '"' {
                    k += 1;
                    while k < b.len() && b[k] != '"' {
                        if b[k] == '\\' {
                            k += 1;
                        }
                        k += 1;
                    }
                    k = (k + 1).min(b.len());
                } else {
                    while k < b.len() && b[k] != ' ' {
                        k += 1;
                    }
                }
                if !plain.is_empty() {
                    out.push((std::mem::take(&mut plain), message));
                }
                let value: String = b[j + 1..k].iter().collect();
                out.push((format!("{key}="), Style::new().fg(KEY)));
                let style = value_style(&key, &value);
                out.push((value, style));
                i = k;
                continue;
            }
        }
        plain.push(b[i]);
        i += 1;
    }
    if !plain.is_empty() {
        out.push((plain, message));
    }
    out
}

/// Splits styled pieces into rows of at most `width` characters.
pub fn wrap(pieces: Vec<(String, Style)>, width: usize) -> Vec<Vec<(String, Style)>> {
    let width = width.max(1);
    let mut rows = vec![Vec::new()];
    let mut used = 0;
    for (text, style) in pieces {
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            if used == width {
                rows.push(Vec::new());
                used = 0;
            }
            let take = (width - used).min(chars.len());
            let part: String = chars.drain(..take).collect();
            used += take;
            rows.last_mut().unwrap().push((part, style));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &[(String, Style)]) -> Vec<&str> {
        s.iter().map(|p| p.0.as_str()).collect()
    }

    #[test]
    fn splits_a_zone_line() {
        let line =
            r#"conn{service="zone" peer=127.0.0.1:52876}: crystal_zone:   command command="gm" args="" gm_level=3"#;
        let s = segments(line, Style::new());
        assert_eq!(
            texts(&s),
            [
                r#"conn{service="zone" peer=127.0.0.1:52876}: "#,
                "crystal_zone: ",
                "  command ",
                "command=",
                r#""gm""#,
                " ",
                "args=",
                r#""""#,
                " ",
                "gm_level=",
                "3"
            ]
        );
        assert_eq!(s[4].1.fg, Some(COMMAND));
        assert_eq!(s[10].1.fg, Some(GM));
    }

    #[test]
    fn quoted_values_keep_their_spaces() {
        let s = segments(r#"crystal_zone: session init character="Null Crystal" gm_level=3"#, Style::new());
        assert!(texts(&s).contains(&r#""Null Crystal""#));
        let who = s.iter().find(|p| p.0 == r#""Null Crystal""#).unwrap();
        assert_eq!(who.1.fg, Some(WHO));
    }

    #[test]
    fn plain_text_stays_whole() {
        let s = segments("Console ready. a=b is not after a space?", Style::new());
        assert_eq!(texts(&s)[0], "Console ready. ");
    }

    #[test]
    fn wraps_across_pieces() {
        let rows = wrap(vec![("abcd".into(), Style::new()), ("efg".into(), Style::new())], 3);
        let flat: Vec<String> = rows.iter().map(|r| r.iter().map(|p| p.0.as_str()).collect()).collect();
        assert_eq!(flat, ["abc", "def", "g"]);
    }
}
