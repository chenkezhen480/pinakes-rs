//! A small XML tree for the Office formats, read with limits: entities are never expanded
//! beyond XML's own five and character references (a document type declaration is skipped, so
//! no entity it defines is ever resolved and nothing outside the file is read), nesting and
//! element counts are capped, and names keep only their local part (`w:p` is `p`), which is
//! all the Office readers need.

use quick_xml::Reader;
use quick_xml::events::Event;

use super::{Limits, Refused};

/// An element: its local name, attributes (by their names as written) and children.
#[derive(Debug, Default)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

#[derive(Debug)]
pub enum Node {
    Element(Element),
    Text(String),
}

impl Element {
    /// An attribute's value: by its name as written when `name` has a prefix (`r:id`), else
    /// by local name, an unprefixed attribute first.
    pub fn attr(&self, name: &str) -> Option<&str> {
        let found = if name.contains(':') {
            self.attrs.iter().find(|(k, _)| k == name)
        } else {
            self.attrs
                .iter()
                .find(|(k, _)| k == name)
                .or_else(|| self.attrs.iter().find(|(k, _)| local(k.as_bytes()) == name))
        };
        found.map(|(_, v)| v.as_str())
    }

    /// The child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// The first child element with this local name.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    /// Follows a path of child names.
    pub fn path(&self, names: &[&str]) -> Option<&Element> {
        names.iter().try_fold(self, |e, n| e.child(n))
    }

    /// Every descendant element with this local name, in document order (not below a match).
    pub fn find_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        for e in self.elements() {
            if e.name == name {
                out.push(e);
            } else {
                e.find_all(name, out);
            }
        }
    }

    /// The text of the element and everything below it, joined.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let mut stack: Vec<&Node> = self.children.iter().rev().collect();
        while let Some(n) = stack.pop() {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Element(e) => stack.extend(e.children.iter().rev()),
            }
        }
        out
    }
}

fn local(name: &[u8]) -> String {
    let local = match name.iter().position(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    };
    String::from_utf8_lossy(local).into_owned()
}

fn start(e: &quick_xml::events::BytesStart<'_>, decoder: quick_xml::encoding::Decoder) -> Element {
    let attrs = e
        .attributes()
        .flatten()
        .map(|a| {
            let value = a
                .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
            (String::from_utf8_lossy(a.key.as_ref()).into_owned(), value)
        })
        .collect();
    Element {
        name: local(e.name().as_ref()),
        attrs,
        children: Vec::new(),
    }
}

/// Parses `bytes` (UTF-8, as Office writes its parts) into a tree under a synthetic root.
pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Element, Refused> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().check_end_names = false;
    let mut stack = vec![Element::default()];
    let mut count = 0usize;
    let bad = |e: &dyn std::fmt::Display| Refused::Unreadable(format!("malformed XML: {e}"));
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| bad(&e))?;
        match event {
            Event::Start(e) => {
                count += 1;
                if count.is_multiple_of(4096) {
                    limits.late()?;
                }
                if count > limits.xml_elements {
                    return Err(Refused::TooLarge(format!(
                        "more than {} XML elements",
                        limits.xml_elements
                    )));
                }
                if stack.len() > limits.xml_depth {
                    return Err(Refused::TooLarge(format!(
                        "XML nested deeper than {} levels",
                        limits.xml_depth
                    )));
                }
                stack.push(start(&e, reader.decoder()));
            }
            Event::Empty(e) => {
                count += 1;
                if count.is_multiple_of(4096) {
                    limits.late()?;
                }
                if count > limits.xml_elements {
                    return Err(Refused::TooLarge(format!(
                        "more than {} XML elements",
                        limits.xml_elements
                    )));
                }
                let el = start(&e, reader.decoder());
                stack
                    .last_mut()
                    .expect("the root stays")
                    .children
                    .push(Node::Element(el));
            }
            Event::End(_) => {
                if stack.len() > 1 {
                    let el = stack.pop().expect("checked");
                    stack
                        .last_mut()
                        .expect("the root stays")
                        .children
                        .push(Node::Element(el));
                }
            }
            Event::Text(t) => {
                let text = t.xml10_content().map_err(|e| bad(&e))?;
                push_text(&mut stack, &text);
            }
            Event::CData(t) => {
                push_text(&mut stack, &String::from_utf8_lossy(&t));
            }
            Event::GeneralRef(r) => {
                // Character references and XML's five; any other entity is dropped, never
                // looked up.
                let ch = match r.resolve_char_ref().ok().flatten() {
                    Some(c) => Some(c),
                    None => match &*r as &[u8] {
                        b"lt" => Some('<'),
                        b"gt" => Some('>'),
                        b"amp" => Some('&'),
                        b"apos" => Some('\''),
                        b"quot" => Some('"'),
                        _ => None,
                    },
                };
                if let Some(c) = ch {
                    push_text(&mut stack, c.encode_utf8(&mut [0; 4]));
                }
            }
            // A document type (and the entities it may declare), comments and processing
            // instructions are skipped.
            Event::DocType(_) | Event::Comment(_) | Event::Decl(_) | Event::PI(_) => {}
            Event::Eof => break,
        }
        buf.clear();
    }
    // Elements left open are closed.
    while stack.len() > 1 {
        let el = stack.pop().expect("checked");
        stack
            .last_mut()
            .expect("the root stays")
            .children
            .push(Node::Element(el));
    }
    Ok(stack.pop().expect("the root"))
}

fn push_text(stack: &mut [Element], text: &str) {
    if text.is_empty() {
        return;
    }
    let top = stack.last_mut().expect("the root stays");
    match top.children.last_mut() {
        Some(Node::Text(t)) => t.push_str(text),
        _ => top.children.push(Node::Text(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_are_never_expanded_and_names_are_local() {
        let xml = br#"<?xml version="1.0"?>
<!DOCTYPE d [<!ENTITY boom "BOOM"><!ENTITY ext SYSTEM "file:///etc/passwd">]>
<w:d xmlns:w="x"><w:t a:k="1 &amp; 2">a &lt; b &#x4e2d; &boom; &ext;</w:t></w:d>"#;
        let root = parse(xml, &Limits::default()).unwrap();
        let t = root.path(&["d", "t"]).unwrap();
        assert_eq!(t.attr("k"), Some("1 & 2"));
        assert_eq!(t.text(), "a < b 中  ");
    }

    #[test]
    fn nesting_and_element_counts_are_capped() {
        let deep = format!("{}{}", "<a>".repeat(300), "</a>".repeat(300));
        assert!(matches!(
            parse(deep.as_bytes(), &Limits::default()),
            Err(Refused::TooLarge(_))
        ));
        let limits = Limits {
            xml_elements: 10,
            ..Limits::default()
        };
        let many = format!("<r>{}</r>", "<b/>".repeat(20));
        assert!(matches!(
            parse(many.as_bytes(), &limits),
            Err(Refused::TooLarge(_))
        ));
        // Past its deadline the reader stops, though no other limit is reached.
        let late = Limits {
            until: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ..Limits::default()
        };
        let long = format!("<r>{}</r>", "<b/>".repeat(10_000));
        match parse(long.as_bytes(), &late) {
            Err(Refused::TooLarge(why)) => assert!(why.contains("longer than allowed"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(parse(long.as_bytes(), &Limits::default()).is_ok());
    }
}
