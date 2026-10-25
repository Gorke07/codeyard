//! Emmet: expands an abbreviation to HTML/JSX or CSS. Output uses snippet syntax (`$1`, `$0`
//! tab stops, `\` escape); the editor places it with `expand_snippet_at`.

/// Expands the abbreviation. `jsx`: `className` instead of `class`. `None`: not a valid abbreviation.
pub fn expand_html(abbr: &str, jsx: bool) -> Option<String> {
    let abbr = abbr.trim();
    if abbr.is_empty() {
        return None;
    }
    if abbr == "!" || abbr == "html:5" {
        return Some("<!DOCTYPE html>\n<html lang=\"${1:en}\">\n<head>\n\t<meta charset=\"UTF-8\">\n\t<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n\t<title>${2:Document}</title>\n</head>\n<body>\n\t$0\n</body>\n</html>".into());
    }
    let chars: Vec<char> = abbr.chars().collect();
    let mut p = Parser { c: &chars, i: 0, depth: 0, budget: MAX_NODES };
    let (nodes, _) = p.level(None)?;
    if p.i != chars.len() || nodes.is_empty() {
        return None;
    }
    let mut r = Renderer { stops: 0, jsx };
    let mut out = String::new();
    for (k, n) in nodes.iter().enumerate() {
        if k > 0 {
            out.push('\n');
        }
        r.render(n, 0, 1, None, &mut out);
    }
    // With no placeholders at all, the cursor goes to the end.
    out.push_str("$0");
    (out.len() <= MAX_OUTPUT).then_some(out)
}

#[derive(Clone, Debug, Default)]
struct Node {
    name: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, Option<String>)>,
    text: Option<String>,
    children: Vec<Node>,
    /// Index (1-based) and total of this copy of the `*n` multiplier; `$` is counted accordingly.
    index: Option<(usize, usize)>,
}

struct Parser<'a> {
    c: &'a [char],
    i: usize,
    depth: usize,
    /// Maximum number of nodes that can be produced (multiplier and child copies count towards this).
    budget: usize,
}

/// Node limit and output limit for what an abbreviation may produce (memory/CPU protection).
const MAX_NODES: usize = 2_000;
const MAX_OUTPUT: usize = 256 * 1024;

fn count(n: &Node) -> usize {
    1 + n.children.iter().map(count).sum::<usize>()
}

