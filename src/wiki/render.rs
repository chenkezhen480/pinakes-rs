//! A page's Markdown as HTML for the console (design, "M10": pages are asked for as HTML).
//! Pages are written by a model and may come from fetched sites, so nothing they hold may run:
//! raw HTML in the Markdown is shown as text, links keep only `http`, `https` and `mailto`
//! addresses (and the console's own routes), and images are dropped, so the HTML holds only the
//! tags the renderer itself writes, with every text and attribute escaped. Wiki links
//! (`[[expertise/returns]]`, `[[space:type/key|label]]`) become links to the console's pages.

use pulldown_cmark::{CowStr, Event, LinkType, Options, Parser, Tag, TagEnd, TextMergeStream};

/// Whether a link's address may be followed from the console.
fn allowed(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return true;
    }
    // Relative addresses and fragments; anything with a scheme (`javascript:`, `data:`) is not.
    let scheme_end = lower.find(':');
    let first_sep = lower.find(['/', '?', '#']);
    match (scheme_end, first_sep) {
        (Some(c), Some(s)) => s < c,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The console route of a wiki link's target: a node (`space:type/key`) or a page.
fn route(target: &str) -> String {
    let target = target.trim();
    // `page#section` names a section of a page; a node's key may hold a `#` of its own.
    let (path, _section) = if target.contains(':') {
        (target, "")
    } else {
        target.split_once('#').unwrap_or((target, ""))
    };
    let encoded: String = path
        .split('/')
        .map(|seg| {
            seg.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b':' => {
                        (b as char).to_string()
                    }
                    _ => format!("%{b:02X}"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/");
    if path.contains(':') {
        format!("#/graph/node/{encoded}")
    } else {
        format!("#/wiki/{encoded}")
    }
}

/// Text with its `[[target]]` and `[[target|label]]` links as HTML links.
fn wiki_links(text: &str, out: &mut Vec<Event<'static>>) {
    let mut rest = text;
    while let Some(open) = rest.find("[[") {
        let Some(close) = rest[open + 2..].find("]]") else {
            break;
        };
        let inner = &rest[open + 2..open + 2 + close];
        if inner.is_empty() || inner.contains('\n') || inner.contains('[') {
            out.push(Event::Text(CowStr::from(rest[..open + 2].to_owned())));
            rest = &rest[open + 2..];
            continue;
        }
        if open > 0 {
            out.push(Event::Text(CowStr::from(rest[..open].to_owned())));
        }
        let (target, label) = inner.split_once('|').unwrap_or((inner, inner));
        out.push(Event::Html(CowStr::from(format!(
            "<a class=\"wikilink\" href=\"{}\" data-target=\"{}\">{}</a>",
            escape(&route(target)),
            escape(target.trim()),
            escape(label.trim())
        ))));
        rest = &rest[open + 2 + close + 2..];
    }
    if !rest.is_empty() {
        out.push(Event::Text(CowStr::from(rest.to_owned())));
    }
}

/// The page body as safe HTML.
pub fn html(markdown: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;
    let parser = TextMergeStream::new(Parser::new_ext(markdown, options));
    let mut events: Vec<Event<'static>> = Vec::new();
    let mut in_code = false;
    // Inside an image: its alt text is shown as text, the image itself never loaded.
    let mut in_image = 0usize;
    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(k)) => {
                in_code = true;
                events.push(Event::Start(Tag::CodeBlock(k)).into_static());
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                events.push(Event::End(TagEnd::CodeBlock));
            }
            // Raw HTML is shown as the text it is.
            Event::Html(h) | Event::InlineHtml(h) => {
                events.push(Event::Text(CowStr::from(h.into_string())));
            }
            Event::Start(Tag::Image { .. }) => in_image += 1,
            Event::End(TagEnd::Image) => in_image = in_image.saturating_sub(1),
            // A link inside an image's text is text too (its end is dropped below with the
            // rest of the image's events, so its start must be as well).
            Event::Start(Tag::Link { .. }) if in_image > 0 => {}
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                // An e-mail autolink is written as `mailto:` by the renderer.
                let safe = link_type == LinkType::Email || allowed(&dest_url);
                let dest = if safe {
                    dest_url.into_string()
                } else {
                    String::from("#")
                };
                events.push(Event::Start(Tag::Link {
                    link_type,
                    dest_url: CowStr::from(dest),
                    title: CowStr::from(title.into_string()),
                    id: CowStr::from(id.into_string()),
                }));
            }
            Event::Text(t) if !in_code => wiki_links(&t, &mut events),
            other if in_image > 0 => {
                if let Event::Text(t) = other {
                    events.push(Event::Text(CowStr::from(t.into_string())));
                }
            }
            other => events.push(other.into_static()),
        }
    }
    let mut out = String::with_capacity(markdown.len() * 3 / 2);
    pulldown_cmark::html::push_html(&mut out, events.into_iter());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_tables_and_wiki_links() {
        let h = html(
            "# 退货\n\n见 [[expertise/returns|退货政策]] 与 [[commerce:order/1042]]。\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n`[[not a link]]`\n",
        );
        assert!(h.contains("<h1>退货</h1>"), "{h}");
        assert!(
            h.contains(r##"<a class="wikilink" href="#/wiki/expertise/returns" data-target="expertise/returns">退货政策</a>"##),
            "{h}"
        );
        assert!(
            h.contains(r##"href="#/graph/node/commerce:order/1042""##),
            "{h}"
        );
        assert!(h.contains("<table>"), "{h}");
        assert!(h.contains("<code>[[not a link]]</code>"), "{h}");
    }

    #[test]
    fn a_link_inside_an_image_leaves_no_open_anchor() {
        let h = html(
            "![see [docs](https://x.example)](https://img.example/a.png)

after",
        );
        assert!(!h.contains("<a"), "{h}");
        assert!(h.contains("docs") && h.contains("<p>after</p>"), "{h}");
    }

    #[test]
    fn links_carry_every_character_of_a_target() {
        // A node's key keeps `&`, `?` and `#`, encoded; a page's `#section` is cut off.
        assert_eq!(
            route("shop:order/A&B?#1"),
            "#/graph/node/shop:order/A%26B%3F%231"
        );
        assert_eq!(
            route("expertise/returns#refunds"),
            "#/wiki/expertise/returns"
        );
        assert_eq!(
            route("expertise/退货"),
            "#/wiki/expertise/%E9%80%80%E8%B4%A7"
        );
    }

    #[test]
    fn nothing_a_page_holds_can_run() {
        let attacks = [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "[x](javascript:alert(1))",
            "[x](JaVaScRiPt:alert(1))",
            "[x](data:text/html;base64,PHNjcmlwdD4=)",
            "![x](https://tracker.example/p.gif)",
            "<a href=\"javascript:alert(1)\">x</a>",
            "[[\"><script>alert(1)</script>]]",
            "<iframe src=\"https://evil.example\"></iframe>",
            "[x](vbscript:msgbox)",
        ];
        for a in attacks {
            let h = html(a);
            // Every tag is one the renderer writes, with no event attribute.
            for tag in h.split('<').skip(1) {
                let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
                let name: String = tag
                    .trim_start_matches('/')
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                assert!(
                    ["p", "a", "code", "pre", "em", "strong"].contains(&name.as_str()),
                    "{a}: <{tag}> in {h}"
                );
                let lower = tag.to_ascii_lowercase();
                assert!(!lower.contains(" on"), "{a}: {h}");
                for scheme in ["javascript", "data:", "vbscript"] {
                    assert!(!lower.contains(&format!("href=\"{scheme}")), "{a}: {h}");
                }
            }
        }
        // Safe links stay.
        assert!(
            html("[a](https://example.com/x?y=1)").contains(r#"href="https://example.com/x?y=1""#)
        );
        assert!(html("[a](/pages/x)").contains(r#"href="/pages/x""#));
    }
}
