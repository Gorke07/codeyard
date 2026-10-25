//! Spell checking: Hunspell dictionaries (pure Rust), word-extraction rules, user dictionary and dictionary download.
//!
//! The English dictionary (SCOWL, MIT/BSD) ships with the app; other languages are downloaded on request and cached
//! ([`install`]). Several dictionaries work together (a word is correct if any accepts it).

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

pub use spellbook::Dictionary;

/// Developer words (Kubernetes, regex, tokio…): not in the English dictionary, common in programmer text.
const TECH: &str = include_str!("../dicts/tech.txt");

const EN_AFF: &str = include_str!("../dicts/en/index.aff");
const EN_DIC: &str = include_str!("../dicts/en/index.dic");

/// Longest word we look up suggestions for (characters).
const MAX_SUGGEST_WORD: usize = 64;

/// Word cache cap (keeps memory bounded in long sessions).
const CACHE_MAX: usize = 50_000;

/// What gets checked: everywhere in prose files (except code/links), only comments in code.
pub fn prose_extension(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "md" | "markdown" | "txt" | "rst" | "adoc" | "asciidoc" | "tex" | "org" | "text" | "mdx")
}

/// Whether this is a Markdown file (inline code is skipped).
pub fn is_markdown(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "md" | "markdown" | "mdx")
}

pub fn embedded_english() -> Dictionary {
    Dictionary::new(EN_AFF, EN_DIC).expect("the bundled English dictionary parses")
}

/// Dictionary from `dir/index.aff` and `dir/index.dic` (UTF-8).
pub fn load_dir(dir: &Path) -> Result<Dictionary, String> {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()));
    parse(&read("index.aff")?, &read("index.dic")?).map_err(|e| format!("{}: {e}", dir.display()))
}

/// Parse the dictionary. `FLAG num` dictionaries like Turkish use flag `0`, which spellbook rejects.
/// If flags occur only in affix (SFX/PFX) definitions, all are shifted up by one (meaning unchanged).
fn parse(aff: &str, dic: &str) -> Result<Dictionary, String> {
    match Dictionary::new(aff, dic) {
        Ok(d) => Ok(d),
        Err(first) => match shift_numeric_flags(aff, dic) {
            Some((a, d)) => Dictionary::new(&a, &d).map_err(|e| e.to_string()),
            None => Err(first.to_string()),
        },
    }
}

fn shift_numeric_flags(aff: &str, dic: &str) -> Option<(String, String)> {
    // Other flag-bearing directives (COMPOUNDFLAG, KEEPCASE…) make the shift unsafe.
    const PLAIN: &[&str] = &["SET", "LANG", "TRY", "FLAG", "SFX", "PFX", "NAME", "HOME", "VERSION", "REP", "MAP", "ICONV", "OCONV", "WORDCHARS", "BREAK"];
    let mut num = false;
    let mut out = String::with_capacity(aff.len() + 1024);
    for line in aff.lines() {
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("FLAG") => {
                num = parts.next() == Some("num");
                out.push_str(line);
            }
            Some(d @ ("SFX" | "PFX")) => {
                let rest: Vec<&str> = parts.collect();
                let flag: u32 = rest.first()?.parse().ok()?;
                out.push_str(&format!("{d} {}", flag + 1));
                for r in &rest[1..] {
                    out.push(' ');
                    out.push_str(r);
                }
            }
            Some(d) if !d.starts_with('#') && !PLAIN.contains(&d) => return None,
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    if !num {
        return None;
    }
    let mut dic_out = String::with_capacity(dic.len() + dic.len() / 16);
    for (i, line) in dic.lines().enumerate() {
        match (i, line.split_once('/')) {
            (0, _) | (_, None) => dic_out.push_str(line),
            (_, Some((word, rest))) => {
                let (flags, tail) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(f, t)| (f, t));
                dic_out.push_str(word);
                dic_out.push('/');
                let shifted: Option<Vec<String>> = flags.split(',').map(|f| f.parse::<u32>().ok().map(|n| (n + 1).to_string())).collect();
                dic_out.push_str(&shifted?.join(","));
                if !tail.is_empty() {
                    dic_out.push(' ');
                    dic_out.push_str(tail);
                }
            }
        }
        dic_out.push('\n');
    }
    Some((out, dic_out))
}

