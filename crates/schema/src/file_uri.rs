//! File URI conversion shared by schema references and the editor's LSP client.

pub fn from_path(path: &str) -> String {
    let windows = path.as_bytes().get(1) == Some(&b':') || path.starts_with(r"\\");
    let normalized = windows.then(|| {
        if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
            format!("//{rest}").replace('\\', "/")
        } else {
            path.strip_prefix(r"\\?\").unwrap_or(path).replace('\\', "/")
        }
    });
    let path = normalized.as_deref().unwrap_or(path);
    let drive = path.as_bytes().get(1) == Some(&b':');
    let mut out = String::from(if path.starts_with("//") {
        "file:"
    } else if drive {
        "file:///"
    } else {
        "file://"
    });
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let rest = if rest.starts_with('/') || rest.as_bytes().get(1) == Some(&b':') {
        rest.to_string()
    } else {
        let (authority, path) = rest.split_once('/')?;
        if authority.eq_ignore_ascii_case("localhost") { format!("/{path}") } else { format!("//{authority}/{path}") }
    };
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let path = String::from_utf8(out).ok()?;
    if path.starts_with('/') && path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic) && path.as_bytes().get(2) == Some(&b':') {
        Some(path[1..].to_string())
    } else {
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_paths_round_trip_without_changing_their_authority() {
        for path in ["C:/şablon boşluk/a#b.json", "//server/share/a b.json", "/srv/şablon/a#b.json"] {
            assert_eq!(to_path(&from_path(path)).as_deref(), Some(path));
        }
        assert_eq!(from_path(r"\\?\C:\work\schema.json"), "file:///C:/work/schema.json");
        assert_eq!(from_path(r"\\?\UNC\server\share\a.json"), "file://server/share/a.json");
        assert_eq!(to_path("file://localhost/tmp/a%20b.json").as_deref(), Some("/tmp/a b.json"));
        assert!(to_path("file:///a%XX.json").is_none());
    }
}
