//! HTML to Markdown: headings, paragraphs, lists, tables, links, emphasis, code and quotes
//! kept; scripts, styles, forms and navigation chrome (`nav`, a site's `header` and `footer`,
//! `aside`, hidden elements) dropped. When the page marks its main content (`main`, or a single
//! `article`), only that is read. The page's `<title>` heads it when its content has no `h1`.
//! Every link's address is also returned, chrome included, for the acquisition crawl.

use ego_tree::NodeRef;
use scraper::{Html, Node};

use super::md::{self, Md};
use super::{Limits, Refused};

type N<'a> = NodeRef<'a, Node>;

/// Deeper than this, an element's content is read as plain text.
const MAX_DEPTH: usize = 200;

pub struct Page {
    pub markdown: String,
    pub links: Vec<String>,
}

/// Nesting past which a page isn't parsed at all: the parser's work grows with the square of
/// the depth (tens of thousands of nested lists take minutes), and it runs before any deadline
/// can be checked.
const MAX_SOURCE_DEPTH: usize = 2000;

/// How deep the page's tags nest, roughly, from its source: start tags in, end tags out,
/// leaving out void elements and those whose end tag may be omitted (an old page's unclosed
/// `<li>` and `<p>` don't count), and comments, scripts and styles.
fn source_depth(text: &str) -> usize {
    const VOID: [&str; 14] = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    const OPTIONAL_END: [&str; 15] = [
        "li", "p", "dt", "dd", "option", "optgroup", "tr", "td", "th", "tbody", "thead", "tfoot",
        "colgroup", "rt", "rp",
    ];
    let b = text.as_bytes();
    let (mut depth, mut deepest, mut i) = (0usize, 0usize, 0usize);
    let skip_to = |from: usize, end: &str| -> usize {
        text[from..]
            .find(end)
            .map_or(b.len(), |k| from + k + end.len())
    };
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if text[i..].starts_with("<!--") {
            i = skip_to(i, "-->");
            continue;
        }
        let closing = b.get(i + 1) == Some(&b'/');
        let start = i + 1 + usize::from(closing);
        let name_end = b[start..]
            .iter()
            .position(|c| !c.is_ascii_alphanumeric())
            .map_or(b.len(), |k| start + k);
        if name_end == start {
            i += 1;
            continue;
        }
        let name = text[start..name_end].to_ascii_lowercase();
        let tag_end = text[name_end..]
            .find('>')
            .map_or(b.len(), |k| name_end + k + 1);
        let self_closing = text[name_end..tag_end].trim_end_matches('>').ends_with('/');
        i = tag_end;
        if VOID.contains(&name.as_str()) || OPTIONAL_END.contains(&name.as_str()) {
            continue;
        }
        if closing {
            depth = depth.saturating_sub(1);
        } else if !self_closing {
            if name == "script" || name == "style" {
                i = skip_to(i, &format!("</{name}"));
                continue;
            }
            depth += 1;
            deepest = deepest.max(depth);
        }
    }
    deepest
}

