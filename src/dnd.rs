//! What a file drop types into the terminal.

/// The `text/uri-list` of a drop as the text to paste: local files become
/// shell-quoted paths, anything else is kept as the URI. Entries are
/// space-separated, with a trailing space so the next word can follow.
#[must_use]
pub fn uri_list_to_paste(list: &str) -> String {
    let mut out = String::new();
    for line in list.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match file_path(line) {
            Some(path) => out.push_str(&shell_quote(&path)),
            None => out.push_str(&shell_quote(line)),
        }
        out.push(' ');
    }
    out
}

/// `file://[host]/a%20b` -> `/a b`.
fn file_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let mut bytes = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        let hex = (raw[i] == b'%')
            .then(|| raw.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(b) = hex {
            bytes.push(b);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn shell_quote(s: &str) -> String {
    let safe = |c: char| c.is_alphanumeric() || "_-./:@%+=,".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_path() {
        assert_eq!(
            uri_list_to_paste("file:///home/u/a.txt\r\n"),
            "/home/u/a.txt "
        );
    }

    #[test]
    fn decodes_and_quotes() {
        assert_eq!(
            uri_list_to_paste("file:///tmp/my%20file's.txt"),
            r"'/tmp/my file'\''s.txt' "
        );
    }

    #[test]
    fn several_with_comment_and_host() {
        assert_eq!(
            uri_list_to_paste("# note\r\nfile://host/a\r\nfile:///b%c3%a9\r\n"),
            "/a /bé "
        );
    }

    #[test]
    fn other_uris_are_kept_quoted() {
        assert_eq!(
            uri_list_to_paste("https://x.org/a?b=1"),
            "'https://x.org/a?b=1' "
        );
    }

    #[test]
    fn newline_in_name_cannot_escape_quotes() {
        assert_eq!(uri_list_to_paste("file:///a%0Ab"), "'/a\nb' ");
    }
}