// --- Downloadable dictionaries ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Available {
    pub id: &'static str,
    pub name: &'static str,
    /// The dictionary's license (downloaded data; not distributed with the app).
    pub license: &'static str,
    /// Approximate download size (MB).
    pub megabytes: u32,
}

/// Recommended dictionaries (`wooorm/dictionaries`, Hunspell-based, UTF-8). `en` ships with the app.
pub const AVAILABLE: &[Available] = &[
    Available { id: "en-GB", name: "English (United Kingdom)", license: "MIT AND BSD", megabytes: 1 },
    Available { id: "tr", name: "Turkish (Türkçe)", license: "MIT", megabytes: 9 },
    Available { id: "de", name: "German (Deutsch)", license: "GPL-2.0 OR GPL-3.0", megabytes: 2 },
    Available { id: "fr", name: "French (Français)", license: "MPL-2.0", megabytes: 2 },
    Available { id: "es", name: "Spanish (Español)", license: "GPL-3.0 OR LGPL-3.0 OR MPL-1.1", megabytes: 1 },
    Available { id: "it", name: "Italian (Italiano)", license: "GPL-3.0", megabytes: 1 },
    Available { id: "pt", name: "Portuguese (Português)", license: "LGPL-3.0 OR MPL-2.0", megabytes: 2 },
    Available { id: "nl", name: "Dutch (Nederlands)", license: "BSD-3-Clause OR CC-BY-3.0", megabytes: 1 },
    Available { id: "ru", name: "Russian (Русский)", license: "BSD-3-Clause", megabytes: 5 },
    Available { id: "pl", name: "Polish (Polski)", license: "GPL-3.0 OR LGPL-3.0 OR MPL-2.0", megabytes: 3 },
    Available { id: "sv", name: "Swedish (Svenska)", license: "LGPL-3.0", megabytes: 3 },
    Available { id: "uk", name: "Ukrainian (Українська)", license: "GPL-3.0", megabytes: 5 },
    Available { id: "cs", name: "Czech (Čeština)", license: "GPL-2.0", megabytes: 1 },
    Available { id: "da", name: "Danish (Dansk)", license: "GPL-2.0 OR LGPL-2.1 OR MPL-1.1", megabytes: 1 },
    Available { id: "hu", name: "Hungarian (Magyar)", license: "GPL-2.0 OR LGPL-2.1 OR MPL-1.1", megabytes: 2 },
    Available { id: "ro", name: "Romanian (Română)", license: "GPL-2.0 OR LGPL-2.1 OR MPL-1.1", megabytes: 1 },
];

pub fn available(id: &str) -> Option<&'static Available> {
    AVAILABLE.iter().find(|a| a.id.eq_ignore_ascii_case(id))
}

/// Whether the language id is a safe folder name (no path traversal or URL injection).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 12 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Cache folder of a dictionary: `<root>/<id>`.
pub fn dir_for(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}

