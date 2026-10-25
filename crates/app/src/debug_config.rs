//! Debug configurations: `.vscode/launch.json` / `.codeyard/launch.jsonc` and
//! the ones auto-detected from the project (Cargo, Go, open Python file). Pure text handling.
//!
//! `type` -> adapter: `lldb`, `codelldb`, `cppdbg`, `lldb-dap` -> `lldb-dap` (C, C++, Rust, Swift);
//! `go` -> `dlv dap`; `python`, `debugpy` -> `python3 -m debugpy.adapter`. Other types (node ...)
//! get an explicit "not supported" message; the user can also give their own adapter via the `debugAdapter`
//! field: `{ "command": "...", "args": [] }`.

use serde_json::{Value, json};

use crate::workbench::tasks::{Task, expand_variables};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// DAP `adapterID`.
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    /// No process spawn: connect directly over TCP to an already running DAP server (`debugpy --listen`, `dlv --headless`...).
    pub connect: Option<(String, u16)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchConfig {
    pub name: String,
    pub adapter: Result<Adapter, String>,
    /// `launch` or `attach`.
    pub request: String,
    /// Arguments sent to the adapter (`program`, `args`, `cwd`... variables expanded).
    pub args: Value,
    /// Shell command to run before launch (`preLaunchTask` or automatic build).
    pub pre_launch: Option<String>,
}

/// If `lldb-dap` isn't on PATH, on macOS finds it via Xcode (`xcrun`): a shell, the adapter inherits stdin/stdout.
const LLDB_DAP: &str = "if command -v lldb-dap >/dev/null 2>&1; then exec lldb-dap; elif command -v lldb-vscode >/dev/null 2>&1; then exec lldb-vscode; else exec xcrun lldb-dap; fi";

/// Where js-debug may be installed (`src/dapDebugServer.js`): `CODEYARD_JS_DEBUG`, the folder we
/// download to, VS Code (user extension and the one inside the app), Mason (neovim).
pub fn find_js_debug() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("CODEYARD_JS_DEBUG").map(std::path::PathBuf::from) {
        candidates.push(if p.extension().is_some() { p } else { p.join("js-debug/src/dapDebugServer.js") });
    }
    candidates.push(adapters_dir().join("js-debug/src/dapDebugServer.js"));
    if let Some(h) = &home {
        for dir in [h.join(".vscode/extensions"), h.join(".vscode-insiders/extensions")] {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if e.file_name().to_string_lossy().starts_with("ms-vscode.js-debug-") {
                    candidates.push(e.path().join("src/dapDebugServer.js"));
                }
            }
        }
        candidates.push(h.join(".local/share/nvim/mason/packages/js-debug-adapter/js-debug/src/dapDebugServer.js"));
    }
    candidates.push("/Applications/Visual Studio Code.app/Contents/Resources/app/extensions/ms-vscode.js-debug/src/dapDebugServer.js".into());
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from) {
        candidates.push(local.join("Programs/Microsoft VS Code/resources/app/extensions/ms-vscode.js-debug/src/dapDebugServer.js"));
    }
    candidates.push("/usr/share/code/resources/app/extensions/ms-vscode.js-debug/src/dapDebugServer.js".into());
    candidates.into_iter().find(|p| p.is_file())
}

/// Folder for adapters we download ourselves: `<settings dir>/adapters`.
pub fn adapters_dir() -> std::path::PathBuf {
    crate::settings::config_dir().unwrap_or_else(|| std::path::PathBuf::from(".codeyard")).join("adapters")
}

/// `Debug: Install JavaScript Debugger` downloads this (Microsoft js-debug, MIT).
pub const JS_DEBUG_URL: &str = "https://github.com/microsoft/vscode-js-debug/releases/download/v1.140.0/js-debug-dap-v1.140.0.tar.gz";

/// SHA-256 of the downloaded archive (v1.140.0 release file): a partial, corrupt or tampered download is rejected without being run.
/// Updated together with `JS_DEBUG_URL` when the version is bumped.
pub const JS_DEBUG_SHA256: &str = "27dab92937ec1ab35821ae955aac867544fe06a1b6307229049f2d789af10968";

