//! Schema support for JSON/YAML: span-preserving parsing, JSON Schema validation, hover and completion,
//! SchemaStore mapping and cached schema downloads. UI-independent; needs no Node, and also works on
//! remote (SSH) files because the content comes from the editor's memory.

pub mod cursor;
pub mod doc;
pub mod file_uri;
#[cfg(test)]
mod fuzz_tests;
pub mod gate;
pub mod ide;
pub mod store;
pub mod validate;
pub mod walk;
