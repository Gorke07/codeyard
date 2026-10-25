//! Identifier case conversions (camelCase, snake_case, kebab-case…):
//! splits words at `_`, `-`, spaces and lower→upper case transitions.

/// Combining marks (e.g. the dot in "i̇") are part of the word.
fn is_combining(c: char) -> bool {
    ('\u{300}'..='\u{36f}').contains(&c)
}

/// Lowercase: Turkish "İ" becomes a single "i" (not two characters like "i̇").
fn lower(s: &str) -> String {
    s.chars().flat_map(|c| if c == 'İ' { 'i'.to_lowercase() } else { c.to_lowercase() }).collect()
}

/// Case-conversion language (`editor.caseLocale`): Turkish pairs `i` ↔ `İ` and `ı` ↔ `I`; others pair `i` ↔ `I`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseLocale {
    /// Turkish if the text has a Turkish-only letter (ı İ ş Ş ğ Ğ), else universal rules.
    Auto,
    Turkish,
    Invariant,
}

impl CaseLocale {
    pub fn from_setting(s: &str) -> Self {
        match s {
            "tr" => CaseLocale::Turkish,
            "en" => CaseLocale::Invariant,
            _ => CaseLocale::Auto,
        }
    }

    fn turkish(self, text: &str) -> bool {
        match self {
            CaseLocale::Turkish => true,
            CaseLocale::Invariant => false,
            CaseLocale::Auto => text.chars().any(|c| matches!(c, 'ı' | 'İ' | 'ş' | 'Ş' | 'ğ' | 'Ğ')),
        }
    }
}

fn upper_char(c: char, tr: bool, out: &mut String) {
    match (tr, c) {
        (true, 'i') => out.push('İ'),
        (true, 'ı') => out.push('I'),
        _ => out.extend(c.to_uppercase()),
    }
}

fn lower_char(c: char, tr: bool, out: &mut String) {
    match (tr, c) {
        (true, 'I') => out.push('ı'),
        (true, 'İ') => out.push('i'),
        _ => out.extend(c.to_lowercase()),
    }
}

/// Convert to uppercase (the command equivalent of ⌘K ⌘U).
pub fn upper(s: &str, loc: CaseLocale) -> String {
    let tr = loc.turkish(s);
    let mut out = String::with_capacity(s.len());
    s.chars().for_each(|c| upper_char(c, tr, &mut out));
    out
}

/// Convert to lowercase.
pub fn lower_locale(s: &str, loc: CaseLocale) -> String {
    let tr = loc.turkish(s);
    let mut out = String::with_capacity(s.len());
    s.chars().for_each(|c| lower_char(c, tr, &mut out));
    out
}

/// "hello wORLD" → "Hello World": first letter of each word uppercase, the rest lowercase (in Turkish "ilk IŞIK" → "İlk Işık").
pub fn title(s: &str, loc: CaseLocale) -> String {
    let tr = loc.turkish(s);
    let mut out = String::with_capacity(s.len());
    let mut start = true;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if start {
                upper_char(ch, tr, &mut out);
            } else {
                lower_char(ch, tr, &mut out);
            }
            start = false;
        } else {
            out.push(ch);
            start = !(ch == '\'' || ch == '’');
        }
    }
    out
}

/// "fooBar_baz-qux HTTPServer 2nd" → ["foo","Bar","baz","qux","HTTP","Server","2nd"].
pub fn split_words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &ch) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() && !is_combining(ch) {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        // aB → a|B ; ABc → A|Bc (a new word after an acronym)
        let boundary =
            ch.is_uppercase() && prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit() || (p.is_uppercase() && next.is_some_and(|n| n.is_lowercase())));
        if boundary && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn capitalize(w: &str) -> String {
    let mut it = w.chars();
    match it.next() {
        Some(f) => f.to_uppercase().chain(lower(it.as_str()).chars()).collect(),
        None => String::new(),
    }
}

pub fn to_camel(s: &str) -> String {
    let words = split_words(s);
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i == 0 {
            out.push_str(&lower(w));
        } else {
            out.push_str(&capitalize(w));
        }
    }
    out
}

pub fn to_pascal(s: &str) -> String {
    split_words(s).iter().map(|w| capitalize(w)).collect()
}

fn joined(s: &str, sep: &str, f: impl Fn(&str) -> String) -> String {
    split_words(s).iter().map(|w| f(w)).collect::<Vec<_>>().join(sep)
}

pub fn to_snake(s: &str) -> String {
    joined(s, "_", lower)
}

pub fn to_kebab(s: &str) -> String {
    joined(s, "-", lower)
}

pub fn to_constant(s: &str) -> String {
    joined(s, "_", str::to_uppercase)
}

