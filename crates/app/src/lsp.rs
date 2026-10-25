//! Language server client. The server runs as the agent's piped process
//! on the machine where the files live (local or SSH); this file holds only
//! JSON-RPC: framing, request/response matching, replies to the server's own requests.
//! Independent of the UI; blocking calls are made on background threads.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use host::{Host, ProcEvent};
use serde_json::{Value, json};

// --- Servers ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// Key in the settings and the LSP languageId ("rust", "python"...).
    pub language: String,
    pub command: String,
    pub args: Vec<String>,
}

/// File -> (languageId, server key). One server can carry several languages.
/// Keys without an entry in `DEFAULT_SERVERS` (json, yaml, toml) are opt-in through `lsp.servers`.
pub fn language_of(path: &str) -> Option<(&'static str, &'static str)> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if matches!(name, "Gemfile" | "Rakefile" | "Podfile" | "Vagrantfile" | "Guardfile") {
        return Some(("ruby", "ruby"));
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    Some(match ext.as_str() {
        "rs" => ("rust", "rust"),
        "c" | "h" => ("c", "c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => ("cpp", "c"),
        "go" => ("go", "go"),
        "py" | "pyi" => ("python", "python"),
        "ts" | "mts" | "cts" => ("typescript", "typescript"),
        "tsx" => ("typescriptreact", "typescript"),
        "js" | "mjs" | "cjs" => ("javascript", "typescript"),
        "jsx" => ("javascriptreact", "typescript"),
        "java" => ("java", "java"),
        "cs" => ("csharp", "csharp"),
        "php" | "phtml" => ("php", "php"),
        "rb" | "rake" | "gemspec" | "ru" => ("ruby", "ruby"),
        "kt" | "kts" => ("kotlin", "kotlin"),
        "swift" => ("swift", "swift"),
        "lua" => ("lua", "lua"),
        "sh" | "bash" => ("shellscript", "bash"),
        "html" | "htm" => ("html", "html"),
        "css" => ("css", "css"),
        "scss" => ("scss", "css"),
        "less" => ("less", "css"),
        // No default server: the built-in schema engine already checks JSON and YAML, and the usual servers
        // (vscode-json-language-server, yaml-language-server, taplo) download schemas on their own, outside
        // `schema.download` and the confirmation of remote requests. Opt in with `lsp.servers`.
        "json" => ("json", "json"),
        "jsonc" => ("jsonc", "json"),
        "yaml" | "yml" => ("yaml", "yaml"),
        "toml" => ("toml", "toml"),
        _ => return None,
    })
}

/// Default server per key: (key, command, arguments, install hint shown when it is missing).
/// Commands are looked up on the host's PATH; macOS ships a `sourcekit-lsp` shim next to `xcrun`.
pub const DEFAULT_SERVERS: &[(&str, &str, &[&str], &str)] = &[
    ("rust", "rust-analyzer", &[], "rustup component add rust-analyzer"),
    ("c", "clangd", &[], "install clangd (Xcode tools, apt install clangd)"),
    ("go", "gopls", &[], "go install golang.org/x/tools/gopls@latest"),
    ("python", "pyright-langserver", &["--stdio"], "npm i -g pyright"),
    ("typescript", "typescript-language-server", &["--stdio"], "npm i -g typescript-language-server typescript"),
    ("java", "jdtls", &[], "brew install jdtls, or unpack Eclipse JDT LS and put jdtls on PATH"),
    ("csharp", "csharp-ls", &[], "dotnet tool install -g csharp-ls"),
    ("php", "intelephense", &["--stdio"], "npm i -g intelephense"),
    ("ruby", "ruby-lsp", &[], "gem install ruby-lsp"),
    ("kotlin", "kotlin-language-server", &[], "brew install kotlin-language-server"),
    ("swift", "sourcekit-lsp", &[], "install Xcode or a Swift toolchain (it contains sourcekit-lsp)"),
    ("lua", "lua-language-server", &[], "brew install lua-language-server"),
    ("bash", "bash-language-server", &["start"], "npm i -g bash-language-server"),
    ("html", "vscode-html-language-server", &["--stdio"], "npm i -g vscode-langservers-extracted"),
    ("css", "vscode-css-language-server", &["--stdio"], "npm i -g vscode-langservers-extracted"),
];

fn default_server(key: &str) -> Option<(&'static str, &'static [&'static str])> {
    DEFAULT_SERVERS.iter().find(|d| d.0 == key).map(|d| (d.1, d.2))
}

/// Title of a list of locations (`what` names the kind: the languages differ in plural forms, so each kind has its own key).
pub fn locations_title(what: &str, n: usize) -> String {
    use crate::i18n::tn;
    match what {
        "references" => tn("{} reference", "{} references", n, &[]),
        "bookmarks" => tn("{} bookmark", "{} bookmarks", n, &[]),
        "implementations" => tn("{} implementation", "{} implementations", n, &[]),
        "definition" => tn("{} definition", "{} definitions", n, &[]),
        "type definition" => tn("{} type definition", "{} type definitions", n, &[]),
        "incoming calls" => tn("{} incoming call", "{} incoming calls", n, &[]),
        "outgoing calls" => tn("{} outgoing call", "{} outgoing calls", n, &[]),
        "supertypes" => tn("{} supertype", "{} supertypes", n, &[]),
        "subtypes" => tn("{} subtype", "{} subtypes", n, &[]),
        _ => format!("{n} {what}"),
    }
}

/// Message when a location request found nothing (`what` as in `locations_title`).
pub fn none_found(what: &str) -> String {
    use crate::i18n::t;
    match what {
        "references" => t("No references found"),
        "implementations" => t("No implementations found"),
        "definition" => t("No definition found"),
        "type definition" => t("No type definition found"),
        "incoming calls" => t("No incoming calls found"),
        "outgoing calls" => t("No outgoing calls found"),
        "supertypes" => t("No supertypes found"),
        "subtypes" => t("No subtypes found"),
        _ => return format!("No {what} found"),
    }
    .to_string()
}

/// Message when the server lacks a hierarchy capability.
pub fn unsupported(what: &str) -> String {
    use crate::i18n::t;
    match what {
        "incoming calls" => t("This language server does not support incoming calls"),
        "outgoing calls" => t("This language server does not support outgoing calls"),
        "supertypes" => t("This language server does not support supertypes"),
        "subtypes" => t("This language server does not support subtypes"),
        _ => return format!("This language server does not support {what}"),
    }
    .to_string()
}

/// The install hint in the user's language (most hints are shell commands and stay as they are).
pub fn install_hint_text(hint: &str) -> String {
    use crate::i18n::t;
    match hint {
        "install clangd (Xcode tools, apt install clangd)" => t("install clangd (Xcode tools, apt install clangd)"),
        "brew install jdtls, or unpack Eclipse JDT LS and put jdtls on PATH" => t("brew install jdtls, or unpack Eclipse JDT LS and put jdtls on PATH"),
        "install Xcode or a Swift toolchain (it contains sourcekit-lsp)" => t("install Xcode or a Swift toolchain (it contains sourcekit-lsp)"),
        other => other,
    }
    .to_string()
}

/// Install command of the default server (shown in the missing-server notice).
pub fn install_hint(key: &str) -> Option<&'static str> {
    DEFAULT_SERVERS.iter().find(|d| d.0 == key).map(|d| d.3)
}

/// Server definitions from the settings: language → (command, arguments); `None` disables that language.
pub type ServerOverrides = [(String, Option<(String, Vec<String>)>)];

/// (line, UTF-16 column) range.
pub type Range16 = ((u32, u32), (u32, u32));

/// Configuration for a server key: settings override the defaults.
pub fn server_for(key: &str, overrides: &ServerOverrides) -> Option<ServerConfig> {
    if let Some((_, o)) = overrides.iter().rev().find(|(k, _)| k == key) {
        return o.as_ref().map(|(command, args)| ServerConfig { language: key.into(), command: command.clone(), args: args.clone() });
    }
    let (command, args) = default_server(key)?;
    Some(ServerConfig { language: key.into(), command: command.into(), args: args.iter().map(|s| s.to_string()).collect() })
}

// --- URI ---

pub fn path_to_uri(path: &str) -> String {
    schema::file_uri::from_path(path)
}

pub fn uri_to_path(uri: &str) -> Option<String> {
    schema::file_uri::to_path(uri)
}

// --- Framing ---

pub fn encode(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Upper bound for a single LSP/DAP message (64 MB; the largest real responses are a few MB).
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Extracts complete messages from output that arrives in pieces.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, data: &[u8]) -> Vec<Value> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some(head_end) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&self.buf[..head_end]).to_string();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length:").or_else(|| l.strip_prefix("content-length:")))
                .and_then(|v| v.trim().parse::<usize>().ok());
            let Some(len) = len else {
                // Malformed header: skip (the server may have logged to stdout).
                self.buf.drain(..head_end + 4);
                continue;
            };
            // Oversized (or overflowing) length: a broken or hostile server. Waiting for the body would bloat memory and
            // `start + len` could overflow; since the stream is untrustworthy, the accumulated data is discarded.
            if len > MAX_MESSAGE {
                self.buf.clear();
                break;
            }
            let start = head_end + 4;
            if self.buf.len() < start + len {
                break;
            }
            if let Ok(v) = serde_json::from_slice(&self.buf[start..start + len]) {
                out.push(v);
            }
            self.buf.drain(..start + len);
        }
        // Without this, unbounded data before the header ends (a server dumping endless text to stdout) could bloat memory: whatever is left after the loop
        // is either headerless or a partial body, and the body limit is already `MAX_MESSAGE`.
        if self.buf.len() > MAX_MESSAGE + 64 * 1024 {
            self.buf.clear();
        }
        out
    }
}