/// Whether this is a known HTML tag (avoids wrong expansion in code files such as JSX/PHP/Vue).
pub fn is_known_tag(name: &str) -> bool {
    const TAGS: &[&str] = &[
        "div",
        "span",
        "p",
        "a",
        "ul",
        "ol",
        "li",
        "img",
        "table",
        "tr",
        "td",
        "th",
        "form",
        "input",
        "button",
        "label",
        "select",
        "option",
        "section",
        "header",
        "footer",
        "nav",
        "main",
        "article",
        "aside",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "pre",
        "code",
        "em",
        "strong",
        "b",
        "i",
        "br",
        "hr",
        "textarea",
        "script",
        "style",
        "link",
        "meta",
        "title",
        "head",
        "body",
        "html",
        "thead",
        "tbody",
        "small",
        "figure",
        "figcaption",
        "video",
        "audio",
        "canvas",
        "svg",
        "iframe",
        "dl",
        "dt",
        "dd",
        "blockquote",
        "details",
        "summary",
    ];
    TAGS.contains(&name)
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    /// One level: siblings joined by `+`; `>` children; `^` climbs one level up.
    /// Returns: the nodes and the number of pending `^` to report to the caller (0: none).
    /// `close`: this level ends with `)`.
    fn level(&mut self, close: Option<char>) -> Option<(Vec<Node>, usize)> {
        self.depth += 1;
        if self.depth > 24 {
            return None;
        }
        let result = self.level_inner(close);
        self.depth -= 1;
        result
    }

    fn level_inner(&mut self, close: Option<char>) -> Option<(Vec<Node>, usize)> {
        let mut nodes: Vec<Node> = Vec::new();
        loop {
            let items = self.item()?;
            let start = nodes.len();
            nodes.extend(items);
            match self.peek() {
                Some('+') => self.i += 1,
                Some('>') => {
                    self.i += 1;
                    let (kids, up) = self.level(close)?;
                    let cost = kids.iter().map(count).sum::<usize>().checked_mul(nodes.len() - start)?;
                    self.budget = self.budget.checked_sub(cost)?;
                    for node in &mut nodes[start..] {
                        node.children.extend(kids.clone());
                    }
                    match up {
                        0 => return Some((nodes, 0)),
                        // More climbing needed: hand off to the parent level.
                        n if n > 1 => return Some((nodes, n - 1)),
                        // Climbed exactly to this level: a sibling may follow (`+` is optional).
                        _ => match self.peek() {
                            None => return Some((nodes, 0)),
                            Some(c) if Some(c) == close => return Some((nodes, 0)),
                            Some('+') => self.i += 1,
                            Some(_) => {}
                        },
                    }
                }
                Some('^') => {
                    let mut n = 0;
                    while self.peek() == Some('^') {
                        self.i += 1;
                        n += 1;
                    }
                    return Some((nodes, n));
                }
                _ => return Some((nodes, 0)),
            }
        }
    }

    /// A single item or a `( … )` group, followed by `*n`. Returns the list of multiplier copies.
    fn item(&mut self) -> Option<Vec<Node>> {
        let mut base: Vec<Node> = if self.peek() == Some('(') {
            self.i += 1;
            let (inner, _) = self.level(Some(')'))?;
            if self.peek() != Some(')') {
                return None;
            }
            self.i += 1;
            inner
        } else {
            vec![self.element()?]
        };
        if self.peek() == Some('*') {
            self.i += 1;
            let start = self.i;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
            }
            let n: usize = self.c[start..self.i].iter().collect::<String>().parse().ok().filter(|n| (1..=200).contains(n))?;
            let cost = base.iter().map(count).sum::<usize>().checked_mul(n)?;
            self.budget = self.budget.checked_sub(cost)?;
            let copy = base.clone();
            base = Vec::new();
            for k in 1..=n {
                for mut node in copy.clone() {
                    node.index = Some((k, n));
                    base.push(node);
                }
            }
        }
        Some(base)
    }

    fn word(&mut self) -> String {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '$' | '@')) {
            self.i += 1;
        }
        self.c[start..self.i].iter().collect()
    }

    fn element(&mut self) -> Option<Node> {
        self.budget = self.budget.checked_sub(1)?;
        let mut n = Node::default();
        let w = self.word();
        if !w.is_empty() {
            n.name = Some(w);
        }
        let mut any = n.name.is_some();
        loop {
            match self.peek() {
                Some('#') => {
                    self.i += 1;
                    let w = self.word();
                    if w.is_empty() {
                        return None;
                    }
                    n.id = Some(w);
                }
                Some('.') => {
                    self.i += 1;
                    let w = self.word();
                    if w.is_empty() {
                        return None;
                    }
                    n.classes.push(w);
                }
                Some('[') => {
                    self.i += 1;
                    self.attrs(&mut n)?;
                }
                Some('{') => {
                    self.i += 1;
                    let start = self.i;
                    let mut depth = 1;
                    while let Some(c) = self.peek() {
                        match c {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            _ => {}
                        }
                        self.i += 1;
                    }
                    if self.peek() != Some('}') {
                        return None;
                    }
                    n.text = Some(self.c[start..self.i].iter().collect());
                    self.i += 1;
                }
                _ => break,
            }
            any = true;
        }
        any.then_some(n)
    }

    /// Contents of `[a=b c="d e" f]`; the `]` is consumed.
    fn attrs(&mut self, n: &mut Node) -> Option<()> {
        loop {
            while self.peek() == Some(' ') {
                self.i += 1;
            }
            match self.peek()? {
                ']' => {
                    self.i += 1;
                    return Some(());
                }
                _ => {
                    let start = self.i;
                    while self.peek().is_some_and(|c| !matches!(c, '=' | ' ' | ']')) {
                        self.i += 1;
                    }
                    let name: String = self.c[start..self.i].iter().collect();
                    if name.is_empty() {
                        return None;
                    }
                    let value = if self.peek() == Some('=') {
                        self.i += 1;
                        Some(match self.peek()? {
                            q @ ('"' | '\'') => {
                                self.i += 1;
                                let s = self.i;
                                while self.peek()? != q {
                                    self.i += 1;
                                }
                                let v: String = self.c[s..self.i].iter().collect();
                                self.i += 1;
                                v
                            }
                            _ => {
                                let s = self.i;
                                while self.peek().is_some_and(|c| !matches!(c, ' ' | ']')) {
                                    self.i += 1;
                                }
                                self.c[s..self.i].iter().collect()
                            }
                        })
                    } else {
                        None
                    };
                    n.attrs.push((name, value));
                }
            }
        }
    }
}

const VOID: [&str; 14] = ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
const INLINE: [&str; 25] = [
    "a", "abbr", "acronym", "b", "bdo", "big", "br", "button", "cite", "code", "dfn", "em", "i", "img", "input", "kbd", "label", "map", "object", "q", "samp",
    "select", "small", "span", "strong",
];