pub trait Fetch {
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

pub struct Curl;

impl Fetch for Curl {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        let out = std::process::Command::new("curl")
            .args(["-fsSL", "--max-time", "120", "--max-filesize", "40000000", "--"])
            .arg(url)
            .output()
            .map_err(|e| format!("curl: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() { format!("could not download {url}") } else { err })
        }
    }
}

pub fn dictionary_url(id: &str, file: &str) -> String {
    format!("https://raw.githubusercontent.com/wooorm/dictionaries/main/dictionaries/{id}/{file}")
}

/// Download the dictionary and write it to `root/<id>`. Files are parsed before writing (a corrupt one never enters the cache);
/// the write is atomic (temp folder + rename). The license file is kept too.
pub fn install(fetch: &dyn Fetch, root: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err(format!("“{id}” is not a valid dictionary name"));
    }
    let text = |file: &str| -> Result<String, String> {
        String::from_utf8(fetch.get(&dictionary_url(id, file))?).map_err(|_| format!("{id}/{file} is not UTF-8 text"))
    };
    let (aff, dic) = (text("index.aff")?, text("index.dic")?);
    parse(&aff, &dic).map_err(|e| format!("The {id} dictionary could not be read: {e}"))?;
    let license = text("license").unwrap_or_default();
    // Leftover partial folders from a crashed install (`.<id>.part<pid>`, `.<id>.old<pid>`) are removed, but only if older than an
    // hour: another app installing right now owns its live temp folder.
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let stale = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Duration::from_secs(3600));
            if stale && (name.starts_with(&format!(".{id}.part")) || name.starts_with(&format!(".{id}.old"))) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let tmp = root.join(format!(".{id}.part{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let write = |name: &str, body: &str| std::fs::write(tmp.join(name), body).map_err(|e| format!("{}: {e}", tmp.join(name).display()));
    write("index.aff", &aff)?;
    write("index.dic", &dic)?;
    if !license.is_empty() {
        write("license", &license)?;
    }
    let dest = dir_for(root, id);
    // Move the old copy aside, put the new one in place; if the rename fails the old dictionary is restored (it used to be
    // deleted first and then moved, so one failure lost the dictionary for good).
    let old = root.join(format!(".{id}.old{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&old);
    let had_old = dest.exists() && std::fs::rename(&dest, &old).is_ok();
    match std::fs::rename(&tmp, &dest) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&old);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            if had_old {
                let _ = std::fs::rename(&old, &dest);
            }
            Err(format!("{}: {e}", dest.display()))
        }
    }
}