// --- Diagnostics ---

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    /// (line, UTF-16 column), as the server sent it.
    pub start: (u32, u32),
    pub end: (u32, u32),
    /// 1 error, 2 warning, 3 information, 4 hint.
    pub severity: u8,
    pub message: String,
    pub source: Option<String>,
}

fn position(v: &Value) -> (u32, u32) {
    (v["line"].as_u64().unwrap_or(0) as u32, v["character"].as_u64().unwrap_or(0) as u32)
}

pub fn parse_range(v: &Value) -> Range16 {
    (position(&v["start"]), position(&v["end"]))
}

pub fn parse_diagnostics(params: &Value) -> Option<(String, Vec<Diagnostic>)> {
    let path = uri_to_path(params["uri"].as_str()?)?;
    let diags = params["diagnostics"]
        .as_array()?
        .iter()
        .map(|d| {
            let (start, end) = parse_range(&d["range"]);
            Diagnostic {
                start,
                end,
                severity: d["severity"].as_u64().unwrap_or(1).clamp(1, 4) as u8,
                message: d["message"].as_str().unwrap_or_default().to_string(),
                source: d["source"].as_str().map(str::to_string),
            }
        })
        .collect();
    Some((path, diags))
}

// --- Client ---

#[derive(Debug, Clone)]
pub enum ServerEvent {
    Diagnostics {
        path: String,
        diagnostics: Vec<Diagnostic>,
    },
    /// Ongoing work ("indexing 45%"); `None`: finished.
    Progress(Option<String>),
    /// Message from the server to the user (error/warning).
    Message(String),
    /// `window/logMessage`: for the Output panel (type: 1 error … 4 log).
    Log {
        level: u8,
        text: String,
    },
    /// The server wants an edit applied (after a code action command):
    /// once applied, `Client::respond(id, …)`.
    ApplyEdit {
        id: Value,
        edit: Value,
    },
    /// `window/showMessageRequest`: the user picks one of the options (or dismisses it);
    /// the reply is `Client::respond(id, {"title": …} | null)`.
    Question {
        id: Value,
        message: String,
        actions: Vec<String>,
    },
    Exited(Option<i32>),
}

/// Queue discipline for events the UI has not taken yet. Only state that a later message fully replaces is folded together:
/// the diagnostics of a file (`publishDiagnostics` always carries the whole list) and the progress line. Everything else
/// (questions, edits the server asks for, messages, log lines, exit) stays queued in order and is never dropped.
pub fn merge_server_event(queue: &mut std::collections::VecDeque<ServerEvent>, ev: ServerEvent) {
    let slot = match &ev {
        ServerEvent::Diagnostics { path, .. } => queue.iter_mut().find(|e| matches!(e, ServerEvent::Diagnostics { path: p, .. } if p == path)),
        ServerEvent::Progress(_) => queue.iter_mut().find(|e| matches!(e, ServerEvent::Progress(_))),
        _ => None,
    };
    match slot {
        Some(slot) => *slot = ev,
        None => queue.push_back(ev),
    }
}

type Pending = Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value, String>>>>>;

pub struct Client {
    pub config: ServerConfig,
    host: Arc<dyn Host>,
    proc: u32,
    next_id: AtomicI64,
    pending: Pending,
    alive: Arc<AtomicBool>,
    pub capabilities: Value,
}

