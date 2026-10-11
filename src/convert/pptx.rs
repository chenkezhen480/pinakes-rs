//! PowerPoint (`.pptx`) to Markdown: one heading per slide from its title (`Slide n` without
//! one), then its text frames top to bottom (body placeholders as lists, by indent level),
//! tables, and the speaker notes under `### Notes`. Pictures and charts are dropped.

use super::md::{self, Md};
use super::package::{Package, relationships, resolve};
use super::xml::Element;
use super::{Limits, Refused};

/// What a shape on a slide holds.
enum Shape<'a> {
    Title(String),
    Text { body: &'a Element, list: bool },
    Table(&'a Element),
}

pub fn convert(bytes: &[u8], limits: &Limits) -> Result<String, Refused> {
    let mut pkg = Package::open(bytes, limits)?;
    let pres = pkg.xml("ppt/presentation.xml")?.ok_or_else(|| {
        Refused::Unreadable("not a PowerPoint file: no ppt/presentation.xml".into())
    })?;
    let rels = relationships(pkg.xml("ppt/_rels/presentation.xml.rels")?);
    let mut slides = Vec::new();
    if let Some(list) = pres.path(&["presentation", "sldIdLst"]) {
        for s in list.elements().filter(|e| e.name == "sldId") {
            if let Some((target, _)) = s.attr("r:id").and_then(|id| rels.get(id)) {
                slides.push(resolve("ppt", target));
            }
        }
    }
    let mut out = Md::new();
    for (n, path) in slides.iter().enumerate() {
        if out.len() > limits.markdown_bytes {
            return Err(Refused::TooLarge(format!(
                "the text is longer than {} MB",
                limits.markdown_bytes >> 20
            )));
        }
        let Some(slide) = pkg.xml(path)? else {
            continue;
        };
        let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
        let slide_rels = relationships(pkg.xml(&format!("{dir}/_rels/{file}.rels"))?);
        let mut shapes = Vec::new();
        if let Some(tree) = slide.path(&["sld", "cSld", "spTree"]) {
            collect(tree, &mut shapes);
        }
        let title = shapes.iter().find_map(|(_, s)| match s {
            Shape::Title(t) if !t.is_empty() => Some(t.clone()),
            _ => None,
        });
        out.heading(1, &title.unwrap_or_else(|| format!("Slide {}", n + 1)));
        shapes.sort_by_key(|(pos, _)| *pos);
        for (_, shape) in &shapes {
            match shape {
                Shape::Title(_) => {}
                Shape::Text { body, list } => text_frame(body, *list, &mut out),
                Shape::Table(tbl) => out.table(&table(tbl)),
            }
        }
        let notes = slide_rels
            .values()
            .find(|(t, external)| !external && t.contains("notesSlide"))
            .map(|(t, _)| resolve(dir, t));
        if let Some(notes) = notes.and_then(|p| pkg.xml(&p).transpose()) {
            let notes = notes?;
            let mut found = Vec::new();
            if let Some(tree) = notes.path(&["notes", "cSld", "spTree"]) {
                collect(tree, &mut found);
            }
            let mut lines = Vec::new();
            for (_, s) in &found {
                // The notes' body; the slide image and number placeholders hold no text.
                if let Shape::Text { body, list: true } = s {
                    for p in body.elements().filter(|e| e.name == "p") {
                        let t = md::collapse(&paragraph(p));
                        if !t.is_empty() {
                            lines.push(t);
                        }
                    }
                }
            }
            if !lines.is_empty() {
                out.heading(3, "Notes");
                for l in lines {
                    out.paragraph(&md::escape_start(&l));
                }
            }
        }
    }
    Ok(out.finish())
}

/// The shapes of a shape tree, groups opened, with their positions (top, left).
fn collect<'a>(tree: &'a Element, out: &mut Vec<((i64, i64), Shape<'a>)>) {
    for el in tree.elements() {
        let pos = position(el);
        match el.name.as_str() {
            "sp" => {
                let ph = el.path(&["nvSpPr", "nvPr", "ph"]);
                let kind = ph.map(|p| p.attr("type").unwrap_or("body"));
                let Some(body) = el.child("txBody") else {
                    continue;
                };
                match kind {
                    Some("title" | "ctrTitle") => {
                        let text: Vec<String> = body
                            .elements()
                            .filter(|e| e.name == "p")
                            .map(|p| md::collapse(&paragraph(p)))
                            .filter(|t| !t.is_empty())
                            .collect();
                        out.push((pos, Shape::Title(text.join(" "))));
                    }
                    Some("body" | "obj") => out.push((pos, Shape::Text { body, list: true })),
                    // Dates, footers and slide numbers repeat on every slide.
                    Some("dt" | "ftr" | "sldNum" | "hdr" | "sldImg") => {}
                    _ => out.push((pos, Shape::Text { body, list: false })),
                }
            }
            "grpSp" => collect(el, out),
            "graphicFrame" => {
                if let Some(tbl) = el.path(&["graphic", "graphicData", "tbl"]) {
                    out.push((pos, Shape::Table(tbl)));
                }
            }
            "AlternateContent" => {
                if let Some(choice) = el.child("Choice") {
                    collect(choice, out);
                }
            }
            _ => {}
        }
    }
}

fn position(el: &Element) -> (i64, i64) {
    let xfrm = el
        .path(&["spPr", "xfrm"])
        .or_else(|| el.path(&["grpSpPr", "xfrm"]))
        .or_else(|| el.child("xfrm"));
    let off = xfrm.and_then(|x| x.child("off"));
    let coord = |name: &str| {
        off.and_then(|o| o.attr(name))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    (coord("y"), coord("x"))
}

/// A text paragraph's text (`a:p`).
fn paragraph(p: &Element) -> String {
    let mut out = String::new();
    for el in p.elements() {
        match el.name.as_str() {
            "r" | "fld" => {
                if let Some(t) = el.child("t") {
                    out.push_str(&t.text());
                }
            }
            "br" => out.push(' '),
            _ => {}
        }
    }
    out
}

fn text_frame(body: &Element, list: bool, out: &mut Md) {
    for p in body.elements().filter(|e| e.name == "p") {
        let text = md::collapse(&paragraph(p));
        if text.is_empty() {
            continue;
        }
        let ppr = p.child("pPr");
        let level: usize = ppr
            .and_then(|pp| pp.attr("lvl"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let bulleted = match ppr {
            Some(pp) if pp.child("buNone").is_some() => false,
            Some(pp) if pp.child("buChar").is_some() || pp.child("buAutoNum").is_some() => true,
            _ => list,
        };
        if bulleted {
            out.item(level, None, &text);
        } else {
            out.paragraph(&md::escape_start(&text));
        }
    }
    out.end_list();
}

fn table(tbl: &Element) -> Vec<Vec<String>> {
    tbl.elements()
        .filter(|e| e.name == "tr")
        .map(|tr| {
            tr.elements()
                .filter(|e| e.name == "tc")
                .map(|tc| {
                    if tc.attr("hMerge").is_some() || tc.attr("vMerge").is_some() {
                        return String::new();
                    }
                    tc.child("txBody")
                        .map(|b| {
                            b.elements()
                                .filter(|e| e.name == "p")
                                .map(|p| md::collapse(&paragraph(p)))
                                .filter(|t| !t.is_empty())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::package::tests::zip_of;
    use super::*;

    const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

    fn sp(ph: Option<&str>, y: i64, paras: &[(&str, u8)]) -> String {
        let ph = ph
            .map(|t| format!(r#"<p:ph type="{t}"/>"#))
            .unwrap_or_default();
        let ps: String = paras
            .iter()
            .map(|(t, l)| format!(r#"<a:p><a:pPr lvl="{l}"/><a:r><a:t>{t}</a:t></a:r></a:p>"#))
            .collect();
        format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="1" name="s"/><p:cNvSpPr/><p:nvPr>{ph}</p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="{y}"/></a:xfrm></p:spPr><p:txBody>{ps}</p:txBody></p:sp>"#
        )
    }

    #[test]
    fn slides_in_order_with_titles_lists_tables_and_notes() {
        let pres = format!(
            r#"<p:presentation {NS}><p:sldIdLst><p:sldId id="257" r:id="rId3"/><p:sldId id="256" r:id="rId2"/></p:sldIdLst></p:presentation>"#
        );
        let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="slide" Target="slides/slide2.xml"/></Relationships>"#;
        // The body is placed above the title in the file but below it on the slide.
        let slide1 = format!(
            r#"<p:sld {NS}><p:cSld><p:spTree>{}{}{}<p:graphicFrame><p:xfrm><a:off x="0" y="900"/></p:xfrm><a:graphic><a:graphicData><a:tbl><a:tr><a:tc><a:txBody><a:p><a:r><a:t>季度</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>销量</a:t></a:r></a:p></a:txBody></a:tc></a:tr><a:tr><a:tc><a:txBody><a:p><a:r><a:t>Q1</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>120</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            sp(Some("body"), 500, &[("增长来自华东", 0), ("新客占六成", 1)]),
            sp(Some("title"), 100, &[("季度回顾", 0)]),
            sp(Some("sldNum"), 50, &[("7", 0)]),
        );
        let slide_rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="notesSlide" Target="../notesSlides/notesSlide1.xml"/></Relationships>"#;
        let notes = format!(
            r#"<p:notes {NS}><p:cSld><p:spTree>{}{}</p:spTree></p:cSld></p:notes>"#,
            sp(Some("sldImg"), 0, &[]),
            sp(Some("body"), 10, &[("强调华东", 0)]),
        );
        let slide2 = format!(
            r#"<p:sld {NS}><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:sld>"#,
            sp(None, 0, &[("Agenda first", 0)]),
        );
        let file = zip_of(&[
            ("ppt/presentation.xml", pres.as_bytes()),
            ("ppt/_rels/presentation.xml.rels", rels.as_bytes()),
            ("ppt/slides/slide1.xml", slide1.as_bytes()),
            ("ppt/slides/_rels/slide1.xml.rels", slide_rels.as_bytes()),
            ("ppt/notesSlides/notesSlide1.xml", notes.as_bytes()),
            ("ppt/slides/slide2.xml", slide2.as_bytes()),
        ]);
        let md = convert(&file, &Limits::default()).unwrap();
        assert_eq!(
            md,
            "# Slide 1\n\nAgenda first\n\n# 季度回顾\n\n- 增长来自华东\n  - 新客占六成\n\n| 季度 | 销量 |\n| --- | --- |\n| Q1 | 120 |\n\n### Notes\n\n强调华东\n"
        );
    }
}
