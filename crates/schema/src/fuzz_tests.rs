//! Random (but repeatable) inputs: no crashes, and valid ranges. On half-typed, broken, Unicode-heavy JSON/YAML
//! no function may panic, and every byte range it returns must sit on a char boundary inside the text.

use serde_json::json;

use crate::cursor::{json_ctx, yaml_ctx};
use crate::doc::{parse_json, parse_yaml, position};
use crate::ide::{Format, complete_inner as complete, diagnose_inner as diagnose, hover_inner as hover};
use crate::store::Store;
use crate::validate::SchemaSet;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const TOKENS: &[&str] = &[
    "{",
    "}",
    "[",
    "]",
    ":",
    ",",
    "\"",
    "'",
    "-",
    " ",
    "\n",
    "  ",
    "    ",
    "# ",
    "// ",
    "/*",
    "*/",
    "true",
    "null",
    "12",
    "-3.5e2",
    "key",
    "name",
    "a.b",
    "\\",
    "\\u00e9",
    "é",
    "ğ",
    "ü",
    "😀",
    "\u{200b}",
    "&a",
    "*a",
    "!tag",
    "---",
    "...",
    "<<",
    "? ",
    "| ",
    "> ",
    "port: ",
    "- ",
    "$schema",
    "\"name\": ",
    "\t",
];

fn random_text(r: &mut Rng) -> String {
    (0..r.below(60)).map(|_| TOKENS[r.below(TOKENS.len())]).collect()
}

fn schema() -> std::sync::Arc<SchemaSet> {
    SchemaSet::from_value(
        json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {"type": "string", "description": "n"},
                "port": {"type": "integer"},
                "mode": {"enum": ["a", "b"]},
                "list": {"type": "array", "items": {"type": "object", "properties": {"x": {"type": "boolean"}}}},
                "nested": {"type": "object", "properties": {"deep": {"type": "object", "properties": {"k": {"type": "string"}}}}}
            }
        }),
        "file:///fuzz.json",
        &Store::new(None, false),
    )
}

fn boundaries(text: &str, r: &mut Rng) -> Vec<usize> {
    let all: Vec<usize> = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
    (0..8).map(|_| all[r.below(all.len())]).collect()
}

#[test]
fn nothing_panics_and_every_range_is_valid_on_random_documents() {
    let set = schema();
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    for round in 0..2500 {
        let text = random_text(&mut r);
        let _ = parse_json(&text);
        let _ = parse_yaml(&text);
        for format in [Format::Json, Format::Yaml] {
            for d in diagnose(&text, format, Some(&set)) {
                assert!(d.start <= d.end && d.end <= text.len(), "round {round} {format:?}: {d:?} in {text:?}");
                assert!(text.is_char_boundary(d.start) && text.is_char_boundary(d.end), "round {round}: diagnostic splits a character: {d:?} in {text:?}");
            }
            for at in boundaries(&text, &mut r) {
                let _ = position(&text, at);
                if let Some(c) = complete(&set, format, &text, at) {
                    assert!(
                        c.filter_start <= at.max(c.filter_start) && text.is_char_boundary(c.filter_start),
                        "round {round}: filter start {} for {text:?} at {at}",
                        c.filter_start
                    );
                    for item in &c.items {
                        assert!(item.start <= item.end && item.end <= text.len(), "round {round} {format:?}: {item:?} in {text:?} at {at}");
                        assert!(
                            text.is_char_boundary(item.start) && text.is_char_boundary(item.end),
                            "round {round}: completion splits a character: {item:?} in {text:?}"
                        );
                        assert!(item.start <= at && at <= item.end, "the replaced range must contain the cursor: {item:?} at {at} in {text:?}");
                        assert!(!item.label.is_empty());
                        if let Some(cur) = item.cursor {
                            assert!(cur <= item.insert.chars().count(), "cursor outside the inserted text: {item:?}");
                        }
                    }
                }
                if let Some((_, a, b)) = hover(&set, format, &text, at) {
                    assert!(
                        a <= b && b <= text.len() && text.is_char_boundary(a) && text.is_char_boundary(b),
                        "round {round}: hover range {a}..{b} in {text:?}"
                    );
                }
                let _ = (json_ctx(&text, at), yaml_ctx(&text, at));
            }
        }
    }
}

#[test]
fn truncated_valid_documents_never_panic_at_any_cut() {
    let set = schema();
    let json_doc =
        "{\n  \"name\": \"ğüş é 😀\",\n  \"list\": [{\"x\": true}, {\"x\": fal}],\n  \"nested\": {\"deep\": {\"k\": \"v\"}}, // c\n  \"port\": 80\n}\n";
    let yaml_doc = "name: ğüş\nlist:\n  - x: true\n  - x:\nnested:\n  deep:\n    k: \"v\"\n# c\nport: 80\n---\nname: 2\n";
    for (doc, format) in [(json_doc, Format::Json), (yaml_doc, Format::Yaml)] {
        for cut in doc.char_indices().map(|(i, _)| i).chain(std::iter::once(doc.len())) {
            let text = &doc[..cut];
            for d in diagnose(text, format, Some(&set)) {
                assert!(text.is_char_boundary(d.start) && text.is_char_boundary(d.end) && d.end <= text.len(), "{d:?} at cut {cut}");
            }
            let _ = complete(&set, format, text, cut);
            let _ = hover(&set, format, text, cut.saturating_sub(1).min(text.len()));
            // Every prefix also works with the cursor in the middle.
            let mid = text.char_indices().map(|(i, _)| i).nth(text.chars().count() / 2).unwrap_or(0);
            let _ = complete(&set, format, text, mid);
        }
    }
}