/// IDs of installed (cached) dictionaries.
pub fn installed(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .map(|rd| rd.flatten().filter(|e| e.path().join("index.dic").is_file()).filter_map(|e| e.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

// --- Checker ---

pub struct Speller {
    dicts: Vec<Dictionary>,
    user: Mutex<HashSet<String>>,
    cache: Mutex<HashMap<String, bool>>,
}

impl Speller {
    pub fn new(dicts: Vec<Dictionary>, user_words: impl IntoIterator<Item = String>) -> Speller {
        let user: HashSet<String> = TECH.lines().map(str::to_string).chain(user_words.into_iter().map(|w| w.to_lowercase())).collect();
        Speller { dicts, user: Mutex::new(user), cache: Mutex::new(HashMap::new()) }
    }

    pub fn is_empty(&self) -> bool {
        self.dicts.is_empty()
    }

    /// Add to the user dictionary (this session and the file the caller persists).
    pub fn add_user_word(&self, word: &str) {
        self.user.lock().unwrap_or_else(|e| e.into_inner()).insert(word.to_lowercase());
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub fn check_word(&self, word: &str) -> bool {
        if let Some(&ok) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(word) {
            return ok;
        }
        let ok = self.check_uncached(word);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= CACHE_MAX {
            cache.clear();
        }
        cache.insert(word.to_string(), ok);
        ok
    }

    fn check_uncached(&self, word: &str) -> bool {
        if self.user.lock().unwrap_or_else(|e| e.into_inner()).contains(&word.to_lowercase()) {
            return true;
        }
        let known = |w: &str| self.dicts.iter().any(|d| d.check(w));
        if known(word) {
            return true;
        }
        // Typographic apostrophe: "don’t" is "don't" in the dictionary.
        if word.contains('’') && known(&word.replace('’', "'")) {
            return true;
        }
        // Proper noun + suffix: "İstanbul'a", "Ahmet'in": the suffix is accepted if the part before the apostrophe is valid.
        if let Some((stem, suffix)) = word.split_once(['\'', '’'])
            && stem.chars().count() >= 2
            && !suffix.is_empty()
            && suffix.chars().count() <= 6
            && suffix.chars().all(char::is_alphabetic)
        {
            // The stem may be in the user dictionary too: ask the cached general check.
            return self.check_word(stem);
        }
        false
    }

    /// At most 8 suggestions (dictionary order, no duplicates). For a capitalized or all-caps word
    /// they are looked up on the lowercase form and returned in the same shape ("Teh" → "The", "TEH" → "THE").
    pub fn suggest(&self, word: &str) -> Vec<String> {
        // Very long "words" (run-together text, base64) can take seconds to get suggestions for and the result is useless.
        if word.chars().count() > MAX_SUGGEST_WORD {
            return Vec::new();
        }
        let first_upper = word.chars().next().is_some_and(char::is_uppercase);
        let rest_lower = word.chars().skip(1).all(|c| !c.is_uppercase());
        let all_upper = word.chars().count() > 1 && word.chars().all(|c| !c.is_lowercase());
        let lower = word.to_lowercase();
        let recase = |s: String| -> String {
            if all_upper {
                s.to_uppercase()
            } else if first_upper && rest_lower {
                let mut it = s.chars();
                it.next().map(|f| f.to_uppercase().collect::<String>() + it.as_str()).unwrap_or(s)
            } else {
                s
            }
        };
        let mut out: Vec<String> = Vec::new();
        let collect = |query: &str, recase_all: bool, out: &mut Vec<String>| {
            for d in &self.dicts {
                let mut s = Vec::new();
                d.suggest(query, &mut s);
                for x in s {
                    let x = if recase_all { recase(x) } else { x };
                    if !out.contains(&x) {
                        out.push(x);
                    }
                }
            }
        };
        if first_upper || all_upper {
            collect(&lower, true, &mut out);
        }
        // If it didn't come from the lowercase form (proper nouns), use the word itself.
        if out.is_empty() {
            collect(word, false, &mut out);
        }
        out.truncate(8);
        out
    }

    /// Byte ranges of misspelled words in a line. `allowed(i)`: whether the byte position may be checked
    /// (comment, prose…). Code fragments, addresses, tokens with digits and mixed-case ones (camelCase, SNAKE_CASE) are skipped.
    pub fn misspelled(&self, line: &str, allowed: &dyn Fn(usize) -> bool) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        for tok in tokens(line) {
            if !allowed(tok.range.start) || tok.skip {
                continue;
            }
            let word = &line[tok.range.clone()];
            if !self.check_word(word) {
                out.push(tok.range);
            }
        }
        out
    }
}

struct Token {
    range: Range<usize>,
    skip: bool,
}

/// A letter or a combining mark attached to one (`e` + U+0301 = é): the mark must not split the word.
fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || ('\u{0300}'..='\u{036f}').contains(&c)
}

fn is_apostrophe(c: char) -> bool {
    c == '\'' || c == '’'
}

/// Split the line into words; decide here which ones get checked.
fn tokens(line: &str) -> Vec<Token> {
    // Addresses and emails: the whole whitespace-free chunk around them is skipped. Chunks are found in one pass
    // (ordered, disjoint); searching back to the line start for each `@` would be quadratic on a megabyte line.
    let mut skip_ranges: Vec<Range<usize>> = Vec::new();
    let mut chunk_start: Option<usize> = None;
    for (i, c) in line.char_indices().chain(std::iter::once((line.len(), ' '))) {
        match (c.is_whitespace(), chunk_start) {
            (false, None) => chunk_start = Some(i),
            (true, Some(s)) => {
                let chunk = &line[s..i];
                if chunk.contains("://") || chunk.contains("www.") || chunk.contains('@') {
                    skip_ranges.push(s..i);
                }
                chunk_start = None;
            }
            _ => {}
        }
    }
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_word_char(chars[i].1) {
            i += 1;
            continue;
        }
        let start = i;
        // An apostrophe is part of a word only between two letters (don't, İstanbul'a).
        while i < chars.len() && (is_word_char(chars[i].1) || (is_apostrophe(chars[i].1) && i > start && chars.get(i + 1).is_some_and(|c| is_word_char(c.1)))) {
            i += 1;
        }
        let (b0, b1) = (chars[start].0, chars.get(i).map_or(line.len(), |c| c.0));
        let word = &line[b0..b1];
        let prev = start.checked_sub(1).map(|p| chars[p].1);
        let next = chars.get(i).map(|c| c.1);
        let letters = word.chars().filter(|c| c.is_alphabetic()).count();
        let upper = word.chars().filter(|c| c.is_uppercase()).count();
        let lower = word.chars().filter(|c| c.is_lowercase()).count();
        // Adjacent digit, underscore or code prefix (@ # $ \ / % &): identifier, variable, path.
        let glued = prev.is_some_and(|c| c.is_ascii_digit() || matches!(c, '_' | '@' | '#' | '$' | '\\' | '/' | '%' | '&'))
            || next.is_some_and(|c| c.is_ascii_digit() || c == '_');
        // `a.b` (file name, field access): not a sentence-ending period.
        let dotted = (prev == Some('.') && start >= 2 && is_word_char(chars[start - 2].1))
            || (next == Some('.') && chars.get(i + 1).is_some_and(|c| is_word_char(c.1) || c.1.is_ascii_digit()));
        let hyphen_digit = prev == Some('-') && start >= 2 && chars[start - 2].1.is_ascii_digit();
        // camelCase / PascalCase (capital inside), ACRONYM (all caps).
        let camel = lower > 0 && word.chars().skip(1).any(char::is_uppercase);
        let acronym = upper == letters && letters > 1;
        let in_address = {
            let k = skip_ranges.partition_point(|r| r.end <= b0);
            skip_ranges.get(k).is_some_and(|r| r.contains(&b0))
        };
        let skip = letters < 3 || glued || dotted || hyphen_digit || camel || acronym || in_address;
        out.push(Token { range: b0..b1, skip });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_huge_line_of_addresses_is_tokenized_in_linear_time() {
        let line = "a@b ".repeat(200_000) + &"x@".repeat(200_000); // ~1.4 MB, the second half has no whitespace at all
        let started = std::time::Instant::now();
        let _ = tokens(&line);
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        // Meaning is unchanged: words inside an address are skipped, neighbours are not.
        let s = en();
        assert_eq!(bad(&s, "mail john@exmple.com about the sentense"), ["sentense"]);
        assert_eq!(bad(&s, "see https://exmple.org/pagee and the mistke"), ["mistke"]);
        assert_eq!(bad(&s, "visit www.exmple.org then wrte"), ["wrte"]);
    }

    struct Fixed(Vec<u8>, Vec<u8>);
    impl Fetch for Fixed {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            if url.ends_with("index.aff") {
                Ok(self.0.clone())
            } else if url.ends_with("index.dic") {
                Ok(self.1.clone())
            } else {
                Err("404".into())
            }
        }
    }

    #[test]
    fn reinstalling_keeps_a_working_copy_and_leaves_other_installers_temp_folders_alone() {
        let root = std::env::temp_dir().join(format!("codeyard-spell-reinstall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fetch = Fixed(b"SET UTF-8\n".to_vec(), b"1\nhello\n".to_vec());
        install(&fetch, &root, "xx").unwrap();
        assert_eq!(installed(&root), ["xx"]);
        // Another process is installing right now: its temp folder must survive ours.
        let other = root.join(".xx.part99999");
        std::fs::create_dir_all(&other).unwrap();
        install(&fetch, &root, "xx").unwrap();
        assert!(other.exists(), "a fresh temp folder of someone else is not swept");
        assert_eq!(installed(&root), ["xx"]);
        assert!(!root.join(format!(".xx.old{}", std::process::id())).exists(), "no leftover backup folder");
        // A broken download never replaces the working dictionary.
        let broken = Fixed(b"SET UTF-8\n".to_vec(), b"not a count\n".to_vec());
        assert!(install(&broken, &root, "xx").is_err());
        assert_eq!(installed(&root), ["xx"], "the old copy still works");
    }

    fn en() -> Speller {
        Speller::new(vec![embedded_english()], [])
    }

    fn bad(s: &Speller, line: &str) -> Vec<String> {
        s.misspelled(line, &|_| true).into_iter().map(|r| line[r].to_string()).collect()
    }

    #[test]
    fn finds_real_misspellings_and_accepts_normal_prose() {
        let s = en();
        assert_eq!(bad(&s, "This sentense has a mistake, and teh typo."), ["sentense", "teh"]);
        assert!(bad(&s, "The quick brown fox jumps over the lazy dog. Don't panic, it's fine.").is_empty());
        assert_eq!(bad(&s, "Colour is the British spelling"), ["Colour"], "the bundled dictionary is American English");
        assert!(bad(&s, "Hello Hello HELLO hello").is_empty(), "capitalisation variants of a known word");
    }

    #[test]
    fn developer_words_are_accepted_out_of_the_box() {
        let s = en();
        assert!(bad(&s, "Deploy it with Kubernetes and Docker, then check the JSON config in the repo on GitHub with regex.").is_empty());
        let tech: Vec<&str> = TECH.lines().collect();
        assert!(tech.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
        assert!(tech.iter().all(|w| w.chars().all(|c| c.is_ascii_lowercase())));
    }

    #[test]
    fn code_like_tokens_are_not_spell_checked() {
        let s = en();
        let line =
            "see https://example.com/xyzzy and user@exmple.org plus fooBarBaz, snake_case_xyz, HTTPS, v1beta, file.rsx, 0xdeadbeef, #hashtagxyz, $varxyz, a-b";
        assert_eq!(bad(&s, line), Vec::<String>::new(), "{:?}", bad(&s, line));
        // Short words are never flagged.
        assert!(bad(&s, "zz qq").is_empty());
        // A sentence-ending period is not a dotted identifier.
        assert_eq!(bad(&s, "It is wrong. Mispeled here."), ["Mispeled"]);
    }

    #[test]
    fn the_allowed_mask_limits_what_is_checked() {
        let s = en();
        let line = "mispeled // coment mispeled";
        let from = line.find("//").unwrap();
        let in_comment = s.misspelled(line, &|i| i >= from);
        assert_eq!(in_comment.iter().map(|r| &line[r.clone()]).collect::<Vec<_>>(), ["coment", "mispeled"]);
    }

    #[test]
    fn user_words_and_apostrophe_suffixes() {
        let s = en();
        assert!(!s.check_word("zorbulon"));
        s.add_user_word("Zorbulon");
        assert!(s.check_word("zorbulon") && s.check_word("Zorbulon") && s.check_word("ZORBULON"));
        // Proper noun + suffix after an apostrophe: the stem decides.
        assert!(s.check_word("Zorbulon'in") && s.check_word("Zorbulon’de"));
        assert!(!s.check_word("zzqxv'in"));
        assert!(s.check_word("don’t"), "typographic apostrophe");
    }

    #[test]
    fn suggestions_are_offered_for_close_words() {
        let s = en();
        let sug = s.suggest("sentense");
        assert!(sug.contains(&"sentence".to_string()), "{sug:?}");
        assert!(sug.len() <= 8);
        assert!(s.suggest(&"a".repeat(200)).is_empty(), "absurdly long words get no suggestions (and no multi-second search)");
        assert_eq!(s.suggest("teh").first().map(String::as_str), Some("the"));
        assert_eq!(s.suggest("Teh").first().map(String::as_str), Some("The"), "a capital letter is kept");
        assert_eq!(s.suggest("TEH").first().map(String::as_str), Some("THE"));
    }

    #[test]
    fn non_ascii_text_keeps_byte_ranges_valid() {
        let s = en();
        let line = "çay and teh çok güzel";
        let r = s.misspelled(line, &|_| true);
        for x in &r {
            assert!(line.is_char_boundary(x.start) && line.is_char_boundary(x.end));
        }
        assert!(r.iter().any(|x| &line[x.clone()] == "teh"));
    }

    struct Fake(HashMap<String, Result<Vec<u8>, String>>);
    impl Fetch for Fake {
        fn get(&self, url: &str) -> Result<Vec<u8>, String> {
            self.0.get(url).cloned().unwrap_or_else(|| Err(format!("404 {url}")))
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("codeyard-spell-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn install_validates_writes_atomically_and_loads() {
        let root = tmp("install");
        let aff = "SET UTF-8\n";
        let dic = "3\nmerhaba\nistanbul\ndünya\n";
        let mut files = HashMap::new();
        files.insert(dictionary_url("xx", "index.aff"), Ok(aff.as_bytes().to_vec()));
        files.insert(dictionary_url("xx", "index.dic"), Ok(dic.as_bytes().to_vec()));
        files.insert(dictionary_url("xx", "license"), Ok(b"MIT".to_vec()));
        install(&Fake(files), &root, "xx").unwrap();
        assert_eq!(installed(&root), ["xx"]);
        let d = load_dir(&dir_for(&root, "xx")).unwrap();
        assert!(d.check("merhaba") && d.check("dünya") && !d.check("hello"));
        assert!(root.join("xx/license").is_file());
        assert!(std::fs::read_dir(&root).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains(".part")), "no temp folders left");

        // A corrupt download never reaches the cache; a failed fetch reports the reason.
        let mut bad = HashMap::new();
        bad.insert(dictionary_url("yy", "index.aff"), Ok(b"SET UTF-8\n".to_vec()));
        bad.insert(dictionary_url("yy", "index.dic"), Ok(b"not a number\nword\n".to_vec()));
        assert!(install(&Fake(bad), &root, "yy").is_err());
        assert_eq!(installed(&root), ["xx"]);
        assert!(install(&Fake(HashMap::new()), &root, "zz").unwrap_err().contains("404"));
        // Ids cannot escape the cache folder or the download address.
        for evil in ["../x", "a/b", "", "a b", "x?y=1", "waytoolongdictionaryid"] {
            assert!(install(&Fake(HashMap::new()), &root, evil).unwrap_err().contains("not a valid"), "{evil}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Manual verification: `CODEYARD_TR_DICT_DIR=<folder with index.aff/index.dic> cargo test -p spell -- --ignored turkish`
    /// (the 9 MB dictionary isn't in the repo; users download it with "Spelling: Install Dictionary").
    #[test]
    #[ignore = "needs the 9 MB Turkish dictionary, which is not in the repo; set CODEYARD_TR_DICT_DIR"]
    fn turkish_dictionary_smoke() {
        let Some(dir) = std::env::var_os("CODEYARD_TR_DICT_DIR") else {
            // Under `--ignored` a silent return would read as a pass.
            panic!("turkish_dictionary_smoke: CODEYARD_TR_DICT_DIR is not set; it was asked for with --ignored, so this must not pass vacuously");
        };
        let start = std::time::Instant::now();
        let d = load_dir(Path::new(&dir)).expect("tr dictionary parses");
        eprintln!("loaded in {:?}", start.elapsed());
        let s = Speller::new(vec![d, embedded_english()], []);
        for ok in ["Merhaba", "dünya", "İstanbul", "ışık", "çalışıyorum", "İstanbul'a", "Ankara'nın", "kitaplarımızdan", "YAZILIM", "ağaç"] {
            assert!(s.check_word(ok), "{ok} should be accepted");
        }
        for wrong in ["merhabaa", "dünyaa", "kitapplar", "yazilim", "çalısıyorum"] {
            assert!(!s.check_word(wrong), "{wrong} should be flagged");
        }
        let sug = s.suggest("kitapplar");
        eprintln!("suggestions for kitapplar: {sug:?}");
        assert!(sug.iter().any(|x| x == "kitaplar"), "{sug:?}");
    }

    #[test]
    fn numeric_flag_zero_dictionaries_are_made_loadable() {
        // Same shape as the Turkish dictionary: `FLAG num`, suffix flag 0.
        let aff = "SET UTF-8\nFLAG num\n\nSFX 0 N 1\nSFX 0 0 de .\n\nSFX 1 Y 1\nSFX 1 0 lar .\n";
        let dic = "2\nev/0,1\nkitap/1\n";
        assert!(Dictionary::new(aff, dic).is_err(), "spellbook alone rejects flag 0");
        let d = parse(aff, dic).unwrap();
        assert!(d.check("ev") && d.check("evde") && d.check("evlar") && d.check("kitaplar"));
        assert!(!d.check("kitapde") && !d.check("evdelar"));
        // Dictionaries with flag-bearing directives we do not understand are left alone (and report the error).
        assert!(shift_numeric_flags("SET UTF-8\nFLAG num\nCOMPOUNDFLAG 0\n", "1\na/0\n").is_none());
        // Morphological fields after the flags survive.
        let (_, d2) = shift_numeric_flags(aff, "1\nev/0,1 po:noun\n").unwrap();
        assert_eq!(d2.lines().nth(1), Some("ev/1,2 po:noun"));
    }

    #[test]
    fn combining_marks_stay_inside_words_and_stale_partial_installs_are_cleaned() {
        let s = en();
        // "café" written with e + combining acute is one word, not "caf" + a stray mark.
        let word = "cafe\u{301}";
        let r = s.misspelled(&format!("{word} and teh"), &|_| true);
        let found: Vec<String> = r.iter().map(|x| format!("{word} and teh")[x.clone()].to_string()).collect();
        assert!(!found.iter().any(|w| w == "caf"), "{found:?}");
        assert!(found.iter().any(|w| w == "teh"));
        let root = tmp("stale");
        std::fs::create_dir_all(root.join(".xx.part99999/junk")).unwrap();
        // Only leftovers older than an hour count as crashed (a fresh one may belong to a running installer).
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        // Windows only lets a directory handle change times when it is opened for attribute writes with backup semantics.
        #[cfg(windows)]
        let stale = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            std::fs::OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(root.join(".xx.part99999"))
        };
        #[cfg(not(windows))]
        let stale = std::fs::File::open(root.join(".xx.part99999"));
        stale.unwrap().set_modified(long_ago).unwrap();
        let mut files = HashMap::new();
        files.insert(dictionary_url("xx", "index.aff"), Ok(b"SET UTF-8\n".to_vec()));
        files.insert(dictionary_url("xx", "index.dic"), Ok(b"1\nword\n".to_vec()));
        install(&Fake(files), &root, "xx").unwrap();
        assert!(!root.join(".xx.part99999").exists(), "leftovers of a crashed install are removed");
        assert_eq!(installed(&root), ["xx"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prose_files_and_catalog() {
        assert!(prose_extension("/a/README.md") && prose_extension("notes.TXT") && !prose_extension("main.rs"));
        assert!(available("TR").is_some() && available("klingon").is_none());
        assert!(AVAILABLE.iter().all(|a| valid_id(a.id)));
        let mut ids: Vec<_> = AVAILABLE.iter().map(|a| a.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), AVAILABLE.len());
    }

    #[test]
    fn random_text_never_panics_and_ranges_are_valid() {
        let tokens = [
            "a",
            "teh",
            "Hello",
            "é",
            "e\u{301}",
            "ğ",
            "İstanbul",
            "'",
            "’",
            "-",
            "_",
            " ",
            "\n",
            "1",
            "@",
            "http://x.y/z",
            "www.q.com",
            ".",
            "ABC",
            "camelCase",
            "😀",
            "ß",
            "\u{200d}",
            "/",
            "\\",
            "#",
        ];
        let s = en();
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..4000 {
            let n = (next() % 20) as usize;
            let text: String = (0..n).map(|_| tokens[(next() % tokens.len() as u64) as usize]).collect();
            for r in s.misspelled(&text, &|_| true) {
                assert!(r.start < r.end && r.end <= text.len() && text.is_char_boundary(r.start) && text.is_char_boundary(r.end), "{text:?} {r:?}");
            }
            // The allowed-mask is honoured: nothing is reported where it says no.
            assert!(s.misspelled(&text, &|_| false).is_empty());
            // Suggestions are expensive (n-gram search in a big dictionary): only a few samples, short words.
            if text.chars().count() <= 8 && n < 3 {
                let _ = s.suggest(&text);
            }
        }
    }
}
