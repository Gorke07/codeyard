//! VS Code `.code-workspace` files. Codeyard opens one root folder per window
//! (git, search, language servers and session saving assume it), so a multi-folder
//! workspace opens as one window per folder. Plain text only.

use serde_json::Value;

/// `folders[].path` or `folders[].uri` (file://) → absolute, normalized folder paths;
/// relative paths are resolved against the file's folder, duplicates dropped, order kept.
/// Folders that don't exist are filtered out with `exists` (`Path::is_dir` on the local disk).
pub fn folders(text: &str, base_dir: &str, exists: impl Fn(&str) -> bool) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(&theme::themes::strip_jsonc(text)) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for f in v["folders"].as_array().into_iter().flatten() {
        let raw = match (f["path"].as_str(), f["uri"].as_str()) {
            (Some(p), _) => p.to_string(),
            (None, Some(u)) => match u.strip_prefix("file://") {
                Some(p) => percent_decode(p),
                None => continue, // like vscode-remote://: unsupported
            },
            _ => continue,
        };
        let abs = normalize(&if raw.starts_with('/') { raw } else { format!("{}/{raw}", base_dir.trim_end_matches('/')) });
        if exists(&abs) && !out.contains(&abs) {
            out.push(abs);
        }
    }
    out
}

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    format!("/{}", parts.join("/"))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn is_workspace_file(path: &str) -> bool {
    path.ends_with(".code-workspace")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_absolute_and_uri_folders() {
        let text = r#"{
            // yorum
            "folders": [
                { "path": "api" },
                { "path": "../web", "name": "Web" },
                { "path": "/opt/tools" },
                { "uri": "file:///srv/my%20app" },
                { "uri": "vscode-remote://ssh-remote+box/home/x" },
                { "path": "./api/" },
                { "name": "no path" },
                { "path": "gone" }
            ],
            "settings": { "editor.tabSize": 2 }
        }"#;
        let all = |p: &str| !p.ends_with("/gone");
        assert_eq!(folders(text, "/work/mono", all), ["/work/mono/api", "/work/web", "/opt/tools", "/srv/my app"]);
        assert!(folders("{ not json", "/w", all).is_empty());
        assert!(folders("{}", "/w", all).is_empty());
    }

    #[test]
    fn recognises_workspace_files() {
        assert!(is_workspace_file("/a/b.code-workspace"));
        assert!(!is_workspace_file("/a/b.json"));
    }
}