/// Verify the js-debug archive and install under `dir/js-debug`: SHA-256 first, then extract to a temp folder, look there for the expected file,
/// finally a single `rename`. An interrupted install is not counted as "installed" by `find_js_debug()`.
pub fn install_js_debug_archive(dir: &std::path::Path, archive: &std::path::Path, sha256: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let got: String = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
    if !got.eq_ignore_ascii_case(sha256) {
        return Err(crate::i18n::t("the checksum does not match: the download is incomplete or was altered").into());
    }
    let tmp = dir.join(format!(".js-debug-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = (|| {
        let out = std::process::Command::new("tar").arg("xzf").arg(archive).arg("-C").arg(&tmp).output().map_err(|e| format!("tar: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        let src = tmp.join("js-debug");
        if !src.join("src/dapDebugServer.js").is_file() {
            return Err(crate::i18n::t("the archive does not contain js-debug/src/dapDebugServer.js").into());
        }
        let dest = dir.join("js-debug");
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&src, &dest).map_err(|e| format!("{}: {e}", dest.display()))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn node_adapter(server: Option<std::path::PathBuf>) -> Result<Adapter, String> {
    match server {
        Some(server) => Ok(Adapter { id: "pwa-node".into(), command: "node".into(), args: vec![server.to_string_lossy().into_owned(), "${port}".into(), "127.0.0.1".into()], connect: None }),
        None => Err(crate::i18n::t("JavaScript debugging needs Microsoft's js-debug adapter: run “Debug: Install JavaScript Debugger” (a one-time download, MIT licensed), or install VS Code (its copy is used).").into()),
    }
}

pub fn adapter_for(ty: &str) -> Result<Adapter, String> {
    let adapter = |id: &str, command: &str, args: &[&str]| Adapter {
        id: id.into(),
        command: command.into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        connect: None,
    };
    match ty {
        // No shell script on Windows: the adapter comes straight from PATH.
        "lldb" | "codelldb" | "cppdbg" | "lldb-dap" | "cppvsdbg" if cfg!(windows) => Ok(adapter("lldb-dap", "lldb-dap", &[])),
        "lldb" | "codelldb" | "cppdbg" | "lldb-dap" | "cppvsdbg" => Ok(adapter("lldb-dap", "sh", &["-c", LLDB_DAP])),
        // Delve only speaks TCP: it connects to us (`${clientAddr}` is filled in dap.rs).
        "go" | "delve" => Ok(adapter("go", "dlv", &["dap", "--client-addr", "${clientAddr}"])),
        "python" | "debugpy" => Ok(adapter("debugpy", "python3", &["-m", "debugpy.adapter"])),
        // js-debug speaks TCP: `${port}` is filled in dap.rs.
        "node" | "pwa-node" | "node-terminal" => node_adapter(find_js_debug()),
        other => Err(crate::i18n::tf(
            "No debug adapter for type “{0}”. Supported: lldb (C, C++, Rust, Swift), go, python; or add {1} to the configuration.",
            &[&other, &"\"debugAdapter\": { \"command\": …, \"args\": […] }"],
        )),
    }
}

/// From `launch.json`: variables (`${workspaceFolder}`, `${file}`...) are expanded, `preLaunchTask`
/// is looked up by label in `tasks` (if not found the label isn't treated as a command, ignored).
pub fn parse_launch_json(text: &str, root: &str, file: Option<&str>, tasks: &[Task]) -> Vec<LaunchConfig> {
    let Ok(v) = serde_json::from_str::<Value>(&theme::themes::strip_jsonc(text)) else { return Vec::new() };
    let items = match &v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("configurations").and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    items
        .iter()
        .filter_map(|c| {
            let name = c["name"].as_str()?.to_string();
            let ty = c["type"].as_str().unwrap_or_default();
            // `connect: { host, port }` (debugpy `--listen`, VS Code Python form) or `debugAdapter: { host, port }` for your own
            // adapter: connect straight to that server over TCP, no process spawn.
            let direct =
                |v: &Value| Some((v["host"].as_str().unwrap_or("127.0.0.1").to_string(), v["port"].as_u64().filter(|p| (1..=65535).contains(p))? as u16));
            let is_attach = c["request"].as_str() == Some("attach");
            let adapter = if let Some(addr) = direct(&c["debugAdapter"]).filter(|_| c["debugAdapter"]["command"].is_null()) {
                Ok(Adapter { id: ty.to_string(), command: String::new(), args: Vec::new(), connect: Some(addr) })
            } else if let (true, true, Some(addr)) = (is_attach, matches!(ty, "python" | "debugpy"), direct(&c["connect"])) {
                Ok(Adapter { id: "debugpy".into(), command: String::new(), args: Vec::new(), connect: Some(addr) })
            } else {
                match c["debugAdapter"]["command"].as_str() {
                    Some(cmd) => Ok(Adapter {
                        id: ty.to_string(),
                        command: cmd.to_string(),
                        args: c["debugAdapter"]["args"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                        connect: None,
                    }),
                    None => adapter_for(ty),
                }
            };
            let mut args = c.clone();
            let is_node = matches!(ty, "node" | "pwa-node" | "node-terminal");
            if let Some(o) = args.as_object_mut() {
                for k in ["name", "type", "request", "preLaunchTask", "debugAdapter"] {
                    o.remove(k);
                }
                // For debugpy connected directly, the address is the connection, not an `attach` argument.
                if matches!(adapter, Ok(Adapter { connect: Some(_), .. })) {
                    o.remove("connect");
                }
                // js-debug wants type and name inside the options; let output reach the DAP console.
                if is_node {
                    o.insert("type".into(), json!("pwa-node"));
                    o.insert("name".into(), json!(name));
                    o.entry("console").or_insert_with(|| json!("internalConsole"));
                }
                // Project root if no working directory is given.
                o.entry("cwd").or_insert_with(|| json!("${workspaceFolder}"));
            }
            let args = expand_value(&args, root, file);
            let pre_launch =
                c["preLaunchTask"].as_str().and_then(|label| tasks.iter().find(|t| t.label == label)).map(|t| expand_variables(&t.command, root, file));
            Some(LaunchConfig { name, adapter, request: c["request"].as_str().unwrap_or("launch").to_string(), args, pre_launch })
        })
        .collect()
}

fn expand_value(v: &Value, root: &str, file: Option<&str>) -> Value {
    match v {
        Value::String(s) => Value::String(expand_variables(s, root, file)),
        Value::Array(a) => Value::Array(a.iter().map(|x| expand_value(x, root, file)).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), expand_value(x, root, file))).collect()),
        other => other.clone(),
    }
}

/// Auto from the project: `Cargo.toml` (`[package] name`) -> `cargo build` + `target/debug/<name>`;
/// `go.mod` -> run the package with `dlv dap`; open `.py` file -> debugpy.
pub fn detect(cargo_toml: Option<&str>, has_go_mod: bool, file: Option<&str>, root: &str) -> Vec<LaunchConfig> {
    let mut out = Vec::new();
    if let Some(name) = cargo_toml.and_then(package_name) {
        out.push(LaunchConfig {
            name: crate::i18n::tf("Debug Rust: {}", &[&name]),
            adapter: adapter_for("lldb"),
            request: "launch".into(),
            args: json!({ "program": format!("{}/target/debug/{name}", root.trim_end_matches('/')), "args": [], "cwd": root }),
            pre_launch: Some("cargo build".into()),
        });
    }
    if has_go_mod {
        out.push(LaunchConfig {
            name: crate::i18n::t("Debug Go package").into(),
            adapter: adapter_for("go"),
            request: "launch".into(),
            // outputMode remote: program output reaches the console as a DAP event (default: delve's stdout).
            args: json!({ "mode": "debug", "program": root, "cwd": root, "outputMode": "remote" }),
            pre_launch: None,
        });
    }
    if let Some(f) = file.filter(|f| [".js", ".mjs", ".cjs"].iter().any(|e| f.ends_with(e))) {
        let base = f.rsplit('/').next().unwrap_or(f);
        out.push(LaunchConfig {
            name: crate::i18n::tf("Debug Node: {}", &[&base]),
            adapter: adapter_for("node"),
            request: "launch".into(),
            args: json!({ "type": "pwa-node", "name": format!("Debug Node: {base}"), "program": f, "cwd": root, "console": "internalConsole" }),
            pre_launch: None,
        });
    }
    if let Some(f) = file.filter(|f| f.ends_with(".py")) {
        let base = f.rsplit('/').next().unwrap_or(f);
        out.push(LaunchConfig {
            name: crate::i18n::tf("Debug Python: {}", &[&base]),
            adapter: adapter_for("python"),
            request: "launch".into(),
            args: json!({ "program": f, "cwd": root, "console": "internalConsole" }),
            pre_launch: None,
        });
    }
    out
}

/// `name = "..."` in the `[package]` section (lines outside the table are ignored).
fn package_name(cargo: &str) -> Option<String> {
    let mut in_package = false;
    for line in cargo.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_package = l == "[package]";
        } else if in_package
            && let Some((k, v)) = l.split_once('=')
            && k.trim() == "name"
        {
            return Some(v.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_of(path: &std::path::Path) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(std::fs::read(path).unwrap()).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn archive_with(files: &[(&str, &str)], name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("codeyard-jsdbg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let content = root.join("content");
        for (rel, text) in files {
            let p = content.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let archive = root.join("a.tar.gz");
        let ok = std::process::Command::new("tar").arg("czf").arg(&archive).arg("-C").arg(&content).arg(".").status().map(|s| s.success()).unwrap_or(false);
        assert!(ok, "tar is available");
        let dest = root.join("adapters");
        std::fs::create_dir_all(&dest).unwrap();
        (archive, dest)
    }

    #[test]
    fn the_js_debug_archive_is_verified_and_installed_whole_or_not_at_all() {
        let (archive, dest) = archive_with(&[("js-debug/src/dapDebugServer.js", "// server")], "ok");
        let sha = sha256_of(&archive);
        // Wrong checksum (truncated or altered download): nothing is installed.
        let e = install_js_debug_archive(&dest, &archive, &"0".repeat(64)).unwrap_err();
        assert!(e.contains("checksum"), "{e}");
        assert!(!dest.join("js-debug").exists());
        // Right checksum: installed, no temporary folder left.
        install_js_debug_archive(&dest, &archive, &sha).unwrap();
        assert!(dest.join("js-debug/src/dapDebugServer.js").is_file());
        assert_eq!(std::fs::read_dir(&dest).unwrap().flatten().count(), 1, "the install folder was cleaned up");
        // Installing again over an existing copy works.
        install_js_debug_archive(&dest, &archive, &sha.to_uppercase()).unwrap();
        // An archive without the expected file is refused even with a matching checksum.
        let (other, dest2) = archive_with(&[("readme.txt", "x")], "wrong");
        let e = install_js_debug_archive(&dest2, &other, &sha256_of(&other)).unwrap_err();
        assert!(e.contains("does not contain"), "{e}");
        assert!(!dest2.join("js-debug").exists(), "a half install is never left as 'installed'");
    }

    fn task(label: &str, command: &str) -> Task {
        Task { label: label.into(), command: command.into(), source: "tasks.json" }
    }

    #[test]
    fn launch_json_expands_variables_and_finds_the_pre_launch_task() {
        let text = r#"{ "version": "0.2.0", "configurations": [
            // VS Code form
            { "name": "Run app", "type": "lldb", "request": "launch", "program": "${workspaceFolder}/build/app",
              "args": ["--port", "${workspaceFolderBasename}"], "preLaunchTask": "build" },
            { "name": "Py", "type": "python", "request": "launch", "program": "${file}", "cwd": "${workspaceFolder}/src" },
            { "name": "Ruby", "type": "ruby", "program": "x.rb" },
            { "name": "Mine", "type": "custom", "debugAdapter": { "command": "my-dap", "args": ["--stdio"] }, "program": "x" },
            { "type": "lldb" }
        ] }"#;
        let c = parse_launch_json(text, "/p/demo", Some("/p/demo/src/m.py"), &[task("build", "make -C ${workspaceFolder}")]);
        assert_eq!(c.len(), 4, "unnamed configuration is skipped");
        assert_eq!(c[0].name, "Run app");
        assert_eq!(c[0].args["program"], "/p/demo/build/app");
        assert_eq!(c[0].args["args"], json!(["--port", "demo"]));
        assert_eq!(c[0].args["cwd"], "/p/demo", "project root when cwd is not given");
        assert!(c[0].args.get("preLaunchTask").is_none() && c[0].args.get("type").is_none(), "meta fields do not go to the adapter");
        assert_eq!(c[0].pre_launch.as_deref(), Some("make -C /p/demo"));
        assert_eq!(c[0].adapter.as_ref().unwrap().id, "lldb-dap");
        assert_eq!((c[1].args["program"].as_str(), c[1].args["cwd"].as_str()), (Some("/p/demo/src/m.py"), Some("/p/demo/src")));
        assert_eq!(c[1].adapter.as_ref().unwrap().args, ["-m", "debugpy.adapter"]);
        assert!(c[2].adapter.as_ref().unwrap_err().contains("ruby"), "unsupported type gives an explicit message");
        assert_eq!(c[3].adapter.as_ref().unwrap().command, "my-dap");
        assert_eq!(c[3].request, "launch");
    }

    #[test]
    fn attach_by_socket_connects_directly_instead_of_spawning_an_adapter() {
        let text = r#"{ "configurations": [
            { "name": "Py", "type": "python", "request": "attach", "connect": { "host": "localhost", "port": 5678 }, "justMyCode": false },
            { "name": "PyDefaultHost", "type": "debugpy", "request": "attach", "connect": { "port": 5679 } },
            { "name": "Headless", "type": "go", "request": "attach", "mode": "remote", "debugAdapter": { "host": "127.0.0.1", "port": 2345 } },
            { "name": "NotAttach", "type": "python", "request": "launch", "program": "x.py", "connect": { "port": 1 } },
            { "name": "BadPort", "type": "python", "request": "attach", "connect": { "port": 0 } }
        ] }"#;
        let c = parse_launch_json(text, "/p", None, &[]);
        let a = |i: usize| c[i].adapter.clone().unwrap();
        assert_eq!(a(0).connect, Some(("localhost".to_string(), 5678)));
        assert_eq!(a(0).id, "debugpy");
        assert_eq!(a(1).connect, Some(("127.0.0.1".to_string(), 5679)));
        assert_eq!(a(2).connect, Some(("127.0.0.1".to_string(), 2345)));
        assert!(c[0].args.get("connect").is_none() && c[0].args["justMyCode"] == false, "address is not an attach argument; other options pass through");
        assert_eq!(a(3).connect, None, "connect is not an address for launch");
        assert_eq!(a(4).connect, None, "invalid port is ignored (falls back to debugpy.adapter)");
    }

    #[test]
    fn bad_json_gives_no_configurations() {
        assert!(parse_launch_json("{ not json", "/p", None, &[]).is_empty());
        assert!(parse_launch_json("{}", "/p", None, &[]).is_empty());
    }

    #[test]
    fn detects_cargo_go_and_python() {
        let cargo = "[workspace]\nmembers = []\n\n[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[dependencies]\nname = \"not-this\"\n";
        let c = detect(Some(cargo), true, Some("/p/a.py"), "/p/");
        assert_eq!(c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Debug Rust: demo", "Debug Go package", "Debug Python: a.py"]);
        assert_eq!(c[0].args["program"], "/p/target/debug/demo");
        assert_eq!(c[0].pre_launch.as_deref(), Some("cargo build"));
        assert!(detect(Some("[workspace]\nmembers = []\n"), false, Some("/p/a.rs"), "/p").is_empty(), "workspace without a package and a non-.py file");
    }

    #[test]
    fn node_uses_js_debug_over_tcp_and_explains_when_it_is_missing() {
        let a = node_adapter(Some("/x/js-debug/src/dapDebugServer.js".into())).unwrap();
        assert_eq!((a.id.as_str(), a.command.as_str()), ("pwa-node", "node"));
        assert_eq!(a.args, ["/x/js-debug/src/dapDebugServer.js", "${port}", "127.0.0.1"]);
        assert_eq!(crate::dap::transport_of(&a.args), crate::dap::Transport::Connect);
        assert!(node_adapter(None).unwrap_err().contains("Install JavaScript Debugger"));
        // Go: delve connects to us.
        assert_eq!(crate::dap::transport_of(&adapter_for("go").unwrap().args), crate::dap::Transport::Accept);
        assert_eq!(crate::dap::transport_of(&adapter_for("python").unwrap().args), crate::dap::Transport::Stdio);
    }

    #[test]
    fn node_launch_arguments_carry_type_name_and_console() {
        let c = parse_launch_json(
            r#"{ "configurations": [ { "name": "Server", "type": "node", "request": "launch", "program": "${workspaceFolder}/server.js" } ] }"#,
            "/p",
            None,
            &[],
        );
        assert_eq!(c[0].args["type"], "pwa-node");
        assert_eq!(c[0].args["name"], "Server");
        assert_eq!(c[0].args["console"], "internalConsole");
        assert_eq!(c[0].args["program"], "/p/server.js");
        let d = detect(None, false, Some("/p/app.mjs"), "/p");
        assert_eq!(d[0].name, "Debug Node: app.mjs");
        assert_eq!(d[0].args["program"], "/p/app.mjs");
    }

    #[test]
    fn unknown_type_message_lists_what_is_supported() {
        let e = adapter_for("ruby").unwrap_err();
        assert!(e.contains("lldb") && e.contains("go") && e.contains("python") && e.contains("debugAdapter"), "{e}");
    }
}