struct Renderer {
    stops: usize,
    jsx: bool,
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '$' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `$` (and `$$` padding) → sequence number; `escape` is applied afterwards for escaped fields.
fn number(s: &str, idx: Option<(usize, usize)>) -> String {
    let Some((k, _)) = idx else { return s.to_string() };
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let mut j = i;
            while j < chars.len() && chars[j] == '$' {
                j += 1;
            }
            out.push_str(&format!("{k:0width$}", width = j - i));
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

impl Renderer {
    fn stop(&mut self) -> String {
        self.stops += 1;
        format!("${}", self.stops)
    }

    fn render(&mut self, n: &Node, level: usize, _count: usize, parent: Option<&str>, out: &mut String) {
        let idx = n.index;
        let name = match &n.name {
            Some(w) => number(w, idx),
            None => match parent {
                Some("ul" | "ol") => "li".into(),
                Some("table" | "tbody" | "thead") => "tr".into(),
                Some("tr") => "td".into(),
                Some("select") => "option".into(),
                Some(p) if INLINE.contains(&p) => "span".into(),
                _ => "div".into(),
            },
        };
        let attr = |k: &str, v: &str| format!(" {}=\"{}\"", escape(k), escape(v));
        let name = escape(&name);
        let mut open = format!("<{name}");
        if let Some(id) = &n.id {
            open.push_str(&attr("id", &number(id, idx)));
        }
        if !n.classes.is_empty() {
            let classes: Vec<String> = n.classes.iter().map(|c| number(c, idx)).collect();
            open.push_str(&attr(if self.jsx { "className" } else { "class" }, &classes.join(" ")));
        }
        let mut given: Vec<&str> = Vec::new();
        for (k, v) in &n.attrs {
            given.push(k);
            match v {
                Some(v) => open.push_str(&attr(k, &number(v, idx))),
                None => open.push_str(&format!(" {}=\"{}\"", escape(k), self.stop())),
            }
        }
        // Default attributes of the tag (empty placeholder if the user gave none).
        let defaults: &[(&str, &str)] = match name.as_str() {
            "a" => &[("href", "")],
            "img" => &[("src", ""), ("alt", "")],
            "link" => &[("rel", "stylesheet"), ("href", "")],
            "script" => &[],
            "input" => &[("type", "text")],
            "form" => &[("action", "")],
            "label" => &[("for", "")],
            _ => &[],
        };
        for (k, v) in defaults {
            if !given.contains(k) {
                if v.is_empty() {
                    open.push_str(&format!(" {k}=\"{}\"", self.stop()));
                } else {
                    open.push_str(&format!(" {k}=\"{v}\""));
                }
            }
        }
        open.push('>');
        let pad = "\t".repeat(level);
        let text = n.text.as_deref().map(|t| escape(&number(t, idx)));
        if VOID.contains(&name.as_str()) {
            out.push_str(&pad);
            out.push_str(&open);
            return;
        }
        let close = format!("</{name}>");
        out.push_str(&pad);
        if n.children.is_empty() {
            out.push_str(&open);
            match text {
                Some(t) => out.push_str(&t),
                None => out.push_str(&self.stop()),
            }
            out.push_str(&close);
            return;
        }
        // With children: only inline children fit on a single line; otherwise they go on subsequent lines.
        let inline_only = n.children.iter().all(|c| c.children.is_empty() && c.name.as_deref().is_some_and(|x| INLINE.contains(&x)) && text.is_none());
        out.push_str(&open);
        if let Some(t) = &text {
            out.push_str(t);
        }
        let _ = inline_only;
        for child in &n.children {
            out.push('\n');
            self.render(child, level + 1, 1, Some(&name), out);
        }
        out.push('\n');
        out.push_str(&pad);
        out.push_str(&close);
    }
}

// --- CSS ---

const CSS: &[(&str, &str)] = &[
    ("m", "margin"),
    ("mt", "margin-top"),
    ("mr", "margin-right"),
    ("mb", "margin-bottom"),
    ("ml", "margin-left"),
    ("p", "padding"),
    ("pt", "padding-top"),
    ("pr", "padding-right"),
    ("pb", "padding-bottom"),
    ("pl", "padding-left"),
    ("w", "width"),
    ("h", "height"),
    ("maw", "max-width"),
    ("mah", "max-height"),
    ("miw", "min-width"),
    ("mih", "min-height"),
    ("t", "top"),
    ("r", "right"),
    ("b", "bottom"),
    ("l", "left"),
    ("z", "z-index"),
    ("fz", "font-size"),
    ("fw", "font-weight"),
    ("lh", "line-height"),
    ("ta", "text-align"),
    ("td", "text-decoration"),
    ("bd", "border"),
    ("bdrs", "border-radius"),
    ("bg", "background"),
    ("bgc", "background-color"),
    ("c", "color"),
    ("d", "display"),
    ("pos", "position"),
    ("op", "opacity"),
    ("ov", "overflow"),
    ("fl", "float"),
    ("cur", "cursor"),
    ("jc", "justify-content"),
    ("ai", "align-items"),
    ("fd", "flex-direction"),
    ("fx", "flex"),
    ("gap", "gap"),
];

/// Keyword abbreviations (`df` → `display: flex`).
const CSS_KEYWORDS: &[(&str, &str)] = &[
    ("dn", "display: none"),
    ("db", "display: block"),
    ("dib", "display: inline-block"),
    ("di", "display: inline"),
    ("df", "display: flex"),
    ("dg", "display: grid"),
    ("posa", "position: absolute"),
    ("posr", "position: relative"),
    ("posf", "position: fixed"),
    ("poss", "position: sticky"),
    ("fwb", "font-weight: bold"),
    ("tac", "text-align: center"),
    ("tal", "text-align: left"),
    ("tar", "text-align: right"),
    ("jcc", "justify-content: center"),
    ("aic", "align-items: center"),
    ("jcsb", "justify-content: space-between"),
    ("fdc", "flex-direction: column"),
    ("curp", "cursor: pointer"),
    ("ovh", "overflow: hidden"),
    ("tdn", "text-decoration: none"),
    ("bxz", "box-sizing: border-box"),
];

/// CSS abbreviation: `m10` → `margin: 10px;`, `w50p` → `width: 50%;`, `df` → `display: flex;`.
pub fn expand_css(abbr: &str) -> Option<String> {
    let abbr = abbr.trim();
    if abbr.is_empty() || abbr.contains(' ') {
        return None;
    }
    if let Some((_, v)) = CSS_KEYWORDS.iter().find(|(k, _)| *k == abbr) {
        return Some(format!("{v};$0"));
    }
    // Property abbreviation + value(s): "m10-20" → "margin: 10px 20px"
    let split = abbr.find(|c: char| c.is_ascii_digit() || c == '-' || c == '.').unwrap_or(abbr.len());
    let (key, rest) = abbr.split_at(split);
    let (_, prop) = CSS.iter().find(|(k, _)| *k == key)?;
    if rest.is_empty() {
        return Some(format!("{prop}: $1;$0"));
    }
    let unitless = matches!(*prop, "font-weight" | "z-index" | "opacity" | "flex" | "line-height");
    let values: Option<Vec<String>> = rest
        .split('-')
        .map(|v| {
            let (num, unit) = v.split_at(v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len()));
            if num.is_empty() {
                return None;
            }
            Some(match unit {
                "" if unitless || num == "0" => num.to_string(),
                "" => format!("{num}px"),
                "p" => format!("{num}%"),
                "e" => format!("{num}em"),
                "r" => format!("{num}rem"),
                "x" => format!("{num}ex"),
                u => format!("{num}{u}"),
            })
        })
        .collect();
    Some(format!("{prop}: {};$0", values?.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(a: &str) -> String {
        expand_html(a, false).unwrap()
    }

    #[test]
    fn simple_elements_attributes_and_text() {
        assert_eq!(h("div"), "<div>$1</div>$0");
        assert_eq!(h("p.intro#top{Hello}"), "<p id=\"top\" class=\"intro\">Hello</p>$0");
        assert_eq!(h(".box"), "<div class=\"box\">$1</div>$0");
        assert_eq!(h("a[href=https://x.y title=\"a b\"]{go}"), "<a href=\"https://x.y\" title=\"a b\">go</a>$0");
        assert_eq!(h("a"), "<a href=\"$1\">$2</a>$0", "default href placeholder");
        assert_eq!(h("img"), "<img src=\"$1\" alt=\"$2\">$0");
        assert_eq!(h("br"), "<br>$0");
        assert_eq!(h("input[type=email]"), "<input type=\"email\">$0");
        assert_eq!(h("a.btn.big"), "<a class=\"btn big\" href=\"$1\">$2</a>$0");
    }

    #[test]
    fn nesting_siblings_climbing_and_groups() {
        assert_eq!(h("ul>li"), "<ul>\n\t<li>$1</li>\n</ul>$0");
        assert_eq!(h("div+p"), "<div>$1</div>\n<p>$2</p>$0");
        assert_eq!(h("div>p^span"), "<div>\n\t<p>$1</p>\n</div>\n<span>$2</span>$0");
        assert_eq!(h("div>(h1+p)+footer"), "<div>\n\t<h1>$1</h1>\n\t<p>$2</p>\n\t<footer>$3</footer>\n</div>$0");
        assert_eq!(h("table>tr>td"), "<table>\n\t<tr>\n\t\t<td>$1</td>\n\t</tr>\n</table>$0");
        assert_eq!(h("ul>.item"), "<ul>\n\t<li class=\"item\">$1</li>\n</ul>$0", "implicit li under ul");
        assert_eq!(h("span>.x"), "<span>\n\t<span class=\"x\">$1</span>\n</span>$0", "implicit span under an inline element");
    }

    #[test]
    fn multiplication_and_numbering() {
        assert_eq!(h("li{item $}*2"), "<li>item 1</li>\n<li>item 2</li>$0");
        assert_eq!(h("ul>li.row$*3"), "<ul>\n\t<li class=\"row1\">$1</li>\n\t<li class=\"row2\">$2</li>\n\t<li class=\"row3\">$3</li>\n</ul>$0");
        assert_eq!(h("p{$$}*2"), "<p>01</p>\n<p>02</p>$0");
        assert_eq!(h("(p+span)*2").matches("<p>").count(), 2);
        assert!(expand_html("li*0", false).is_none() && expand_html("li*999", false).is_none());
    }

    #[test]
    fn doctype_jsx_and_invalid_input() {
        assert!(h("!").starts_with("<!DOCTYPE html>") && h("!").contains("${1:en}"));
        assert_eq!(expand_html(".a", true).unwrap(), "<div className=\"a\">$1</div>$0");
        for bad in ["", "div>", ">div", "div[", "(div", "div)", "div{x", "#", ".", "div*", "a b"] {
            assert!(expand_html(bad, false).is_none() || bad == "div>" && false, "{bad:?} → {:?}", expand_html(bad, false));
        }
    }

    #[test]
    fn text_is_escaped_for_snippets() {
        assert_eq!(h("p{cost $5}"), "<p>cost \\$5</p>$0");
        assert_eq!(h("p{a\\b}"), "<p>a\\\\b</p>$0");
    }

    #[test]
    fn css_abbreviations() {
        assert_eq!(expand_css("m10").unwrap(), "margin: 10px;$0");
        assert_eq!(expand_css("m10-20").unwrap(), "margin: 10px 20px;$0");
        assert_eq!(expand_css("w50p").unwrap(), "width: 50%;$0");
        assert_eq!(expand_css("fz1.5r").unwrap(), "font-size: 1.5rem;$0");
        assert_eq!(expand_css("fw700").unwrap(), "font-weight: 700;$0");
        assert_eq!(expand_css("m0").unwrap(), "margin: 0;$0");
        assert_eq!(expand_css("df").unwrap(), "display: flex;$0");
        assert_eq!(expand_css("c").unwrap(), "color: $1;$0");
        assert!(expand_css("zzz10").is_none() && expand_css("").is_none() && expand_css("m x").is_none());
    }

    #[test]
    fn fuzz_never_panics() {
        let alphabet: Vec<char> = "ab.#>+^*()[]{}=\"' $-1".chars().collect();
        let mut seed = 0x9e3779b97f4a7c15u64;
        for _ in 0..4000 {
            let mut s = String::new();
            for _ in 0..(seed % 14) {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                s.push(alphabet[(seed >> 33) as usize % alphabet.len()]);
            }
            let _ = expand_html(&s, false);
            let _ = expand_css(&s);
        }
    }

    #[test]
    fn blow_up_abbreviations_are_refused_quickly() {
        let t = std::time::Instant::now();
        for bad in [
            "div*200>div*200>div*200",
            "((a*200)*200)*200",
            "div*200>div*200>div*200>div*200",
            "(a*50)*50",
            "a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a>a",
        ] {
            assert!(expand_html(bad, false).is_none(), "{bad}");
        }
        // A reasonable size still works.
        assert!(expand_html("ul>li*20>a", false).is_some());
        assert!(t.elapsed().as_millis() < 500, "{:?}", t.elapsed());
    }

    #[test]
    fn names_are_escaped_and_known_tags_listed() {
        assert!(!expand_html("x[a$b=1]", false).unwrap().contains("a$b"), "$ in an attribute name is escaped");
        assert!(is_known_tag("div") && !is_known_tag("this") && !is_known_tag("console") && !is_known_tag("props"));
    }
}
