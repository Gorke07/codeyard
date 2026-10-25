//! Walking a schema: which subschemas apply under a path (`engines` -> `node`)? Hover text and completion
//! candidates come from here. `$ref` (local and remote), `allOf/anyOf/oneOf` and `if/then/else` are treated as a union.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;

use crate::validate::SchemaSet;

/// Path segment: an object key or an array index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Index(usize),
}

/// A subschema and the document it was resolved in (the base for relative `$ref`s).
#[derive(Clone)]
pub struct Sub {
    doc: Arc<Value>,
    base: String,
    pub node: Value,
}

const MAX_DEPTH: usize = 10;
const MAX_SUBS: usize = 64;

fn join_url(base: &str, reference: &str) -> String {
    if reference.contains("://") {
        return reference.to_string();
    }
    if reference.starts_with('/') {
        // Server/root of the base schema.
        if let Some(i) = base.find("://") {
            let host_end = base[i + 3..].find('/').map_or(base.len(), |n| i + 3 + n);
            return format!("{}{reference}", &base[..host_end]);
        }
        return reference.to_string();
    }
    match base.rfind('/') {
        Some(i) => format!("{}/{reference}", &base[..i]),
        None => reference.to_string(),
    }
}

fn pointer_get<'a>(doc: &'a Value, frag: &str) -> Option<&'a Value> {
    if frag.is_empty() || frag == "/" {
        return Some(doc);
    }
    let decoded: String = {
        let b = frag.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%'
                && i + 2 < b.len()
                && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16)
            {
                out.push(v);
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).ok()?
    };
    doc.pointer(&decoded)
}

impl SchemaSet {
    fn root_sub(&self) -> Sub {
        Sub { doc: self.root.clone(), base: self.base.clone(), node: (*self.root).clone() }
    }

    /// Follow `$ref` and flatten the `allOf/anyOf/oneOf/if/then/else` branches (including itself).
    fn expand(&self, sub: Sub, depth: usize, seen: &mut HashSet<String>, out: &mut Vec<Sub>) {
        if depth > MAX_DEPTH || out.len() >= MAX_SUBS {
            return;
        }
        let Value::Object(map) = &sub.node else { return };
        if let Some(Value::String(r)) = map.get("$ref") {
            let (url, frag) = r.split_once('#').unwrap_or((r.as_str(), ""));
            let (doc, base) = if url.is_empty() {
                (sub.doc.clone(), sub.base.clone())
            } else {
                let full = join_url(&sub.base, url);
                match self.fetch_json(&full) {
                    Ok(d) => (d, full),
                    Err(_) => return,
                }
            };
            let key = format!("{base}#{frag}");
            if !seen.insert(key) {
                return;
            }
            if let Some(target) = pointer_get(&doc, frag) {
                self.expand(Sub { doc: doc.clone(), base, node: target.clone() }, depth + 1, seen, out);
            }
            // Old drafts ignore `$ref` siblings; newer ones (2019+) allow keywords beside it.
        }
        out.push(sub.clone());
        for key in ["allOf", "anyOf", "oneOf"] {
            if let Some(Value::Array(branches)) = map.get(key) {
                for b in branches {
                    self.expand(Sub { doc: sub.doc.clone(), base: sub.base.clone(), node: b.clone() }, depth + 1, seen, out);
                }
            }
        }
        for key in ["if", "then", "else"] {
            if let Some(b @ Value::Object(_)) = map.get(key) {
                self.expand(Sub { doc: sub.doc.clone(), base: sub.base.clone(), node: b.clone() }, depth + 1, seen, out);
            }
        }
    }

    fn expanded(&self, sub: Sub) -> Vec<Sub> {
        let mut out = Vec::new();
        self.expand(sub, 0, &mut HashSet::new(), &mut out);
        out
    }

    fn child(&self, parent: &Sub, node: &Value) -> Vec<Sub> {
        self.expanded(Sub { doc: parent.doc.clone(), base: parent.base.clone(), node: node.clone() })
    }

