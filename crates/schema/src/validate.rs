//! Schema loading and validation. `SchemaSet` holds the root schema, the resolution base and the validator; remote
//! `$ref`s go through `Store` (cached).

use std::sync::{Arc, OnceLock};

use jsonschema::error::ValidationErrorKind;
use jsonschema::{Retrieve, Uri, Validator};
use serde_json::Value;

use crate::doc::{self, Parsed};
use crate::store::{FileSource, Store};

/// A validation violation: which node, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub pointer: String,
    pub message: String,
}

pub struct SchemaSet {
    pub root: Arc<Value>,
    /// Base that relative `$ref`s resolve against (URL or `file://` path).
    pub base: String,
    pub(crate) store: Arc<Store>,
    /// Where local schema files are read from (remote workspace); `None`: this machine's file system.
    pub(crate) files: Option<Arc<dyn FileSource>>,
    validator: OnceLock<Result<Validator, String>>,
}

struct StoreRetriever(Arc<Store>, Option<Arc<dyn FileSource>>);

impl Retrieve for StoreRetriever {
    fn retrieve(&self, uri: &Uri<String>) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let url = uri.as_str();
        // Built-in meta-schemas come from the library; the rest from the store (cache, download) or the workspace's host.
        self.0.json_with(url, self.1.as_deref()).map(|v| (*v).clone()).map_err(|e| e.into())
    }
}

impl SchemaSet {
    /// Load from a URL or local path.
    pub fn load(source: &str, store: &Arc<Store>) -> Result<Arc<SchemaSet>, String> {
        Self::load_with(source, store, None)
    }

    pub fn load_with(source: &str, store: &Arc<Store>, files: Option<Arc<dyn FileSource>>) -> Result<Arc<SchemaSet>, String> {
        let root = store.json_with(source, files.as_deref())?;
        let base = if source.contains("://") { source.split('#').next().unwrap_or(source).to_string() } else { crate::file_uri::from_path(source) };
        Ok(Arc::new(SchemaSet { root, base, store: store.clone(), files, validator: OnceLock::new() }))
    }

    pub fn from_value(root: Value, base: &str, store: &Arc<Store>) -> Arc<SchemaSet> {
        Arc::new(SchemaSet { root: Arc::new(root), base: base.into(), store: store.clone(), files: None, validator: OnceLock::new() })
    }

    /// Schema document (remote vs local handled): for `$ref` resolution and walking.
    pub(crate) fn fetch_json(&self, url: &str) -> Result<Arc<Value>, String> {
        self.store.json_with(url, self.files.as_deref())
    }

    fn validator(&self) -> &Result<Validator, String> {
        self.validator.get_or_init(|| {
            jsonschema::options()
                .with_base_uri(self.base.clone())
                .with_retriever(StoreRetriever(self.store.clone(), self.files.clone()))
                // `format` hints (uri, date-time...) are noise in most config files; structure and types are enough.
                .should_validate_formats(false)
                .build(&self.root)
                .map_err(|e| format!("The schema could not be compiled: {e}"))
        })
    }

    /// Validate the document. `opaque` (custom-tagged) nodes and their subtrees are skipped; at most `MAX` violations.
    pub fn validate(&self, parsed: &Parsed) -> Result<Vec<Violation>, String> {
        const MAX: usize = 200;
        let v = self.validator().as_ref().map_err(|e| e.clone())?;
        let mut out: Vec<Violation> = Vec::new();
        for e in v.iter_errors(&parsed.value) {
            let pointer = e.instance_path().as_str().to_string();
            if parsed.opaque.iter().any(|o| pointer == *o || pointer.starts_with(&format!("{o}/"))) {
                continue;
            }
            let mut message = e.to_string();
            if message.chars().count() > 200 {
                message = message.chars().take(200).collect::<String>() + "…";
            }
            // Regex engine limits (catastrophic backtracking): the schema's pattern is at fault, not the document; don't report.
            if matches!(e.kind(), ValidationErrorKind::BacktrackLimitExceeded { .. } | ValidationErrorKind::RegexEngineFailure { .. }) {
                continue;
            }
            match e.kind() {
                // Unexpected properties are reported on their own keys, not on the object.
                ValidationErrorKind::AdditionalProperties { unexpected } => {
                    for name in unexpected {
                        // A root `$schema` declaration links the document to the schema; it isn't a property.
                        if name == "$schema" && pointer.is_empty() {
                            continue;
                        }
                        out.push(Violation { pointer: doc::child(&pointer, name), message: format!("Property “{name}” is not allowed here") });
                    }
                }
                _ => out.push(Violation { pointer, message }),
            }
            if out.len() >= MAX {
                break;
            }
        }
        out.sort_by(|a, b| a.pointer.cmp(&b.pointer).then_with(|| a.message.cmp(&b.message)));
        out.dedup();
        Ok(out)
    }
}