pub fn convert(bytes: &[u8], limits: &Limits) -> Result<Page, Refused> {
    let text = decode(bytes);
    if source_depth(&text) > MAX_SOURCE_DEPTH {
        return Err(Refused::TooLarge(format!(
            "the page nests deeper than {MAX_SOURCE_DEPTH} levels"
        )));
    }
    let doc = Html::parse_document(&text);
    let root = *doc.root_element();
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for n in root.descendants() {
        if let Some(href) = element(n)
            .filter(|e| e.name() == "a")
            .and_then(|e| e.attr("href"))
            .map(str::trim)
            .filter(|h| link_allowed(h))
            && seen.insert(href)
        {
            links.push(href.to_owned());
        }
    }
    let title = root
        .descendants()
        .find(|n| element(*n).is_some_and(|e| e.name() == "title"))
        .map(|n| {
            let text: String = n
                .children()
                .filter_map(|c| match c.value() {
                    Node::Text(t) => Some(t.to_string()),
                    _ => None,
                })
                .collect();
            md::collapse(&text)
        })
        .unwrap_or_default();
    let main = find(root, |n| {
        element(n).is_some_and(|e| e.name() == "main" || e.attr("role") == Some("main"))
    });
    let articles: Vec<N<'_>> = root
        .descendants()
        .filter(|n| element(*n).is_some_and(|e| e.name() == "article"))
        .collect();
    let content = main
        .or_else(|| (articles.len() == 1).then(|| articles[0]))
        .or_else(|| find(root, |n| element(n).is_some_and(|e| e.name() == "body")))
        .unwrap_or(root);
    let mut w = Writer {
        md: Md::new(),
        line: String::new(),
        limits,
        has_h1: false,
    };
    w.blocks(content, 0)?;
    w.flush();
    let body = w.md.finish();
    let markdown = if !w.has_h1 && !title.is_empty() {
        format!("# {title}\n\n{body}")
    } else {
        body
    };
    Ok(Page {
        markdown: markdown.trim_end().to_owned() + "\n",
        links,
    })
}

fn link_allowed(href: &str) -> bool {
    let lower = href.to_ascii_lowercase();
    !href.is_empty()
        && !href.starts_with('#')
        && !lower.starts_with("javascript:")
        && !lower.starts_with("data:")
        && !lower.starts_with("vbscript:")
}

/// The page's text: its byte order mark, else the charset its `<meta>` names, else UTF-8 when
/// it is valid, else GB18030 (Chinese sites that don't say).
fn decode(bytes: &[u8]) -> String {
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode_with_bom_removal(bytes).0.into_owned();
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    if let Some(at) = head.find("charset=") {
        let label: String = head[at + 8..]
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) {
            return enc.decode_without_bom_handling(bytes).0.into_owned();
        }
    }
    super::decode_text(bytes)
}

fn element(n: N<'_>) -> Option<&scraper::node::Element> {
    match n.value() {
        Node::Element(e) => Some(e),
        _ => None,
    }
}

fn find<'a>(root: N<'a>, pred: impl Fn(N<'a>) -> bool) -> Option<N<'a>> {
    root.descendants().find(|n| pred(*n))
}

/// All the text below a node, iteratively.
fn text_of(n: N<'_>) -> String {
    // A walk with its own stack that never enters a skipped element: linear in the nodes, and
    // no recursion however deep the page nests.
    let mut out = String::new();
    let mut stack: Vec<N<'_>> = n.children().collect();
    stack.reverse();
    while let Some(d) = stack.pop() {
        match d.value() {
            Node::Text(t) => out.push_str(t),
            Node::Element(e) if skip_always(e) => {}
            Node::Element(_) => {
                let start = stack.len();
                stack.extend(d.children());
                stack[start..].reverse();
            }
            _ => {}
        }
    }
    out
}

/// Elements never read, wherever they are.
fn skip_always(e: &scraper::node::Element) -> bool {
    matches!(
        e.name(),
        "script"
            | "style"
            | "noscript"
            | "template"
            | "iframe"
            | "object"
            | "embed"
            | "svg"
            | "canvas"
            | "head"
            | "input"
            | "select"
            | "option"
            | "button"
            | "textarea"
            | "dialog"
    ) || e.attr("hidden").is_some()
        || e.attr("aria-hidden") == Some("true")
        || e.attr("style").is_some_and(|s| {
            let s = s.replace(' ', "").to_ascii_lowercase();
            s.contains("display:none") || s.contains("visibility:hidden")
        })
}

/// Navigation chrome: menus, the site's banner and footer, side panels.
fn chrome(n: N<'_>, e: &scraper::node::Element) -> bool {
    let role = e.attr("role").unwrap_or_default();
    if matches!(
        role,
        "navigation" | "banner" | "contentinfo" | "search" | "menu"
    ) {
        return true;
    }
    match e.name() {
        "nav" | "aside" => true,
        // An article's own header or footer (its title, its date) is content.
        "header" | "footer" => !n.ancestors().any(|a| {
            element(a).is_some_and(|p| matches!(p.name(), "article" | "main" | "section"))
        }),
        _ => false,
    }
}

struct Writer<'l> {
    md: Md,
    /// Inline content not yet written as a paragraph.
    line: String,
    limits: &'l Limits,
    has_h1: bool,
}