    /// Subschemas that apply under `path`.
    pub fn at(&self, path: &[Seg]) -> Vec<Sub> {
        let mut cur = self.expanded(self.root_sub());
        for seg in path {
            let mut next = Vec::new();
            for sub in &cur {
                let Value::Object(map) = &sub.node else { continue };
                match seg {
                    Seg::Key(k) => {
                        if let Some(p) = map.get("properties").and_then(|p| p.get(k)) {
                            next.extend(self.child(sub, p));
                        } else if let Some(Value::Object(pp)) = map.get("patternProperties") {
                            // Simple patterns (`^x-`, `-suffix$`, exact name) are tried by hand; complex regexes are ignored.
                            for (pat, s) in pp {
                                if simple_pattern_matches(pat, k) {
                                    next.extend(self.child(sub, s));
                                }
                            }
                        }
                        if next.is_empty()
                            && let Some(ap @ Value::Object(_)) = map.get("additionalProperties")
                        {
                            next.extend(self.child(sub, ap));
                        }
                    }
                    Seg::Index(i) => match (map.get("items"), map.get("prefixItems")) {
                        (_, Some(Value::Array(pi))) if *i < pi.len() => next.extend(self.child(sub, &pi[*i])),
                        (Some(items @ Value::Object(_)), _) => next.extend(self.child(sub, items)),
                        (Some(Value::Array(items)), _) if *i < items.len() => next.extend(self.child(sub, &items[*i])),
                        _ => {
                            if let Some(extra @ Value::Object(_)) = map.get("additionalItems") {
                                next.extend(self.child(sub, extra));
                            }
                        }
                    },
                }
            }
            next.truncate(MAX_SUBS);
            cur = next;
            if cur.is_empty() {
                break;
            }
        }
        cur
    }

    /// Hover text: title, description, type, allowed values, default. `None` if there's nothing to describe.
    pub fn hover(&self, path: &[Seg]) -> Option<String> {
        let subs = self.at(path);
        let text_of = |s: &Sub, k: &str| s.node.get(k).and_then(Value::as_str).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let description = subs.iter().find_map(|s| text_of(s, "markdownDescription").or_else(|| text_of(s, "description")));
        let title = subs.iter().find_map(|s| text_of(s, "title"));
        let types = type_names(&subs);
        let values = value_candidates(&subs)
            .into_iter()
            .filter(|c| c.label != "true" && c.label != "false" || types.iter().all(|t| t != "boolean"))
            .map(|c| c.label)
            .take(12)
            .collect::<Vec<_>>();
        let default = subs.iter().find_map(|s| s.node.get("default")).map(|d| d.to_string());
        if description.is_none() && title.is_none() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = &title
            && description.as_ref() != Some(t)
        {
            parts.push(t.clone());
        }
        if let Some(d) = description {
            parts.push(d);
        }
        let mut meta = Vec::new();
        if !types.is_empty() {
            meta.push(format!("Type: {}", types.join(" | ")));
        }
        if !values.is_empty() {
            meta.push(format!("Values: {}", values.join(", ")));
        }
        if let Some(d) = default {
            meta.push(format!("Default: {d}"));
        }
        if !meta.is_empty() {
            parts.push(meta.join("\n"));
        }
        Some(parts.join("\n\n"))
    }

    /// Properties that can be added to an object: (name, value schemas, required); excluding those already written.
    pub fn property_candidates(&self, path: &[Seg], existing: &HashSet<String>) -> Vec<Property> {
        let subs = self.at(path);
        let mut required: HashSet<&str> = HashSet::new();
        for s in &subs {
            if let Some(Value::Array(r)) = s.node.get("required") {
                required.extend(r.iter().filter_map(Value::as_str));
            }
        }
        let mut out: Vec<Property> = Vec::new();
        for s in &subs {
            let Some(Value::Object(props)) = s.node.get("properties") else { continue };
            for (name, schema) in props {
                if existing.contains(name) || out.iter().any(|p| p.name == *name) {
                    continue;
                }
                let value_subs = self.child(s, schema);
                let deprecated = value_subs.iter().any(|v| v.node.get("deprecated") == Some(&Value::Bool(true)));
                out.push(Property {
                    name: name.clone(),
                    detail: type_names(&value_subs).join(" | "),
                    doc: value_subs
                        .iter()
                        .find_map(|v| v.node.get("markdownDescription").or_else(|| v.node.get("description")).and_then(Value::as_str))
                        .map(|d| d.trim().to_string()),
                    required: required.contains(name.as_str()),
                    kind: Kind::of(&value_subs),
                    deprecated,
                });
            }
        }
        // Required first, then alphabetical, so the map order (the workspace may enable `preserve_order`)
        // doesn't affect the result.
        out.sort_by(|a, b| (!a.required, &a.name).cmp(&(!b.required, &b.name)));
        out
    }