/// Byte range of the violation in the text. For containers the key, else the first line; if the node is missing, walk up to its ancestor.
pub fn range_of(parsed: &Parsed, text: &str, pointer: &str) -> (usize, usize) {
    let mut p = pointer.to_string();
    loop {
        if let Some(span) = parsed.spans.get(&p) {
            let container = matches!(parsed.value.pointer(&p), Some(Value::Object(_) | Value::Array(_)));
            if let Some(k) = &span.key
                && container
            {
                return (k.start, k.end);
            }
            if container {
                let start = span.value.start.min(text.len());
                let end = text[start..].find('\n').map_or(text.len(), |n| start + n).max(start + 1).min(text.len());
                return (start, end);
            }
            return (span.value.start, span.value.end.max(span.value.start + 1));
        }
        match p.rfind('/') {
            Some(i) => p.truncate(i),
            None => return (0, text.find('\n').unwrap_or(text.len()).max(1).min(text.len())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> Arc<Store> {
        Store::new(None, false)
    }

    fn schema() -> Arc<SchemaSet> {
        SchemaSet::from_value(
            json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "required": ["name"],
                "additionalProperties": false,
                "properties": {
                    "name": {"type": "string", "description": "Package name"},
                    "version": {"type": "string", "pattern": "^\\d+\\.\\d+\\.\\d+$"},
                    "private": {"type": "boolean"},
                    "scripts": {"type": "object", "additionalProperties": {"type": "string"}},
                    "engines": {"type": "object", "properties": {"node": {"enum": ["18", "20", "22"]}}}
                }
            }),
            "file:///tmp/schema.json",
            &store(),
        )
    }

    fn check(set: &SchemaSet, text: &str) -> Vec<(String, String)> {
        let p = doc::parse_json(text).unwrap();
        set.validate(&p).unwrap().into_iter().map(|v| (v.pointer, v.message)).collect()
    }

    #[test]
    fn reports_type_required_enum_and_unexpected_properties_at_the_right_place() {
        let set = schema();
        assert!(check(&set, r#"{"name": "a", "version": "1.2.3"}"#).is_empty());
        let errs = check(&set, r#"{"version": "x", "private": "yes", "extra": 1, "engines": {"node": "19"}, "scripts": {"a": 1}}"#);
        let at = |p: &str| errs.iter().find(|(q, _)| q == p).map(|(_, m)| m.clone());
        assert!(at("").unwrap().contains("name"), "missing required property is on the object: {errs:?}");
        assert!(at("/version").unwrap().contains("does not match"), "{errs:?}");
        assert!(at("/private").unwrap().contains("boolean"));
        assert_eq!(at("/extra").unwrap(), "Property “extra” is not allowed here", "on the property, not the object");
        assert!(at("/engines/node").is_some());
        assert!(at("/scripts/a").unwrap().contains("string"));
    }

    #[test]
    fn the_root_schema_declaration_is_never_an_unexpected_property() {
        let set = schema();
        assert!(check(&set, r#"{"$schema": "https://x/s.json", "name": "a"}"#).is_empty());
    }

    #[test]
    fn violation_ranges_land_on_keys_values_and_first_lines() {
        let set = schema();
        let text = "{\n  \"private\": \"yes\",\n  \"extra\": 1\n}\n";
        let p = doc::parse_json(text).unwrap();
        let v = set.validate(&p).unwrap();
        let slice = |ptr: &str| {
            let (a, b) = range_of(&p, text, ptr);
            text[a..b].to_string()
        };
        assert_eq!(slice("/private"), "\"yes\"", "scalars: the value");
        assert_eq!(slice("/extra"), "1");
        assert_eq!(slice(""), "{", "root object: its first line");
        assert!(v.iter().any(|x| x.pointer.is_empty()));
        assert_eq!(slice("/missing/deep"), "{", "unknown pointers climb to an ancestor");
    }

    #[test]
    fn opaque_nodes_are_not_validated() {
        let set = SchemaSet::from_value(json!({"type": "object", "properties": {"Name": {"type": "object"}}}), "file:///s.json", &store());
        let docs = doc::parse_yaml("Name: !Ref Param\n");
        let parsed = docs[0].1.as_ref().unwrap();
        assert!(set.validate(parsed).unwrap().is_empty(), "!Ref value is a string in the document but resolves to anything");
        let docs = doc::parse_yaml("Name: plain\n");
        assert_eq!(set.validate(docs[0].1.as_ref().unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn remote_refs_go_through_the_store_and_broken_schemas_report_instead_of_panicking() {
        struct Fake;
        impl crate::store::Fetch for Fake {
            fn get(&self, url: &str) -> Result<String, String> {
                if url.ends_with("/part.json") { Ok(r#"{"type":"integer","minimum":3}"#.into()) } else { Err("404".into()) }
            }
        }
        let store = Store::with_fetch(Box::new(Fake), None, true);
        let set = SchemaSet::from_value(
            json!({"type":"object","properties":{"n":{"$ref":"https://example.com/part.json"}}}),
            "https://example.com/root.json",
            &store,
        );
        let bad = doc::parse_json(r#"{"n": 1}"#).unwrap();
        let v = set.validate(&bad).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pointer, "/n");
        assert!(set.validate(&doc::parse_json(r#"{"n": 5}"#).unwrap()).unwrap().is_empty());
        // Unresolvable $ref: compile error comes back as a message.
        let broken = SchemaSet::from_value(json!({"properties":{"n":{"$ref":"https://example.com/missing.json"}}}), "https://example.com/r.json", &store);
        assert!(broken.validate(&bad).unwrap_err().contains("could not be compiled"));
    }
}