impl Writer<'_> {
    fn flush(&mut self) {
        let lines: Vec<String> = self
            .line
            .split('\n')
            .map(md::collapse)
            .filter(|l| !l.is_empty())
            .collect();
        self.line.clear();
        if !lines.is_empty() {
            self.md.paragraph(&md::broken_lines(&lines));
        }
    }

    fn blocks(&mut self, parent: N<'_>, depth: usize) -> Result<(), Refused> {
        self.limits.late()?;
        if self.md.len() > self.limits.markdown_bytes {
            return Err(Refused::TooLarge(format!(
                "the text is longer than {} MB",
                self.limits.markdown_bytes >> 20
            )));
        }
        if depth > MAX_DEPTH {
            self.line.push_str(&text_of(parent));
            return Ok(());
        }
        for n in parent.children() {
            match n.value() {
                Node::Text(t) => self.line.push_str(t),
                Node::Element(e) => {
                    if skip_always(e) || chrome(n, e) {
                        continue;
                    }
                    match e.name() {
                        h @ ("h1" | "h2" | "h3" | "h4" | "h5" | "h6") => {
                            self.flush();
                            let level = (h.as_bytes()[1] - b'0') as usize;
                            let text = md::collapse(&strip_marks(&inline(n, depth)));
                            if level == 1 && !text.is_empty() {
                                self.has_h1 = true;
                            }
                            self.md.heading(level, &text);
                        }
                        "p" => {
                            self.flush();
                            self.line = inline(n, depth);
                            self.flush();
                        }
                        "ul" | "ol" | "menu" => {
                            self.flush();
                            self.list(n, 0, e.name() == "ol", depth)?;
                            self.md.end_list();
                        }
                        "table" => {
                            self.flush();
                            let rows = table(n, depth);
                            self.md.table(&rows);
                        }
                        "pre" => {
                            self.flush();
                            self.md.code(&text_of(n));
                        }
                        "blockquote" => {
                            self.flush();
                            let mut inner = Writer {
                                md: Md::new(),
                                line: String::new(),
                                limits: self.limits,
                                has_h1: true,
                            };
                            inner.blocks(n, depth + 1)?;
                            inner.flush();
                            self.md.quote(&inner.md.finish());
                        }
                        "hr" => {
                            self.flush();
                            self.md.rule();
                        }
                        "br" => self.line.push('\n'),
                        "dt" => {
                            self.flush();
                            let t = md::collapse(&inline(n, depth));
                            if !t.is_empty() {
                                self.md.paragraph(&format!("**{}**", strip_marks(&t)));
                            }
                        }
                        "div" | "section" | "article" | "main" | "header" | "footer" | "body"
                        | "html" | "figure" | "figcaption" | "dl" | "dd" | "address"
                        | "details" | "summary" | "form" | "fieldset" | "legend" | "center"
                        | "hgroup" | "li" | "caption" => {
                            self.flush();
                            self.blocks(n, depth + 1)?;
                            self.flush();
                        }
                        _ => self.line.push_str(&inline(n, depth)),
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn list(
        &mut self,
        list: N<'_>,
        level: usize,
        ordered: bool,
        depth: usize,
    ) -> Result<(), Refused> {
        // Lists nested past the limit are read as text, as other blocks are: no recursion
        // however deep a page nests them.
        if depth > MAX_DEPTH {
            self.md.paragraph(&md::collapse(&text_of(list)));
            return Ok(());
        }
        self.limits.late()?;
        let mut number = 0;
        for li in list.children() {
            let Some(e) = element(li) else {
                continue;
            };
            if skip_always(e) {
                continue;
            }
            if e.name() != "li" {
                // A list nested straight in a list.
                if matches!(e.name(), "ul" | "ol") {
                    self.list(li, level + 1, e.name() == "ol", depth + 1)?;
                }
                continue;
            }
            number += 1;
            let mut text = String::new();
            let mut nested = Vec::new();
            for c in li.children() {
                match element(c) {
                    Some(ce) if matches!(ce.name(), "ul" | "ol") => nested.push(c),
                    Some(ce) if skip_always(ce) => {}
                    Some(ce) if is_block(ce.name()) => {
                        text.push(' ');
                        text.push_str(&inline(c, depth + 1));
                    }
                    _ => text.push_str(&inline_node(c, depth + 1)),
                }
            }
            self.md
                .item(level, ordered.then_some(number), &md::collapse(&text));
            for n in nested {
                let ordered = element(n).is_some_and(|e| e.name() == "ol");
                self.list(n, level + 1, ordered, depth + 1)?;
            }
        }
        Ok(())
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "table"
            | "pre"
            | "blockquote"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "dl"
            | "figure"
    )
}

/// Emphasis marks inside a heading add nothing.
fn strip_marks(s: &str) -> String {
    s.replace(['*', '`'], "")
}

/// The inline Markdown of a node's children.
fn inline(n: N<'_>, depth: usize) -> String {
    if depth > MAX_DEPTH {
        return text_of(n);
    }
    n.children().map(|c| inline_node(c, depth + 1)).collect()
}

fn inline_node(n: N<'_>, depth: usize) -> String {
    let e = match n.value() {
        Node::Text(t) => return t.to_string(),
        Node::Element(e) => e,
        _ => return String::new(),
    };
    if skip_always(e) || chrome(n, e) {
        return String::new();
    }
    let wrap = |mark: &str| {
        let inner = inline(n, depth);
        let core = md::collapse(&inner);
        if core.is_empty() {
            return inner;
        }
        let lead = if inner.starts_with(char::is_whitespace) {
            " "
        } else {
            ""
        };
        let trail = if inner.ends_with(char::is_whitespace) {
            " "
        } else {
            ""
        };
        format!("{lead}{mark}{core}{mark}{trail}")
    };
    match e.name() {
        "br" => "\n".into(),
        "strong" | "b" => wrap("**"),
        "em" | "i" => wrap("*"),
        "code" | "kbd" | "samp" => {
            let t = md::collapse(&text_of(n));
            if t.is_empty() {
                t
            } else {
                format!("`{}`", t.replace('`', "'"))
            }
        }
        "a" => {
            let text = md::collapse(&inline(n, depth));
            match e.attr("href").map(str::trim).filter(|h| link_allowed(h)) {
                Some(href) if !text.is_empty() => {
                    format!(
                        "[{}]({})",
                        text.replace(['[', ']'], ""),
                        href.replace(' ', "%20")
                    )
                }
                _ => text,
            }
        }
        "img" => e
            .attr("alt")
            .map(md::collapse)
            .filter(|a| !a.is_empty())
            .map(|a| format!(" {a} "))
            .unwrap_or_default(),
        // Cells and list items met inline are spaced apart.
        "td" | "th" | "li" | "div" | "p" => format!(" {} ", inline(n, depth)),
        _ => inline(n, depth),
    }
}

/// A table's rows (its own, not those of tables inside it), cells as inline text, a cell
/// spanning columns followed by empty ones.
fn table(t: N<'_>, depth: usize) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut groups = vec![t];
    while let Some(g) = groups.pop() {
        for c in g.children() {
            match element(c).map(|e| e.name()) {
                Some("thead" | "tbody" | "tfoot") => groups.insert(0, c),
                Some("tr") => {
                    let mut row = Vec::new();
                    for cell in c.children() {
                        let Some(ce) = element(cell).filter(|e| matches!(e.name(), "td" | "th"))
                        else {
                            continue;
                        };
                        row.push(md::collapse(&inline(cell, depth)));
                        let span: usize = ce
                            .attr("colspan")
                            .and_then(|s| s.trim().parse().ok())
                            .unwrap_or(1);
                        for _ in 1..span.clamp(1, 64) {
                            row.push(String::new());
                        }
                    }
                    rows.push(row);
                }
                _ => {}
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_is_kept_and_chrome_dropped() {
        let html = r##"<!doctype html><html><head><title>Shipping FAQ</title>
<style>p{color:red}</style><script>alert("x")</script></head>
<body><header><a href="/">Home</a> <a href="/about">About</a></header>
<nav><ul><li><a href="/faq">FAQ</a></li></ul></nav>
<div class="content"><h2>Delivery <em>times</em></h2>
<p>Parcels arrive in <strong>two to four</strong> days. See <a href="/rates">rates</a>.</p>
<ul><li>Standard<ul><li>3 days</li></ul></li><li>Express</li></ul>
<ol><li>Order</li><li>Pay</li></ol>
<table><thead><tr><th>Zone</th><th>Days</th></tr></thead>
<tbody><tr><td>East</td><td>2</td></tr><tr><td colspan="2">Islands: ask</td></tr></tbody></table>
<p hidden>secret</p><p style="display: none">also secret</p>
<pre>code  here</pre><blockquote><p>Quoted</p></blockquote>
<p># not a heading</p></div>
<footer>© 2026 <a href="javascript:void(0)">x</a></footer></body></html>"##;
        let page = convert(html.as_bytes(), &Limits::default()).unwrap();
        assert_eq!(
            page.markdown,
            "# Shipping FAQ\n\n## Delivery times\n\nParcels arrive in **two to four** days. See [rates](/rates).\n\n- Standard\n  - 3 days\n- Express\n\n1. Order\n2. Pay\n\n| Zone | Days |\n| --- | --- |\n| East | 2 |\n| Islands: ask |  |\n\n```\ncode  here\n```\n\n> Quoted\n\n\\# not a heading\n"
        );
        assert_eq!(page.links, ["/", "/about", "/faq", "/rates"]);
    }

    #[test]
    fn the_main_content_alone_and_the_page_charset() {
        let html = "<html><head><meta charset=\"gbk\"><title>会员</title></head><body><div>菜单</div><main><h1>会员服务</h1><p>金卡每月两张免运费券。</p></main></body></html>";
        let (bytes, _, _) = encoding_rs::GBK.encode(html);
        let page = convert(&bytes, &Limits::default()).unwrap();
        assert_eq!(page.markdown, "# 会员服务\n\n金卡每月两张免运费券。\n");
    }

    #[test]
    fn deep_nesting_is_read_as_text() {
        let html = format!("{}deep{}", "<div>".repeat(1500), "</div>".repeat(1500));
        let page = convert(html.as_bytes(), &Limits::default()).unwrap();
        assert_eq!(page.markdown.trim(), "deep");
        // Lists too: nested past the writer's limit, read as text without recursing.
        let lists = format!(
            "{}<li>deep{}",
            "<ul><li>x".repeat(1500),
            "</li></ul>".repeat(1500)
        );
        let page = convert(lists.as_bytes(), &Limits::default()).unwrap();
        assert!(page.markdown.contains("deep"));
        // Nested deeper than a page ever is, it isn't parsed at all (the parser's work grows
        // with the square of the depth).
        let deeper = "<ul><li>x".repeat(50_000);
        let started = std::time::Instant::now();
        assert!(matches!(
            convert(deeper.as_bytes(), &Limits::default()),
            Err(Refused::TooLarge(_))
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // An old page's many unclosed items and paragraphs don't count as nesting.
        let unclosed = format!(
            "<ul>{}</ul><p>{}",
            "<li>item".repeat(10_000),
            "<p>para".repeat(5_000)
        );
        assert!(convert(unclosed.as_bytes(), &Limits::default()).is_ok());
    }
}