    /// Candidates for a value position: `enum`, `const`, boolean, default and examples.
    pub fn value_candidates_at(&self, path: &[Seg]) -> Vec<ValueCandidate> {
        value_candidates(&self.at(path))
    }
}

fn simple_pattern_matches(pat: &str, key: &str) -> bool {
    let (anchored_start, p) = pat.strip_prefix('^').map_or((false, pat), |p| (true, p));
    let (anchored_end, p) = p.strip_suffix('$').map_or((false, p), |p| (true, p));
    if p.chars().any(|c| "\\[](){}|+?.*".contains(c)) {
        return false;
    }
    match (anchored_start, anchored_end) {
        (true, true) => key == p,
        (true, false) => key.starts_with(p),
        (false, true) => key.ends_with(p),
        (false, false) => key.contains(p),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Scalar,
    Object,
    Array,
}

impl Kind {
    fn of(subs: &[Sub]) -> Kind {
        let types = type_names(subs);
        if types.iter().any(|t| t == "object") && !types.iter().any(|t| matches!(t.as_str(), "string" | "number" | "integer" | "boolean")) {
            Kind::Object
        } else if types.iter().any(|t| t == "array") && types.len() == 1 {
            Kind::Array
        } else if types.is_empty() && subs.iter().any(|s| s.node.get("properties").is_some()) {
            Kind::Object
        } else {
            Kind::Scalar
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub name: String,
    pub detail: String,
    pub doc: Option<String>,
    pub required: bool,
    pub kind: Kind,
    pub deprecated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValueCandidate {
    pub label: String,
    pub value: Value,
    pub doc: Option<String>,
}

fn type_names(subs: &[Sub]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in subs {
        match s.node.get("type") {
            Some(Value::String(t)) => {
                if !out.contains(t) {
                    out.push(t.clone());
                }
            }
            Some(Value::Array(ts)) => {
                for t in ts.iter().filter_map(Value::as_str) {
                    if !out.iter().any(|o| o == t) {
                        out.push(t.into());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn value_candidates(subs: &[Sub]) -> Vec<ValueCandidate> {
    let mut out: Vec<ValueCandidate> = Vec::new();
    let mut push = |value: &Value, doc: Option<String>| {
        let label = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if !matches!(value, Value::Object(_) | Value::Array(_)) && !out.iter().any(|c| c.value == *value) {
            out.push(ValueCandidate { label, value: value.clone(), doc });
        }
    };
    for s in subs {
        if let Some(Value::Array(e)) = s.node.get("enum") {
            for v in e {
                push(v, None);
            }
        }
        if let Some(c) = s.node.get("const") {
            push(c, None);
        }
        if type_names(std::slice::from_ref(s)).iter().any(|t| t == "boolean") {
            push(&Value::Bool(true), None);
            push(&Value::Bool(false), None);
        }
        if let Some(d) = s.node.get("default") {
            push(d, Some("default".into()));
        }
        if let Some(Value::Array(ex)) = s.node.get("examples") {
            for v in ex.iter().take(5) {
                push(v, Some("example".into()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use serde_json::json;

    fn set() -> Arc<SchemaSet> {
        SchemaSet::from_value(
            json!({
                "definitions": {
                    "step": {"type": "object", "properties": {"run": {"type": "string", "description": "Shell command"}, "name": {"type": "string"}}},
                    "job": {"type": "object", "required": ["runs-on"], "properties": {
                        "runs-on": {"enum": ["ubuntu-latest", "macos-latest"], "description": "Runner label"},
                        "steps": {"type": "array", "items": {"$ref": "#/definitions/step"}},
                        "timeout": {"type": "integer", "default": 360, "deprecated": true}
                    }}
                },
                "type": "object",
                "properties": {
                    "name": {"type": "string", "title": "Workflow name", "description": "Shown in the UI"},
                    "jobs": {"type": "object", "additionalProperties": {"$ref": "#/definitions/job"}},
                    "on": {"oneOf": [{"type": "string"}, {"type": "object", "properties": {"push": {"type": "object"}, "pull_request": {"type": "object"}}}]},
                    "env": {"type": "object", "patternProperties": {"^X_": {"type": "string", "description": "x var"}}}
                },
                "allOf": [{"properties": {"concurrency": {"type": "string"}}}]
            }),
            "file:///w.json",
            &Store::new(None, false),
        )
    }

    fn k(s: &str) -> Seg {
        Seg::Key(s.into())
    }

    #[test]
    fn follows_refs_additional_properties_items_and_all_of() {
        let s = set();
        let names = |path: &[Seg]| s.property_candidates(path, &HashSet::new()).into_iter().map(|p| p.name).collect::<Vec<_>>();
        assert_eq!(names(&[]), ["concurrency", "env", "jobs", "name", "on"], "allOf branches contribute; order is alphabetical");
        assert_eq!(names(&[k("jobs"), k("build")]), ["runs-on", "steps", "timeout"], "required first, then alphabetical");
        assert_eq!(names(&[k("jobs"), k("build"), k("steps"), Seg::Index(3)]), ["name", "run"]);
        assert_eq!(names(&[k("on")]), ["pull_request", "push"], "oneOf branches are merged");
        assert!(names(&[k("nope")]).is_empty());
    }

    #[test]
    fn existing_keys_are_not_offered_again_and_flags_are_set() {
        let s = set();
        let existing: HashSet<String> = ["runs-on".to_string()].into();
        let p = s.property_candidates(&[k("jobs"), k("a")], &existing);
        assert_eq!(p.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["steps", "timeout"]);
        let steps = &p[0];
        assert_eq!((steps.kind, steps.detail.as_str()), (Kind::Array, "array"));
        assert!(p[1].deprecated);
        let all = s.property_candidates(&[k("jobs"), k("a")], &HashSet::new());
        assert!(all[0].required && all[0].name == "runs-on");
        assert_eq!(all[0].doc.as_deref(), Some("Runner label"));
    }

    #[test]
    fn value_candidates_come_from_enum_boolean_default() {
        let s = set();
        let labels = |path: &[Seg]| s.value_candidates_at(path).into_iter().map(|c| c.label).collect::<Vec<_>>();
        assert_eq!(labels(&[k("jobs"), k("a"), k("runs-on")]), ["ubuntu-latest", "macos-latest"]);
        assert_eq!(labels(&[k("jobs"), k("a"), k("timeout")]), ["360"]);
        let b = SchemaSet::from_value(json!({"properties": {"private": {"type": "boolean"}}}), "file:///b.json", &Store::new(None, false));
        assert_eq!(b.value_candidates_at(&[k("private")]).into_iter().map(|c| c.label).collect::<Vec<_>>(), ["true", "false"]);
    }

    #[test]
    fn hover_combines_title_description_type_values_default() {
        let s = set();
        assert_eq!(s.hover(&[k("name")]).unwrap(), "Workflow name\n\nShown in the UI\n\nType: string");
        let h = s.hover(&[k("jobs"), k("x"), k("runs-on")]).unwrap();
        assert_eq!(h, "Runner label\n\nValues: ubuntu-latest, macos-latest");
        assert!(s.hover(&[k("jobs")]).is_none(), "no description: no card");
        assert!(s.hover(&[k("env"), k("X_HOME")]).unwrap().starts_with("x var"), "simple patternProperties");
    }

    #[test]
    fn recursive_refs_do_not_loop() {
        let s = SchemaSet::from_value(
            json!({"$ref": "#/definitions/a", "definitions": {"a": {"$ref": "#/definitions/b"}, "b": {"$ref": "#/definitions/a"}}}),
            "file:///r.json",
            &Store::new(None, false),
        );
        assert!(s.at(&[k("x")]).is_empty());
        let tree = SchemaSet::from_value(
            json!({"definitions": {"n": {"type": "object", "properties": {"child": {"$ref": "#/definitions/n"}, "v": {"type": "string"}}}}, "$ref": "#/definitions/n"}),
            "file:///t.json",
            &Store::new(None, false),
        );
        let deep = [k("child"), k("child"), k("child"), k("child")];
        assert_eq!(tree.property_candidates(&deep, &HashSet::new()).len(), 2, "self-referencing schemas can be walked to any depth");
    }

    #[test]
    fn url_joining_handles_relative_absolute_and_root_paths() {
        assert_eq!(join_url("https://a.com/x/y.json", "z.json"), "https://a.com/x/z.json");
        assert_eq!(join_url("https://a.com/x/y.json", "../z.json"), "https://a.com/x/../z.json");
        assert_eq!(join_url("https://a.com/x/y.json", "/root.json"), "https://a.com/root.json");
        assert_eq!(join_url("https://a.com/x/y.json", "https://b.org/q.json"), "https://b.org/q.json");
        assert_eq!(join_url("file:///tmp/s.json", "p.json"), "file:///tmp/p.json");
    }
}