impl Client {
    /// Start the server and handshake. Error if the command is not found or fails to start.
    pub fn start(host: Arc<dyn Host>, config: ServerConfig, root: &str, events: mpsc::Sender<ServerEvent>) -> Result<Arc<Client>, String> {
        let proc = host.proc_spawn(&config.command, config.args.clone(), Some(root.to_string())).map_err(|e| e.to_string())?;
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (writer_host, id) = (host.clone(), proc.id);
        // Reader: responses go to whoever is waiting, notifications to the event channel; it answers
        // the server's own requests (configuration, registration) itself.
        {
            let (pending, alive) = (pending.clone(), alive.clone());
            std::thread::Builder::new()
                .name(format!("lsp-{}", config.language))
                .spawn(move || {
                    let mut dec = Decoder::default();
                    while let Ok(ev) = proc.events.recv() {
                        match ev {
                            ProcEvent::Output(data) => {
                                for msg in dec.push(&data) {
                                    route(msg, &pending, &events, &writer_host, id);
                                }
                            }
                            ProcEvent::Exit(code) => {
                                alive.store(false, Ordering::Relaxed);
                                for (_, tx) in pending.lock().unwrap().drain() {
                                    let _ = tx.send(Err(match code {
                                        Some(127) => "not found".to_string(),
                                        Some(c) => format!("exited with code {c}"),
                                        None => "stopped".to_string(),
                                    }));
                                }
                                let _ = events.send(ServerEvent::Exited(code));
                                break;
                            }
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        let mut client = Client { config, host, proc: id, next_id: AtomicI64::new(1), pending, alive, capabilities: Value::Null };
        let init = client.request("initialize", initialize_params(root), Duration::from_secs(60))?;
        client.capabilities = init["capabilities"].clone();
        client.notify("initialized", json!({}));
        client.notify("workspace/didChangeConfiguration", json!({ "settings": {} }));
        Ok(Arc::new(client))
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn send(&self, msg: Value) {
        self.host.proc_input(self.proc, encode(&msg));
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Send a request and wait for the response (called on a background thread).
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if !self.is_alive() {
            return Err(crate::i18n::t("language server is not running").into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let result = rx.recv_timeout(timeout).map_err(|_| {
            self.pending.lock().unwrap().remove(&id);
            // Tell the server we gave up too (so it drops the expensive work).
            self.notify("$/cancelRequest", json!({ "id": id }));
            crate::i18n::tf("{0}: no answer from {1}", &[&method, &self.config.command])
        });
        result?
    }

    /// Reply to a request from the server (e.g. `workspace/applyEdit`).
    pub fn respond(&self, id: Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// Shut down politely (short wait), then kill the process.
    pub fn shutdown(&self) {
        if self.is_alive() {
            let _ = self.request("shutdown", Value::Null, Duration::from_millis(500));
            self.notify("exit", Value::Null);
        }
        self.host.proc_kill(self.proc);
    }

    /// Whether the server announced this character as a completion trigger.
    pub fn is_trigger(&self, ch: char) -> bool {
        self.capabilities["completionProvider"]["triggerCharacters"].as_array().is_some_and(|a| a.iter().any(|t| t.as_str().is_some_and(|t| t.starts_with(ch))))
    }

    /// Whether the server announced this capability (`inlayHintProvider`, `foldingRangeProvider` …).
    pub fn supports(&self, capability: &str) -> bool {
        !matches!(&self.capabilities[capability], Value::Null | Value::Bool(false))
    }

    pub fn wants_save(&self) -> bool {
        let sync = &self.capabilities["textDocumentSync"];
        sync.is_object() && (sync["save"].is_object() || sync["save"].as_bool() == Some(true))
    }
}

fn route(msg: Value, pending: &Pending, events: &mpsc::Sender<ServerEvent>, host: &Arc<dyn Host>, proc: u32) {
    let method = msg["method"].as_str();
    if std::env::var_os("CODEYARD_LSP_TRACE").is_some() {
        eprintln!("lsp<- {} {} bytes", method.unwrap_or("(response)"), msg.to_string().len());
    }
    match (msg.get("id"), method) {
        // Response.
        (Some(id), None) => {
            let Some(id) = id.as_i64() else { return };
            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                let result = match msg.get("error") {
                    Some(e) => Err(e["message"].as_str().unwrap_or("error").to_string()),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(result);
            }
        }
        // Request from the server: some servers wait if it is not answered.
        (Some(id), Some(method)) => {
            let result = match method {
                "workspace/configuration" => {
                    let n = msg["params"]["items"].as_array().map_or(0, |a| a.len());
                    json!(vec![Value::Null; n])
                }
                "window/workDoneProgress/create" | "client/registerCapability" | "client/unregisterCapability" => Value::Null,
                "workspace/workspaceFolders" => Value::Null,
                "window/showMessageRequest" => {
                    let p = &msg["params"];
                    let actions =
                        p["actions"].as_array().map(|a| a.iter().filter_map(|x| x["title"].as_str().map(str::to_string)).collect()).unwrap_or_default();
                    let message = p["message"].as_str().unwrap_or_default().to_string();
                    let _ = events.send(ServerEvent::Question { id: id.clone(), message, actions });
                    return; // the UI answers once the user chooses
                }
                "workspace/applyEdit" => {
                    let _ = events.send(ServerEvent::ApplyEdit { id: id.clone(), edit: msg["params"]["edit"].clone() });
                    return; // the UI answers once the edit is applied
                }
                _ => {
                    let reply = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "not supported" } });
                    host.proc_input(proc, encode(&reply));
                    return;
                }
            };
            host.proc_input(proc, encode(&json!({ "jsonrpc": "2.0", "id": id, "result": result })));
        }
        (None, Some("textDocument/publishDiagnostics")) => {
            if let Some((path, diagnostics)) = parse_diagnostics(&msg["params"]) {
                let _ = events.send(ServerEvent::Diagnostics { path, diagnostics });
            }
        }
        (None, Some("$/progress")) => {
            let v = &msg["params"]["value"];
            let text = match v["kind"].as_str() {
                Some("end") => None,
                _ => {
                    let title = v["title"].as_str().or(v["message"].as_str()).unwrap_or("working");
                    Some(match v["percentage"].as_u64() {
                        Some(p) => format!("{title} {p}%"),
                        None => title.to_string(),
                    })
                }
            };
            let _ = events.send(ServerEvent::Progress(text));
        }
        // 1 error, 2 warning; information messages are noise.
        (None, Some("window/logMessage")) => {
            let p = &msg["params"];
            let _ = events.send(ServerEvent::Log { level: p["type"].as_u64().unwrap_or(4) as u8, text: p["message"].as_str().unwrap_or_default().to_string() });
        }
        (None, Some("window/showMessage")) if msg["params"]["type"].as_u64().is_some_and(|t| t <= 2) => {
            let _ = events.send(ServerEvent::Message(msg["params"]["message"].as_str().unwrap_or_default().to_string()));
        }
        _ => {}
    }
}

/// Semantic token types announced to the server (the server supplies its own table via `legend`).
pub const SEMANTIC_TYPES: [&str; 14] = [
    "namespace",
    "type",
    "class",
    "enum",
    "interface",
    "struct",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "function",
    "method",
    "macro",
];

fn initialize_params(root: &str) -> Value {
    let uri = path_to_uri(root);
    let name = root.rsplit('/').next().unwrap_or(root);
    json!({
        "processId": Value::Null,
        "clientInfo": { "name": "codeyard", "version": env!("CARGO_PKG_VERSION") },
        "rootUri": uri,
        "rootPath": root,
        "workspaceFolders": [{ "uri": uri, "name": name }],
        "capabilities": {
            "general": { "positionEncodings": ["utf-16"] },
            "window": { "workDoneProgress": true },
            "workspace": {
                "configuration": true,
                "workspaceFolders": true,
                "didChangeConfiguration": { "dynamicRegistration": false },
                "fileOperations": { "willRename": true },
                "applyEdit": true,
                "workspaceEdit": { "documentChanges": true },
                "didChangeWatchedFiles": { "dynamicRegistration": false },
                "symbol": { "dynamicRegistration": false },
            },
            "textDocument": {
                "synchronization": { "didSave": true, "dynamicRegistration": false },
                "publishDiagnostics": { "relatedInformation": false },
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "definition": { "linkSupport": true },
                "references": {},
                "rename": { "prepareSupport": true },
                "inlayHint": {},
                "foldingRange": { "lineFoldingOnly": true },
                "documentLink": {},
                "callHierarchy": {},
                "documentHighlight": {},
                "codeLens": {},
                "semanticTokens": {
                    "requests": { "full": true },
                    "tokenTypes": SEMANTIC_TYPES,
                    "tokenModifiers": [],
                    "formats": ["relative"],
                },
                "typeHierarchy": {},
                "formatting": {},
                "rangeFormatting": {},
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "signatureHelp": { "signatureInformation": { "parameterInformation": { "labelOffsetSupport": true } } },
                "codeAction": {
                    "codeActionLiteralSupport": { "codeActionKind": { "valueSet": [
                        "", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"
                    ] } },
                    "resolveSupport": { "properties": ["edit"] },
                    "dataSupport": true,
                },
                "completion": {
                    "completionItem": {
                        "snippetSupport": true,
                        "insertReplaceSupport": false,
                        "documentationFormat": ["plaintext", "markdown"],
                        "resolveSupport": { "properties": ["documentation", "detail", "additionalTextEdits"] },
                    },
                    "contextSupport": true,
                },
            },
        },
    })
}

// --- Parsing responses ---

/// Completion suggestion: the edit to apply, the server's range or the word start.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    /// LSP CompletionItemKind (3 function, 6 variable...).
    pub kind: u8,
    pub filter: String,
    pub sort: String,
    /// Text to insert (expanded form if it is a snippet) and the cursor position within it (characters).
    pub insert: String,
    pub cursor: Option<usize>,
    /// Snippet tab stops (character ranges within the text, in navigation order).
    pub stops: Vec<(u32, std::ops::Range<usize>)>,
    /// Range given by the server (if absent, the word being typed is replaced).
    pub range: Option<Range16>,
    /// Additional edits such as auto-import.
    pub extra: Vec<(Range16, String)>,
    /// Documentation (converted to plain text); shown next to the selected item.
    pub doc: Option<String>,
    /// The server's original item for `completionItem/resolve` (only those carrying `data`).
    pub resolve: Option<std::sync::Arc<Value>>,
}

/// Words in the document when there is no language server (VS Code `editor.wordBasedSuggestions`).
/// `text`: the text around the cursor, `cursor`: byte position within it. Excludes the word
/// being typed (the one containing the cursor); words near the cursor come first. At most `limit` words.
pub fn buffer_words(text: &str, cursor: usize, limit: usize) -> Vec<CompletionItem> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut best: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (is_word(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                start = None;
                let word = &text[s..i];
                // The word containing the cursor is the one being typed; single letters and numbers are not suggestions.
                if (s..=i).contains(&cursor) || word.chars().count() < 2 || word.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let dist = if i <= cursor { cursor - i } else { s - cursor };
                best.entry(word).and_modify(|d| *d = (*d).min(dist)).or_insert(dist);
            }
            _ => {}
        }
    }
    let mut words: Vec<(&str, usize)> = best.into_iter().collect();
    words.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(b.0)));
    words.truncate(limit);
    words
        .into_iter()
        .enumerate()
        .map(|(rank, (w, _))| CompletionItem {
            label: w.to_string(),
            detail: None,
            kind: 1, // text
            filter: w.to_string(),
            sort: format!("{rank:06}"),
            insert: w.to_string(),
            cursor: None,
            stops: Vec::new(),
            range: None,
            extra: Vec::new(),
            doc: None,
            resolve: None,
        })
        .collect()
}

pub fn parse_completions(v: &Value) -> (Vec<CompletionItem>, bool) {
    let (items, incomplete) = match v {
        Value::Array(a) => (a.as_slice(), false),
        Value::Object(_) => (v["items"].as_array().map(Vec::as_slice).unwrap_or(&[]), v["isIncomplete"].as_bool().unwrap_or(false)),
        _ => (&[][..], false),
    };
    let out = items
        .iter()
        .enumerate()
        .map(|(idx, it)| {
            let label = it["label"].as_str().unwrap_or_default().to_string();
            let snippet = it["insertTextFormat"].as_u64() == Some(2);
            let (raw, range) = match it.get("textEdit") {
                Some(te) if te.is_object() => {
                    let r = if te.get("range").is_some() { &te["range"] } else { &te["replace"] };
                    (te["newText"].as_str().unwrap_or(&label).to_string(), Some(parse_range(r)))
                }
                _ => (it["insertText"].as_str().unwrap_or(&label).to_string(), None),
            };
            let (insert, stops) = if snippet { parse_snippet(&raw) } else { (raw, Vec::new()) };
            let cursor = stops.first().map(|(_, r)| r.start);
            let extra = it["additionalTextEdits"]
                .as_array()
                .map(|a| a.iter().map(|e| (parse_range(&e["range"]), e["newText"].as_str().unwrap_or_default().to_string())).collect())
                .unwrap_or_default();
            CompletionItem {
                filter: it["filterText"].as_str().unwrap_or(&label).to_string(),
                sort: it["sortText"].as_str().unwrap_or(&label).to_string(),
                detail: it["detail"].as_str().map(|d| d.lines().next().unwrap_or(d).to_string()),
                kind: it["kind"].as_u64().unwrap_or(0) as u8,
                label,
                insert,
                cursor,
                stops,
                range,
                extra,
                doc: it.get("documentation").and_then(|d| hover_text(&serde_json::json!({ "contents": d }))).filter(|d| !d.trim().is_empty()),
                resolve: (idx < 1000).then(|| it.get("data").map(|_| std::sync::Arc::new(it.clone()))).flatten(),
            }
        })
        .collect();
    (out, incomplete)
}

/// Expand a snippet to plain text: `${1:name}` → `name`, `$1`/`$0` vanish; the cursor goes to the first
/// tab stop (or `$0` if none). Nested and choice stops are simplified.
#[cfg(test)]
pub fn expand_snippet(s: &str) -> (String, Option<usize>) {
    let (text, stops) = parse_snippet(s);
    (text, stops.first().map(|(_, r)| r.start))
}

/// Snippet → (text, tab stops). Stops are ordered: 1, 2, … then $0; each is a character range
/// within the text (the placeholder's default text; empty for `$1`). Nested
/// placeholders are resolved, and the first of the choices (`${1|a,b|}`) is written.
pub fn parse_snippet(s: &str) -> (String, Vec<(u32, std::ops::Range<usize>)>) {
    fn parse(chars: &[char], i: &mut usize, out: &mut String, count: &mut usize, stops: &mut Vec<(u32, std::ops::Range<usize>)>, until_brace: bool) {
        while *i < chars.len() {
            let c = chars[*i];
            if until_brace && c == '}' {
                return;
            }
            if c == '\\' && *i + 1 < chars.len() {
                out.push(chars[*i + 1]);
                *count += 1;
                *i += 2;
                continue;
            }
            if c == '$' && *i + 1 < chars.len() && chars[*i + 1].is_ascii_digit() {
                let mut j = *i + 1;
                let mut n = 0u32;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    n = n * 10 + chars[j].to_digit(10).unwrap_or(0);
                    j += 1;
                }
                stops.push((n, *count..*count));
                *i = j;
                continue;
            }
            if c == '$' && *i + 1 < chars.len() && chars[*i + 1] == '{' {
                let mut j = *i + 2;
                let mut n = 0u32;
                let digits = j;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    n = n * 10 + chars[j].to_digit(10).unwrap_or(0);
                    j += 1;
                }
                let start = *count;
                if j == digits {
                    // ${name} variable: no default, skip.
                    while j < chars.len() && chars[j] != '}' {
                        j += 1;
                    }
                    *i = j + 1;
                    continue;
                }
                match chars.get(j) {
                    Some(':') => {
                        *i = j + 1;
                        parse(chars, i, out, count, stops, true);
                        j = *i;
                    }
                    Some('|') => {
                        j += 1;
                        while j < chars.len() && chars[j] != ',' && chars[j] != '|' {
                            out.push(chars[j]);
                            *count += 1;
                            j += 1;
                        }
                        while j < chars.len() && chars[j] != '}' {
                            j += 1;
                        }
                    }
                    _ => {}
                }
                stops.push((n, start..*count));
                *i = j + 1;
                continue;
            }
            out.push(c);
            *count += 1;
            *i += 1;
        }
    }
    let chars: Vec<char> = s.chars().collect();
    let (mut out, mut count, mut stops, mut i) = (String::new(), 0usize, Vec::new(), 0usize);
    parse(&chars, &mut i, &mut out, &mut count, &mut stops, false);
    // The first of the stops with the same number; order is 1, 2, … , with 0 last.
    let mut seen = std::collections::HashSet::new();
    stops.retain(|(n, _)| seen.insert(*n));
    stops.sort_by_key(|(n, _)| if *n == 0 { u32::MAX } else { *n });
    (out, stops)
}