/// Strip if the selection is fully inside `open`…`close`, else wrap (block comment toggle).
pub fn toggle_wrap(text: &str, open: &str, close: &str) -> String {
    let t = text.trim();
    if let Some(inner) = t.strip_prefix(open).and_then(|r| r.strip_suffix(close)) {
        let inner = inner.strip_prefix(' ').unwrap_or(inner);
        let inner = inner.strip_suffix(' ').unwrap_or(inner);
        // Leading/trailing whitespace is preserved.
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];
        return format!("{lead}{inner}{trail}");
    }
    let lead = &text[..text.len() - text.trim_start().len()];
    let trail = &text[text.trim_end().len()..];
    format!("{lead}{open} {t} {close}{trail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_on_separators_and_case_changes() {
        assert_eq!(split_words("fooBar_baz-qux"), ["foo", "Bar", "baz", "qux"]);
        assert_eq!(split_words("HTTPServer"), ["HTTP", "Server"]);
        assert_eq!(split_words("parseHTML5Doc"), ["parse", "HTML5", "Doc"]);
        assert_eq!(split_words("  a  b "), ["a", "b"]);
        assert!(split_words("__").is_empty());
    }

    #[test]
    fn conversions() {
        assert_eq!(to_camel("hello_world-foo"), "helloWorldFoo");
        assert_eq!(to_camel("HelloWorld"), "helloWorld");
        assert_eq!(to_pascal("hello_world"), "HelloWorld");
        assert_eq!(to_snake("HelloWorldFoo"), "hello_world_foo");
        assert_eq!(to_snake("HTTPServer"), "http_server");
        assert_eq!(to_kebab("helloWorld"), "hello-world");
        assert_eq!(to_constant("helloWorld"), "HELLO_WORLD");
        assert_eq!(to_snake("zaten_snake"), "zaten_snake");
        assert_eq!(to_camel("ışık_değeri"), "ışıkDeğeri");
    }

    #[test]
    fn block_wrap_toggles() {
        assert_eq!(toggle_wrap("foo()", "/*", "*/"), "/* foo() */");
        assert_eq!(toggle_wrap("/* foo() */", "/*", "*/"), "foo()");
        assert_eq!(toggle_wrap("  a\n b ", "<!--", "-->"), "  <!-- a\n b --> ");
        assert_eq!(toggle_wrap(&toggle_wrap("x y", "/*", "*/"), "/*", "*/"), "x y");
    }
}

#[cfg(test)]
mod locale_tests {
    use super::*;

    #[test]
    fn turkish_and_invariant_casing() {
        assert_eq!(upper("istanbul ılık", CaseLocale::Turkish), "İSTANBUL ILIK");
        assert_eq!(lower_locale("İSTANBUL ILIK", CaseLocale::Turkish), "istanbul ılık");
        assert_eq!(upper("title", CaseLocale::Invariant), "TITLE", "English `i` stays dotless-free");
        assert_eq!(lower_locale("TITLE", CaseLocale::Invariant), "title");
        // Auto: a Turkish-only letter in the text switches to Turkish rules, plain ASCII stays invariant.
        assert_eq!(upper("ışık iş", CaseLocale::Auto), "IŞIK İŞ");
        assert_eq!(upper("file index", CaseLocale::Auto), "FILE INDEX");
        assert_eq!(title("ilk IŞIK iki", CaseLocale::Auto), "İlk Işık İki");
        assert_eq!(title("hello wORLD it's", CaseLocale::Invariant), "Hello World It's");
        assert_eq!(CaseLocale::from_setting("tr"), CaseLocale::Turkish);
        assert_eq!(CaseLocale::from_setting("en"), CaseLocale::Invariant);
        assert_eq!(CaseLocale::from_setting("whatever"), CaseLocale::Auto);
    }
}

#[cfg(test)]
mod hardening {
    use super::*;

    #[test]
    fn random_unicode_never_panics_and_conversions_are_idempotent() {
        let alphabet = ['a', 'B', '_', '-', ' ', '1', 'ç', 'İ', 'ı', 'ß', '😀', 'Z', '.', '\n', 'ǅ'];
        let mut x = 7u64;
        for _ in 0..3000 {
            let n = 1 + (x % 20) as usize;
            let s: String = (0..n)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    alphabet[(x % alphabet.len() as u64) as usize]
                })
                .collect();
            let _ = (to_camel(&s), to_pascal(&s), to_kebab(&s), to_constant(&s), toggle_wrap(&s, "/*", "*/"));
            let snake = to_snake(&s);
            assert_eq!(to_snake(&snake), snake, "{s:?}");
        }
    }
}