/// Hover contents → plain text (markdown markup is simplified).
pub fn hover_text(v: &Value) -> Option<String> {
    fn piece(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => o.get("value").and_then(Value::as_str).map(str::to_string),
            _ => None,
        }
    }
    let contents = &v["contents"];
    let text = match contents {
        Value::Array(a) => a.iter().filter_map(piece).collect::<Vec<_>>().join("\n\n"),
        other => piece(other)?,
    };
    // Reduce markdown to plain text: code fences, heading markers, separators,
    // inline code and bold markers go away; consecutive blank lines collapse to one.
    let mut lines: Vec<String> = Vec::new();
    for l in text.lines() {
        let t = l.trim_start();
        if t.starts_with("```") || matches!(t.trim(), "---" | "***" | "___") {
            continue;
        }
        let l = l.trim_start_matches('#').trim_start_matches(' ');
        let l = l.replace("**", "").replace('`', "");
        if l.trim().is_empty() && lines.last().is_none_or(|p| p.trim().is_empty()) {
            continue;
        }
        lines.push(l);
    }
    let out = lines.join("\n").trim().to_string();
    (!out.is_empty()).then_some(out)
}

/// Definition locations: Location, Location[] or LocationLink[]. (path, start)
pub fn parse_locations(v: &Value) -> Vec<(String, (u32, u32))> {
    let one = |l: &Value| -> Option<(String, (u32, u32))> {
        let uri = l["uri"].as_str().or(l["targetUri"].as_str())?;
        let range = if l.get("targetSelectionRange").is_some() { &l["targetSelectionRange"] } else { &l["range"] };
        Some((uri_to_path(uri)?, parse_range(range).0))
    };
    match v {
        Value::Array(a) => a.iter().filter_map(one).collect(),
        Value::Object(_) => one(v).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// WorkspaceEdit → edits per file. `changes` and `documentChanges`
/// (TextDocumentEdit) are supported; file create/delete/move is counted but not applied.
pub fn parse_workspace_edit(v: &Value) -> (Vec<(String, Vec<(Range16, String)>)>, usize) {
    let mut out: Vec<(String, Vec<(Range16, String)>)> = Vec::new();
    let mut skipped = 0;
    let mut add = |uri: &str, edits: &Value| {
        let Some(path) = uri_to_path(uri) else { return };
        let list: Vec<(Range16, String)> = edits
            .as_array()
            .map(|a| a.iter().map(|e| (parse_range(&e["range"]), e["newText"].as_str().unwrap_or_default().to_string())).collect())
            .unwrap_or_default();
        match out.iter_mut().find(|(p, _)| *p == path) {
            Some((_, l)) => l.extend(list),
            None => out.push((path, list)),
        }
    };
    if let Some(changes) = v["changes"].as_object() {
        for (uri, edits) in changes {
            add(uri, edits);
        }
    }
    if let Some(dc) = v["documentChanges"].as_array() {
        for d in dc {
            match d["textDocument"]["uri"].as_str() {
                Some(uri) => add(uri, &d["edits"]),
                None => skipped += 1, // create / rename / delete
            }
        }
    }
    (out, skipped)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub detail: String,
    pub kind: u8,
    pub line: u32,
    pub col: u32,
    pub depth: usize,
}

pub fn symbol_kind(kind: u8) -> &'static str {
    match kind {
        1 => crate::i18n::t("file"),
        2 => crate::i18n::t("module"),
        3 => crate::i18n::t("namespace"),
        4 => crate::i18n::t("package"),
        5 => crate::i18n::t("class"),
        6 => crate::i18n::t("method"),
        7 => crate::i18n::t("property"),
        8 => crate::i18n::t("field"),
        9 => crate::i18n::t("constructor"),
        10 => crate::i18n::t("enum"),
        11 => crate::i18n::t("interface"),
        12 => crate::i18n::t("function"),
        13 => crate::i18n::t("variable"),
        14 => crate::i18n::t("constant"),
        15 => crate::i18n::t("string"),
        22 => crate::i18n::t("enum member"),
        23 => crate::i18n::t("struct"),
        24 => crate::i18n::t("event"),
        25 => crate::i18n::t("operator"),
        26 => crate::i18n::t("type parameter"),
        _ => crate::i18n::t("symbol"),
    }
}

/// DocumentSymbol[] (nested) or SymbolInformation[] → flat list in document order.
pub fn parse_symbols(v: &Value) -> Vec<Symbol> {
    fn walk(items: &[Value], depth: usize, out: &mut Vec<Symbol>) {
        for it in items {
            let range = if it.get("selectionRange").is_some() { &it["selectionRange"] } else { &it["location"]["range"] };
            let (line, col) = parse_range(range).0;
            out.push(Symbol {
                name: it["name"].as_str().unwrap_or_default().to_string(),
                detail: it["detail"].as_str().or(it["containerName"].as_str()).unwrap_or_default().to_string(),
                kind: it["kind"].as_u64().unwrap_or(0) as u8,
                line,
                col,
                depth,
            });
            if let Some(children) = it["children"].as_array() {
                walk(children, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(a) = v.as_array() {
        walk(a, 0, &mut out);
    }
    out.sort_by_key(|s| (s.line, s.col));
    out
}

/// A symbol found in the project (`workspace/symbol`).
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceSymbol {
    pub name: String,
    pub container: String,
    pub kind: u8,
    pub path: String,
    pub line: u32,
    pub col: u32,
}

/// SymbolInformation[] or WorkspaceSymbol[] (the location may be just a `uri`:
/// then the start of the file). The server's order is kept.
pub fn parse_workspace_symbols(v: &Value) -> Vec<WorkspaceSymbol> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|it| {
            let loc = &it["location"];
            let path = uri_to_path(loc["uri"].as_str()?)?;
            let (line, col) = if loc.get("range").is_some() { parse_range(&loc["range"]).0 } else { (0, 0) };
            Some(WorkspaceSymbol {
                name: it["name"].as_str()?.to_string(),
                container: it["containerName"].as_str().unwrap_or_default().to_string(),
                kind: it["kind"].as_u64().unwrap_or(0) as u8,
                path,
                line,
                col,
            })
        })
        .collect()
}

/// Code action: the edit may come ready-made or `codeAction/resolve` may be needed.
#[derive(Debug, Clone)]
pub struct CodeAction {
    pub title: String,
    pub kind: String,
    pub preferred: bool,
    pub raw: Value,
}

pub fn parse_code_actions(v: &Value) -> Vec<CodeAction> {
    let mut out: Vec<CodeAction> = v
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|it| it["disabled"].is_null())
                .map(|it| CodeAction {
                    title: it["title"].as_str().unwrap_or_default().to_string(),
                    kind: it["kind"].as_str().unwrap_or_default().to_string(),
                    preferred: it["isPreferred"].as_bool().unwrap_or(false),
                    raw: it.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    // Preferred and quick fixes first.
    out.sort_by_key(|a| (!a.preferred, !a.kind.starts_with("quickfix")));
    out
}

/// Signature help: (label, byte range of the active parameter within the label).
/// Hint placed at the end of a line (inlay hint): 0-based line, label, kind (1 type, 2 parameter).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayHint {
    pub line: usize,
    pub label: String,
    pub kind: u8,
}

pub fn parse_inlay_hints(v: &Value) -> Vec<InlayHint> {
    let Some(a) = v.as_array() else { return Vec::new() };
    let mut out: Vec<InlayHint> = a
        .iter()
        .filter_map(|h| {
            let line = h["position"]["line"].as_u64()? as usize;
            let label = match &h["label"] {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts.iter().filter_map(|p| p["value"].as_str()).collect::<String>(),
                _ => return None,
            };
            let label = label.trim().to_string();
            (!label.is_empty()).then(|| InlayHint { line, label, kind: h["kind"].as_u64().unwrap_or(1) as u8 })
        })
        .take(20_000)
        .collect();
    out.sort_by_key(|h| h.line);
    out
}

/// `foldingRange` response: (first line, last line); single-line ranges are dropped.
pub fn parse_folding_ranges(v: &Value) -> Vec<(usize, usize)> {
    let Some(a) = v.as_array() else { return Vec::new() };
    let mut out: Vec<(usize, usize)> =
        a.iter().filter_map(|r| Some((r["startLine"].as_u64()? as usize, r["endLine"].as_u64()? as usize))).filter(|(s, e)| e > s).collect();
    out.sort_unstable();
    out
}

/// `documentLink` response: (range, target URI). Links without a target are dropped.
pub fn parse_document_links(v: &Value) -> Vec<(Range16, String)> {
    let Some(a) = v.as_array() else { return Vec::new() };
    a.iter().filter_map(|l| Some((parse_range(&l["range"]), l["target"].as_str()?.to_string()))).take(20_000).collect()
}

/// `prepareRename` response: `None` cannot be renamed; `Some(None)` word at the position;
/// `Some(Some(range))` the range the server indicated.
pub fn parse_prepare_rename(v: &Value) -> Option<Option<Range16>> {
    if v.is_null() {
        return None;
    }
    if v["defaultBehavior"].as_bool() == Some(true) {
        return Some(None);
    }
    if v.get("range").is_some() {
        return Some(Some(parse_range(&v["range"])));
    }
    if v.get("start").is_some() {
        return Some(Some(parse_range(v)));
    }
    Some(None)
}

/// Rewrite the URIs under `old` (file or folder) in a `WorkspaceEdit` to be under `new`
/// (for `willRenameFiles` edits applied after a move).
pub fn remap_edit_uris(edit: &Value, old: &str, new: &str) -> Value {
    let (old_uri, new_uri) = (path_to_uri(old), path_to_uri(new));
    let map = |uri: &str| -> String {
        if uri == old_uri {
            new_uri.clone()
        } else if let Some(rest) = uri.strip_prefix(&format!("{}/", old_uri.trim_end_matches('/'))) {
            format!("{}/{rest}", new_uri.trim_end_matches('/'))
        } else {
            uri.to_string()
        }
    };
    let mut out = edit.clone();
    if let Some(changes) = edit["changes"].as_object() {
        let mapped: serde_json::Map<String, Value> = changes.iter().map(|(k, v)| (map(k), v.clone())).collect();
        out["changes"] = Value::Object(mapped);
    }
    if let Some(docs) = out["documentChanges"].as_array_mut() {
        for d in docs {
            if let Some(u) = d["textDocument"]["uri"].as_str().map(map) {
                d["textDocument"]["uri"] = Value::String(u);
            }
        }
    }
    out
}

/// Call / type hierarchy response → (file, (line, column)). `key`: the item inside the entry
/// (`from`, `to`); `None`: the entry itself is the item. Position is the start of the item's `selectionRange`.
pub fn parse_hierarchy(v: &Value, key: Option<&str>) -> Vec<(String, (u32, u32))> {
    let Some(a) = v.as_array() else { return Vec::new() };
    a.iter()
        .filter_map(|e| {
            let item = key.map_or(e, |k| &e[k]);
            let path = uri_to_path(item["uri"].as_str()?)?;
            let start = if item["selectionRange"].is_object() { &item["selectionRange"]["start"] } else { &item["range"]["start"] };
            Some((path, (start["line"].as_u64()? as u32, start["character"].as_u64().unwrap_or(0) as u32)))
        })
        .collect()
}

/// Code lens: line, title and the command to run (the `command` object).
#[derive(Debug, Clone, PartialEq)]
pub struct Lens {
    pub line: usize,
    pub title: String,
    pub command: Value,
}

/// `codeLens` response. Items without a command (unresolved) go to the `unresolved` list in raw form.
pub fn parse_code_lenses(v: &Value) -> (Vec<Lens>, Vec<Value>) {
    let (mut lenses, mut unresolved) = (Vec::new(), Vec::new());
    for item in v.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(line) = item["range"]["start"]["line"].as_u64().map(|l| l as usize) else { continue };
        match item["command"]["title"].as_str() {
            Some(t) if !t.is_empty() => lenses.push(Lens { line, title: t.to_string(), command: item["command"].clone() }),
            _ => unresolved.push(item.clone()),
        }
    }
    (lenses, unresolved)
}

/// Semantic token: line, start column (UTF-16), length (UTF-16), type name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemToken {
    pub line: usize,
    pub start: u32,
    pub len: u32,
    pub kind: String,
}

/// `semanticTokens/full` data (relative quintuples) → tokens. `legend`: the server's type table.
pub fn decode_semantic_tokens(data: &Value, legend: &[String]) -> Vec<SemToken> {
    const MAX_TOKENS: usize = 200_000;
    let Some(a) = data.as_array() else { return Vec::new() };
    // Malformed data (a non-number item, a length not a multiple of five) would shift all tokens: drop it.
    let Some(nums) = a.iter().map(Value::as_u64).collect::<Option<Vec<u64>>>() else { return Vec::new() };
    if nums.len() % 5 != 0 {
        return Vec::new();
    }
    let to_u32 = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    let (mut line, mut col) = (0usize, 0u32);
    let mut out = Vec::new();
    for t in nums.as_chunks::<5>().0.iter().take(MAX_TOKENS) {
        let dl = usize::try_from(t[0]).unwrap_or(usize::MAX);
        if dl > 0 {
            line = line.saturating_add(dl);
            col = to_u32(t[1]);
        } else {
            col = col.saturating_add(to_u32(t[1]));
        }
        if let Some(k) = usize::try_from(t[3]).ok().and_then(|i| legend.get(i)).filter(|k| !k.is_empty()) {
            out.push(SemToken { line, start: col, len: to_u32(t[2]), kind: k.clone() });
        }
    }
    out
}

pub fn parse_signature(v: &Value) -> Option<(String, Option<std::ops::Range<usize>>)> {
    let sigs = v["signatures"].as_array()?;
    let active = v["activeSignature"].as_u64().unwrap_or(0) as usize;
    let sig = sigs.get(active).or(sigs.first())?;
    let label = sig["label"].as_str()?.to_string();
    let param_ix = sig["activeParameter"].as_u64().or(v["activeParameter"].as_u64()).unwrap_or(0) as usize;
    let range = sig["parameters"].as_array().and_then(|ps| ps.get(param_ix)).and_then(|p| match &p["label"] {
        Value::String(s) => label.find(s.as_str()).map(|i| i..i + s.len()),
        Value::Array(a) if a.len() == 2 => {
            // UTF-16 boundaries → bytes.
            let (a16, b16) = (a[0].as_u64()? as usize, a[1].as_u64()? as usize);
            let to_byte = |u: usize| {
                let mut units = 0;
                label
                    .char_indices()
                    .find(|(_, c)| {
                        let hit = units >= u;
                        units += c.len_utf16();
                        hit
                    })
                    .map_or(label.len(), |(i, _)| i)
            };
            Some(to_byte(a16)..to_byte(b16))
        }
        _ => None,
    });
    Some((label, range))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_symbols_with_and_without_range() {
        let v = json!([
            { "name": "Buffer", "kind": 23, "containerName": "buffer", "location": { "uri": "file:///p/a.rs", "range": { "start": { "line": 4, "character": 11 }, "end": { "line": 4, "character": 17 } } } },
            { "name": "main", "kind": 12, "location": { "uri": "file:///p/b%20c.rs" } },
            { "kind": 12, "location": { "uri": "file:///p/x.rs" } }
        ]);
        let s = parse_workspace_symbols(&v);
        assert_eq!(s.len(), 2, "unnamed is skipped");
        assert_eq!((s[0].path.as_str(), s[0].line, s[0].col, s[0].container.as_str()), ("/p/a.rs", 4, 11, "buffer"));
        assert_eq!((s[1].path.as_str(), s[1].line), ("/p/b c.rs", 0));
    }

    #[test]
    fn frames_split_across_chunks() {
        let a = encode(&json!({"id": 1, "result": "ğ"}));
        let b = encode(&json!({"method": "x"}));
        let mut all = a.clone();
        all.extend_from_slice(&b);
        let mut d = Decoder::default();
        let (p1, p2) = all.split_at(7);
        assert!(d.push(p1).is_empty());
        let msgs = d.push(p2);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["result"], "ğ", "Content-Length counts bytes");
    }

    #[test]
    fn windows_file_uris_round_trip_drive_and_unc_paths() {
        let path = "C:/şablon boşluk/src/main.rs";
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///C:/%C5%9Fablon%20bo%C5%9Fluk/src/main.rs");
        assert_eq!(path_to_uri(r"C:\şablon boşluk\src\main.rs"), uri);
        assert_eq!(uri_to_path(&uri).as_deref(), Some(path));
        assert_eq!(uri_to_path("file:///C%3A/work/main.rs").as_deref(), Some("C:/work/main.rs"));
        assert_eq!(path_to_uri(r"\\server\share\a b.rs"), "file://server/share/a%20b.rs");
        assert_eq!(uri_to_path("file://server/share/a%20b.rs").as_deref(), Some("//server/share/a b.rs"));
        assert_eq!(uri_to_path("file://localhost/home/ada/main.rs").as_deref(), Some("/home/ada/main.rs"));
    }

    #[test]
    fn a_hostile_content_length_is_dropped_instead_of_buffered_or_overflowing() {
        let mut d = Decoder::default();
        // usize::MAX would overflow `start + len`; 1 GB would be buffered forever.
        assert!(d.push(format!("Content-Length: {}\r\n\r\n{{}}", usize::MAX).as_bytes()).is_empty());
        assert!(d.push(b"Content-Length: 1073741824\r\n\r\nabc").is_empty());
        // The decoder recovers: the next well-formed message is understood.
        let ok = encode(&json!({"id": 9}));
        let msgs = d.push(&ok);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["id"], 9);
        // Endless output without a header terminator does not grow without bound.
        let mut d = Decoder::default();
        for _ in 0..6 {
            d.push(&vec![b'x'; 12 * 1024 * 1024]);
        }
        assert!(d.buf.len() <= MAX_MESSAGE + 12 * 1024 * 1024, "{}", d.buf.len());
    }

    #[test]
    fn uris_round_trip() {
        let p = "/home/kullanıcı/my proje/a#b.rs";
        let u = path_to_uri(p);
        assert!(u.starts_with("file:///home/kullan%C4%B1c%C4%B1/my%20proje/a%23b.rs"), "{u}");
        assert_eq!(uri_to_path(&u).as_deref(), Some(p));
        assert_eq!(uri_to_path("file:///a/b%20c").as_deref(), Some("/a/b c"));
    }

    #[test]
    fn snippets_expand_with_cursor() {
        assert_eq!(expand_snippet("println!(\"$1\")$0"), ("println!(\"\")".into(), Some(10)));
        assert_eq!(expand_snippet("fn ${1:name}(${2:args}) {\n\t$0\n}"), ("fn name(args) {\n\t\n}".into(), Some(3)));
        assert_eq!(expand_snippet("plain"), ("plain".into(), None));
        assert_eq!(expand_snippet("${1|a,b|}x"), ("ax".into(), Some(0)));
        assert_eq!(expand_snippet("a\\$b"), ("a$b".into(), None));
        let (text, stops) = parse_snippet("topla(${1:int a}, ${2:int b})$0");
        assert_eq!(text, "topla(int a, int b)");
        assert_eq!(stops, vec![(1, 6..11), (2, 13..18), (0, 19..19)]);
        let (text, stops) = parse_snippet("${1:outer ${2:inner}}");
        assert_eq!((text.as_str(), stops), ("outer inner", vec![(1, 0..11), (2, 6..11)]));
    }

    #[test]
    fn word_suggestions_skip_the_typed_word_and_prefer_nearby() {
        let text = "uzakkelime\nmerhaba dünya\nmer| yakınkelime merhaba 42 a\n";
        let cursor = text.find('|').unwrap();
        let text = text.replace('|', "");
        let words: Vec<String> = buffer_words(&text, cursor, 100).into_iter().map(|w| w.label).collect();
        assert!(!words.contains(&"mer".to_string()), "the word being typed is not suggested: {words:?}");
        assert!(!words.contains(&"42".to_string()) && !words.contains(&"a".to_string()), "no numbers or single letters");
        assert_eq!(words.iter().filter(|w| *w == "merhaba").count(), 1, "no duplicates");
        let pos = |w: &str| words.iter().position(|x| x == w).unwrap();
        assert!(pos("yakınkelime") < pos("uzakkelime"), "nearby first: {words:?}");
        assert!(words.contains(&"dünya".to_string()), "Turkish letters are word characters");
        assert_eq!(buffer_words(&"kelime ".repeat(10), 0, 5).len(), 1);
        assert!(buffer_words("", 0, 10).is_empty());
    }

    #[test]
    fn parses_completion_and_locations() {
        let v = json!({ "isIncomplete": true, "items": [
            { "label": "push", "kind": 2, "detail": "fn(&mut self, T)", "insertTextFormat": 2,
              "textEdit": { "range": { "start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 6} }, "newText": "push(${1:value})" } },
            { "label": "len" }
        ]});
        let (items, incomplete) = parse_completions(&v);
        assert!(incomplete);
        assert_eq!(items[0].insert, "push(value)");
        assert_eq!(items[0].cursor, Some(5));
        assert_eq!(items[0].range, Some(((1, 4), (1, 6))));
        assert_eq!(items[1].insert, "len");
        assert_eq!(items[1].doc, None);
        let (docs, _) = parse_completions(&json!([
            { "label": "a", "documentation": "düz metin" },
            { "label": "b", "documentation": { "kind": "markdown", "value": "```rust\nfn b()\n```\n**Kalın** açıklama" } },
            { "label": "c", "documentation": "  " },
        ]));
        assert_eq!(docs.iter().map(|d| d.doc.as_deref()).collect::<Vec<_>>(), [Some("düz metin"), Some("fn b()\nKalın açıklama"), None]);
        let locs = parse_locations(
            &json!([{ "targetUri": "file:///x/a.rs", "targetRange": {}, "targetSelectionRange": { "start": {"line": 3, "character": 7}, "end": {"line": 3, "character": 9} } }]),
        );
        assert_eq!(locs, vec![("/x/a.rs".to_string(), (3, 7))]);
        assert_eq!(hover_text(&json!({ "contents": { "kind": "markdown", "value": "```rust\nfn main()\n```\nDocs" } })).as_deref(), Some("fn main()\nDocs"));
        let md = "### function `topla`\n\n---\n\n→ `int`\n**Parameters**:\n- `int a`";
        assert_eq!(hover_text(&json!({ "contents": { "kind": "markdown", "value": md } })).as_deref(), Some("function topla\n\n→ int\nParameters:\n- int a"));
    }

    #[test]
    fn parses_edits_symbols_actions_signature() {
        let we = json!({ "documentChanges": [
            { "textDocument": { "uri": "file:///p/a.rs", "version": 3 }, "edits": [
                { "range": { "start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6} }, "newText": "yeni" } ] },
            { "kind": "rename", "oldUri": "file:///p/b.rs", "newUri": "file:///p/c.rs" }
        ], "changes": { "file:///p/d.rs": [ { "range": { "start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0} }, "newText": "x" } ] } });
        let (files, skipped) = parse_workspace_edit(&we);
        assert_eq!(skipped, 1);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|(p, e)| p == "/p/a.rs" && e[0] == (((0, 3), (0, 6)), "yeni".to_string())));

        let syms = parse_symbols(&json!([{ "name": "Nokta", "kind": 23, "range": {}, "selectionRange": { "start": {"line": 4, "character": 7}, "end": {} },
            "children": [{ "name": "x", "kind": 8, "detail": "i32", "range": {}, "selectionRange": { "start": {"line": 5, "character": 4}, "end": {} } }] }]));
        assert_eq!(syms.iter().map(|s| (s.name.as_str(), s.depth, s.line)).collect::<Vec<_>>(), vec![("Nokta", 0, 4), ("x", 1, 5)]);
        assert_eq!(symbol_kind(23), "struct");

        let acts =
            parse_code_actions(&json!([{ "title": "Yeniden düzenle", "kind": "refactor" }, { "title": "Düzelt", "kind": "quickfix", "isPreferred": true }]));
        assert_eq!(acts[0].title, "Düzelt", "preferred first");

        let sig = parse_signature(&json!({ "signatures": [{ "label": "topla(a: i32, b: i32) -> i32", "parameters": [{ "label": [6, 12] }, { "label": "b: i32" }], "activeParameter": 1 }] })).unwrap();
        assert_eq!(&sig.0[sig.1.clone().unwrap()], "b: i32");
    }

    fn diag(path: &str, n: usize) -> ServerEvent {
        ServerEvent::Diagnostics {
            path: path.into(),
            diagnostics: vec![Diagnostic { start: (0, 0), end: (0, 1), severity: 1, message: "m".into(), source: None }; n],
        }
    }

    /// A UI that stalls keeps the latest diagnostics per file and the latest progress line; every other event stays, in order.
    #[test]
    fn a_stalled_ui_holds_one_diagnostics_list_per_file_and_never_loses_other_events() {
        let mut q = std::collections::VecDeque::new();
        merge_server_event(&mut q, ServerEvent::Question { id: json!(1), message: "reload?".into(), actions: vec!["Yes".into()] });
        for i in 0..50_000 {
            merge_server_event(&mut q, diag(if i % 2 == 0 { "/a.rs" } else { "/b.rs" }, i % 7));
            merge_server_event(&mut q, ServerEvent::Progress(Some(format!("indexing {i}"))));
            if i % 10_000 == 0 {
                merge_server_event(&mut q, ServerEvent::Message(format!("warning {i}")));
            }
        }
        merge_server_event(&mut q, ServerEvent::Progress(None));
        merge_server_event(&mut q, ServerEvent::Exited(Some(1)));
        // 1 question + 2 files + 1 progress + 5 messages + exit; nothing but diagnostics and progress was folded.
        assert_eq!(q.len(), 10, "{} events queued", q.len());
        assert!(matches!(q.front(), Some(ServerEvent::Question { .. })), "ordering of unfoldable events is kept");
        assert!(matches!(q.back(), Some(ServerEvent::Exited(Some(1)))));
        let messages = q.iter().filter(|e| matches!(e, ServerEvent::Message(_))).count();
        assert_eq!(messages, 5, "messages are never merged");
        let progress: Vec<_> = q.iter().filter_map(|e| if let ServerEvent::Progress(p) = e { Some(p.clone()) } else { None }).collect();
        assert_eq!(progress, vec![None], "the final progress state wins");
        fn latest(q: &std::collections::VecDeque<ServerEvent>, path: &str) -> Option<usize> {
            q.iter().find_map(|e| match e {
                ServerEvent::Diagnostics { path: p, diagnostics } if p == path => Some(diagnostics.len()),
                _ => None,
            })
        }
        assert_eq!(latest(&q, "/a.rs"), Some(49_998 % 7));
        assert_eq!(latest(&q, "/b.rs"), Some(49_999 % 7));
        // A later empty list clears the file (the UI removes the entry), so it must replace, not be skipped.
        merge_server_event(&mut q, diag("/a.rs", 0));
        assert_eq!(latest(&q, "/a.rs"), Some(0));
    }

    #[test]
    fn server_overrides() {
        assert_eq!(server_for("rust", &[]).unwrap().command, "rust-analyzer");
        let o = vec![("python".to_string(), Some(("pylsp".to_string(), vec![]))), ("go".to_string(), None)];
        assert_eq!(server_for("python", &o).unwrap().command, "pylsp");
        assert!(server_for("go", &o).is_none(), "null disables");
        assert_eq!(language_of("/a/b.tsx"), Some(("typescriptreact", "typescript")));
    }

    /// Every default server is reachable from at least one file name, carries an install hint and runs without a shell.
    #[test]
    fn every_default_server_has_files_a_hint_and_a_plain_command() {
        let samples: &[(&str, &str, &str)] = &[
            ("a.rs", "rust", "rust"),
            ("a.c", "c", "c"),
            ("A.java", "java", "java"),
            ("A.cs", "csharp", "csharp"),
            ("a.php", "php", "php"),
            ("a.rb", "ruby", "ruby"),
            ("Gemfile", "ruby", "ruby"),
            ("a.kt", "kotlin", "kotlin"),
            ("build.gradle.kts", "kotlin", "kotlin"),
            ("a.swift", "swift", "swift"),
            ("a.lua", "lua", "lua"),
            ("a.sh", "shellscript", "bash"),
            ("a.html", "html", "html"),
            ("a.css", "css", "css"),
            ("a.scss", "scss", "css"),
            ("a.less", "less", "css"),
        ];
        for (file, id, key) in samples {
            assert_eq!(language_of(&format!("/p/{file}")), Some((*id, *key)), "{file}");
        }
        let mut keys = std::collections::HashSet::new();
        for (key, command, args, hint) in DEFAULT_SERVERS {
            assert!(keys.insert(*key), "duplicate default for {key}");
            let cfg = server_for(key, &[]).unwrap_or_else(|| panic!("no default for {key}"));
            assert_eq!((cfg.command.as_str(), cfg.args.len()), (*command, args.len()));
            assert_eq!(cfg.language, *key);
            assert_eq!(install_hint(key), Some(*hint));
            assert!(!command.contains(' ') && !command.contains('/'), "{key}: the command is looked up on PATH, no shell syntax");
            assert!(hint.len() > 8);
        }
        // Every key a file can map to is either a default or one of the documented opt-in keys.
        for ext in [
            "rs", "c", "cpp", "go", "py", "ts", "tsx", "js", "jsx", "java", "cs", "php", "rb", "kt", "swift", "lua", "sh", "html", "css", "json", "yaml",
            "toml",
        ] {
            let (_, key) = language_of(&format!("/p/x.{ext}")).unwrap();
            assert!(default_server(key).is_some() || ["json", "yaml", "toml"].contains(&key), "{ext} -> {key}");
        }
    }

    /// JSON, YAML and TOML are checked by the built-in schema engine (no server, no duplicate diagnostics, no downloads
    /// outside `schema.download`): they have no default server, but `lsp.servers` can still switch one on.
    #[test]
    fn json_yaml_and_toml_servers_are_opt_in() {
        for (file, key) in [("a.json", "json"), ("a.jsonc", "json"), ("a.yaml", "yaml"), ("a.yml", "yaml"), ("Cargo.toml", "toml")] {
            let (_, k) = language_of(file).unwrap();
            assert_eq!(k, key);
            assert!(server_for(k, &[]).is_none(), "{key} must have no default server");
            assert!(install_hint(k).is_none());
        }
        let o = vec![("toml".to_string(), Some(("taplo".to_string(), vec!["lsp".to_string(), "stdio".to_string()])))];
        let cfg = server_for("toml", &o).unwrap();
        assert_eq!((cfg.command.as_str(), cfg.args.as_slice()), ("taplo", &["lsp".to_string(), "stdio".to_string()][..]));
        // Defaults can be turned off, and a later entry wins over an earlier one.
        let off = vec![("java".to_string(), None)];
        assert!(server_for("java", &off).is_none());
        let twice = vec![("php".to_string(), None), ("php".to_string(), Some(("phpactor".to_string(), vec!["language-server".to_string()])))];
        assert_eq!(server_for("php", &twice).unwrap().command, "phpactor");
    }

    /// Real language server (if clangd is available): handshake, diagnostics, hover, definition.
    #[test]
    fn real_clangd_round_trip() {
        if std::process::Command::new("clangd").arg("--version").output().is_err() {
            eprintln!("clangd not found, skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("codeyard-lsp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap(); // servers return the canonical path (the app opens it that way too)
        let src = "int topla(int a, int b) { return a + b; }\nint main(void) { return topla(1, yok); }\n";
        let file = dir.join("main.c");
        std::fs::write(&file, src).unwrap();
        let (root, path) = (dir.to_string_lossy().into_owned(), file.to_string_lossy().into_owned());
        let host: Arc<dyn Host> = Arc::new(host::local());
        let (tx, rx) = mpsc::channel();
        let cfg = server_for("c", &[]).unwrap();
        let client = Client::start(host, cfg, &root, tx).expect("clangd should start");
        assert!(client.is_alive());
        client.notify("textDocument/didOpen", json!({ "textDocument": { "uri": path_to_uri(&path), "languageId": "c", "version": 1, "text": src } }));
        let diags = loop {
            match rx.recv_timeout(Duration::from_secs(20)).expect("a diagnostic should arrive") {
                ServerEvent::Diagnostics { path: p, diagnostics } if p == path && !diagnostics.is_empty() => break diagnostics,
                _ => {}
            }
        };
        assert_eq!(diags[0].severity, 1);
        assert!(diags[0].message.contains("yok"), "{:?}", diags[0]);
        assert_eq!(diags[0].start.0, 1, "on the second line");

        let at = json!({ "textDocument": { "uri": path_to_uri(&path) }, "position": { "line": 1, "character": 26 } });
        let hover = client.request("textDocument/hover", at.clone(), Duration::from_secs(10)).unwrap();
        assert!(hover_text(&hover).is_some_and(|t| t.contains("topla")), "{hover}");
        let def = client.request("textDocument/definition", at, Duration::from_secs(10)).unwrap();
        assert_eq!(parse_locations(&def), vec![(path.clone(), (0, 4))]);
        client.shutdown();
    }

    #[test]
    fn hierarchy_results_become_locations() {
        let v = serde_json::json!([
            { "from": { "uri": "file:///a/b.rs", "selectionRange": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 9 } }, "range": { "start": { "line": 1, "character": 0 } } }, "fromRanges": [] },
            { "from": { "uri": "http://x/y" } },
            { "from": { "uri": "file:///c.rs", "range": { "start": { "line": 7, "character": 1 } } } }
        ]);
        assert_eq!(parse_hierarchy(&v, Some("from")), vec![("/a/b.rs".to_string(), (3, 4)), ("/c.rs".to_string(), (7, 1))]);
        let direct = serde_json::json!([{ "uri": "file:///t.rs", "selectionRange": { "start": { "line": 0, "character": 2 } } }]);
        assert_eq!(parse_hierarchy(&direct, None), vec![("/t.rs".to_string(), (0, 2))]);
        assert!(parse_hierarchy(&serde_json::Value::Null, None).is_empty());
    }

    #[test]
    fn code_lenses_and_semantic_tokens_decode() {
        let v = serde_json::json!([
            { "range": { "start": { "line": 4, "character": 0 } }, "command": { "title": "3 references", "command": "x.showRefs" } },
            { "range": { "start": { "line": 9, "character": 0 } }, "data": { "id": 1 } },
            { "command": { "title": "no range" } }
        ]);
        let (lenses, unresolved) = parse_code_lenses(&v);
        assert_eq!((lenses.len(), unresolved.len()), (1, 1));
        assert_eq!((lenses[0].line, lenses[0].title.as_str()), (4, "3 references"));
        // Relative encoding: (0,2,3,k0) (0,5,2,k1) (2,1,4,k0)
        let data = serde_json::json!([0, 2, 3, 0, 0, 0, 5, 2, 1, 0, 2, 1, 4, 0, 0]);
        let legend = vec!["function".to_string(), "variable".to_string()];
        let t = decode_semantic_tokens(&data, &legend);
        assert_eq!(
            t,
            vec![
                SemToken { line: 0, start: 2, len: 3, kind: "function".into() },
                SemToken { line: 0, start: 7, len: 2, kind: "variable".into() },
                SemToken { line: 2, start: 1, len: 4, kind: "function".into() },
            ]
        );
        assert!(decode_semantic_tokens(&serde_json::Value::Null, &legend).is_empty());
    }

    #[test]
    fn hostile_semantic_tokens_and_uri_remapping() {
        let legend = vec!["function".to_string()];
        let huge = serde_json::json!([0, 4294967295u64, 4294967295u64, 0, 0, 0, 4294967295u64, 1, 0, 0]);
        let t = decode_semantic_tokens(&huge, &legend);
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].start, u32::MAX, "overflow saturates, no panic");
        assert!(decode_semantic_tokens(&serde_json::json!([0, 1, 2, 0]), &legend).is_empty(), "length is not a multiple of five");
        assert!(decode_semantic_tokens(&serde_json::json!([0, "x", 2, 0, 0]), &legend).is_empty(), "non-number item");
        assert!(decode_semantic_tokens(&serde_json::json!([0, 1, 2, 9, 0]), &legend).is_empty(), "type not in the legend");
        let edit = serde_json::json!({
            "changes": { "file:///a/old/x.ts": [], "file:///a/other.ts": [] },
            "documentChanges": [{ "textDocument": { "uri": "file:///a/old.ts", "version": 1 }, "edits": [] }]
        });
        let moved = remap_edit_uris(&edit, "/a/old", "/a/new");
        assert!(moved["changes"].get("file:///a/new/x.ts").is_some() && moved["changes"].get("file:///a/other.ts").is_some());
        let moved_file = remap_edit_uris(&edit, "/a/old.ts", "/a/new.ts");
        assert_eq!(moved_file["documentChanges"][0]["textDocument"]["uri"], "file:///a/new.ts");
    }
}
