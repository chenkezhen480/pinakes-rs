//! Spreadsheets to Markdown: one table per sheet under its name as a heading, empty rows and
//! columns left out, the first row the header. Workbooks (`.xlsx`, `.xlsm`) are read by
//! calamine (pure Rust) one used cell at a time, so the cell cap holds while the sheet is read: a
//! sheet whose used range is huge (one cell at A1, one at XFD1048576) costs only its used cells.
//! The old `.xls` and `.ods` are not read here: calamine builds their sheets as dense grids
//! before anything can be counted. CSV is read by the csv crate.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use calamine::{Data, DataRef, Reader};

use super::md::Md;
use super::package::Package;
use super::{Limits, Refused};

/// Columns of a sheet kept as a table; a sheet wider than this is a database dump, not a
/// document, and the rest is left out (it says so).
const MAX_COLUMNS: usize = 64;

pub fn workbook(bytes: &[u8], limits: &Limits) -> Result<String, Refused> {
    // calamine unpacks the archive itself: check it first, all of it.
    Package::open(bytes, limits)?.verify()?;
    let mut book: calamine::Xlsx<_> = calamine::open_workbook_from_rs(Cursor::new(bytes))
        .map_err(|e| Refused::Unreadable(format!("the spreadsheet can't be read: {e}")))?;
    let mut out = Md::new();
    let mut cells = 0usize;
    // Cells read, empty ones too (the deadline is checked by these), and the text kept: a
    // shared string is copied into every cell that names it, so a small file could otherwise
    // grow without bound before the Markdown's length is checked.
    let mut seen = 0usize;
    let mut text = 0usize;
    for name in book.sheet_names() {
        let bad = |e: calamine::XlsxError| {
            Refused::Unreadable(format!("sheet {name:?} can't be read: {e}"))
        };
        let mut reader = book.worksheet_cells_reader(&name).map_err(bad)?;
        // The used cells, by position; empty ones never stored.
        let mut used: BTreeMap<(u32, u32), String> = BTreeMap::new();
        let mut cut = false;
        while let Some(cell) = reader.next_cell().map_err(bad)? {
            seen += 1;
            if seen.is_multiple_of(4096) {
                limits.late()?;
            }
            let value = text_ref(cell.get_value());
            if value.trim().is_empty() {
                continue;
            }
            if cells >= limits.cells {
                cut = true;
                break;
            }
            text += value.len();
            if text > limits.markdown_bytes {
                return Err(Refused::TooLarge(format!(
                    "the text is longer than {} MB",
                    limits.markdown_bytes >> 20
                )));
            }
            cells += 1;
            used.insert(cell.get_position(), value);
        }
        drop(reader);
        out.heading(2, &name);
        write_sparse(&mut out, &used, cut, limits);
        // The note says the rest of the workbook is left out: the sheets after it are not
        // written at all.
        if cut {
            break;
        }
    }
    Ok(out.finish())
}

/// Writes a sheet's used cells as a table over the rows and columns that hold something.
fn write_sparse(out: &mut Md, used: &BTreeMap<(u32, u32), String>, cut: bool, limits: &Limits) {
    let columns: BTreeSet<u32> = used.keys().map(|(_, c)| *c).collect();
    let wide = columns.len() > MAX_COLUMNS;
    let columns: Vec<u32> = columns.into_iter().take(MAX_COLUMNS).collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current: Option<u32> = None;
    for ((r, c), v) in used {
        let Ok(i) = columns.binary_search(c) else {
            continue;
        };
        if current != Some(*r) {
            current = Some(*r);
            rows.push(vec![String::new(); columns.len()]);
        }
        if let Some(row) = rows.last_mut() {
            row[i] = v.clone();
        }
    }
    out.table(&rows);
    if wide {
        out.paragraph(&format!(
            "(Columns past the first {MAX_COLUMNS} are left out.)"
        ));
    }
    if cut {
        out.paragraph(&format!(
            "(The rest of this workbook is left out: it has more than {} cells.)",
            limits.cells
        ));
    }
}

/// A cell as text: whole numbers without a fraction, dates as dates.
fn text_ref(cell: &DataRef<'_>) -> String {
    match cell {
        DataRef::SharedString(s) => (*s).to_owned(),
        DataRef::String(s) => s.clone(),
        DataRef::Int(i) => i.to_string(),
        DataRef::Float(f) => text(&Data::Float(*f)),
        DataRef::Bool(b) => b.to_string(),
        DataRef::DateTime(d) => text(&Data::DateTime(*d)),
        DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => s.clone(),
        DataRef::Error(_) | DataRef::Empty => String::new(),
    }
}

pub fn csv(bytes: &[u8], limits: &Limits) -> Result<String, Refused> {
    let text = super::decode_text(bytes);
    let delimiter = sniff(&text);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    let mut cells = 0usize;
    let mut cut = false;
    for record in reader.records() {
        let record = record.map_err(|e| Refused::Unreadable(format!("malformed CSV: {e}")))?;
        cells += record.len();
        if cells > limits.cells {
            cut = true;
            break;
        }
        rows.push(record.iter().map(str::to_owned).collect());
    }
    let mut out = Md::new();
    write(&mut out, trim(rows), cut, limits);
    Ok(out.finish())
}

/// The delimiter used most on the first lines: comma, semicolon or tab.
fn sniff(text: &str) -> u8 {
    let head: String = text.lines().take(5).collect::<Vec<_>>().join("\n");
    b",;\t"
        .iter()
        .copied()
        .max_by_key(|d| head.bytes().filter(|b| b == d).count())
        .unwrap_or(b',')
}

/// Writes a sheet's rows as a table within the cell cap, and says once when the rest is left
/// out (`cut`: reading already stopped at the cap).
fn write(out: &mut Md, mut rows: Vec<Vec<String>>, cut: bool, limits: &Limits) {
    let width = rows.first().map(Vec::len).unwrap_or(0);
    let room = limits.cells / width.max(1);
    let cut = cut || rows.len() > room;
    rows.truncate(room.max(1));
    out.table(&rows);
    if cut {
        out.paragraph(&format!(
            "(The rest of this sheet is left out: the file has more than {} cells.)",
            limits.cells
        ));
    }
}

/// Leaves out rows and columns with no text.
fn trim(rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let keep: Vec<bool> = (0..width)
        .map(|i| {
            rows.iter()
                .any(|r| r.get(i).is_some_and(|c| !c.trim().is_empty()))
        })
        .collect();
    rows.into_iter()
        .map(|r| {
            (0..width)
                .filter(|&i| keep[i])
                .map(|i| r.get(i).cloned().unwrap_or_default())
                .collect()
        })
        .collect()
}

/// A cell as text: whole numbers without a fraction, dates as dates.
fn text(cell: &Data) -> String {
    match cell {
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", *f as i64),
        Data::DateTime(d) => match d.as_datetime() {
            // A whole serial number is a day without a time.
            Some(dt) if d.as_f64().fract() == 0.0 => dt.date().to_string(),
            Some(dt) => dt.to_string(),
            None => d.to_string(),
        },
        Data::Error(_) => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_rows_become_a_table_with_empty_rows_and_columns_left_out() {
        let md = csv(
            "品名;;库存\n\n苹果;;12\n梨;;7\n".as_bytes(),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(
            md,
            "| 品名 | 库存 |\n| --- | --- |\n| 苹果 | 12 |\n| 梨 | 7 |\n"
        );
        // GB18030 text (as Excel saves CSV on a Chinese system) is read too.
        let (gbk, _, _) = encoding_rs::GB18030.encode("名称,数量\n螺丝,3\n");
        let md = csv(&gbk, &Limits::default()).unwrap();
        assert!(md.contains("| 螺丝 | 3 |"), "{md}");
    }

    /// A workbook of these sheet XMLs (the cells of each), as Excel lays one out.
    fn xlsx(sheets: &[&str]) -> Vec<u8> {
        xlsx_with(sheets, None)
    }

    /// A workbook of these sheets' cells, with a shared-strings part when given.
    fn xlsx_with(sheets: &[&str], shared: Option<&str>) -> Vec<u8> {
        let workbook_sheets: String = (1..=sheets.len())
            .map(|i| format!(r#"<sheet name="S{i}" sheetId="{i}" r:id="rId{i}"/>"#))
            .collect();
        let mut rels: String = (1..=sheets.len())
            .map(|i| format!(r#"<Relationship Id="rId{i}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{i}.xml"/>"#))
            .collect();
        if shared.is_some() {
            rels.push_str(r#"<Relationship Id="rIdS" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>"#);
        }
        let workbook = format!(
            r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{workbook_sheets}</sheets></workbook>"#
        );
        let rels = format!(
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
        );
        let root_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let types = r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
        let names: Vec<String> = (1..=sheets.len())
            .map(|i| format!("xl/worksheets/sheet{i}.xml"))
            .collect();
        let bodies: Vec<String> = sheets
            .iter()
            .map(|c| format!(r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{c}</sheetData></worksheet>"#))
            .collect();
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("[Content_Types].xml", types.as_bytes()),
            ("_rels/.rels", root_rels.as_bytes()),
            ("xl/workbook.xml", workbook.as_bytes()),
            ("xl/_rels/workbook.xml.rels", rels.as_bytes()),
        ];
        for (n, b) in names.iter().zip(&bodies) {
            entries.push((n, b.as_bytes()));
        }
        if let Some(sst) = shared {
            entries.push(("xl/sharedStrings.xml", sst.as_bytes()));
        }
        super::super::package::tests::zip_of(&entries)
    }

    #[test]
    fn a_huge_used_range_costs_only_its_cells() {
        // One cell at A1 and one at the sheet's last: a dense grid would be 17 billion cells.
        let file = xlsx(&[
            r#"<row r="1"><c r="A1" t="inlineStr"><is><t>first</t></is></c></row><row r="1048576"><c r="XFD1048576" t="inlineStr"><is><t>last</t></is></c></row>"#,
        ]);
        let started = std::time::Instant::now();
        let md = workbook(&file, &Limits::default()).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(
            md.contains("| first |  |") && md.contains("|  | last |"),
            "{md}"
        );
    }

    #[test]
    fn a_shared_string_named_by_many_cells_is_bounded() {
        // One long shared string and many cells naming it: the copies count against the cap.
        let shared = format!(
            r#"<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1"><si><t>{}</t></si></sst>"#,
            "x".repeat(10_000)
        );
        let rows: String = (1..=500)
            .map(|i| format!(r#"<row r="{i}"><c r="A{i}" t="s"><v>0</v></c></row>"#))
            .collect();
        let file = xlsx_with(&[rows.as_str()], Some(&shared));
        let limits = Limits {
            markdown_bytes: 1 << 20,
            ..Limits::default()
        };
        assert!(matches!(
            workbook(&file, &limits),
            Err(Refused::TooLarge(_))
        ));
    }

    #[test]
    fn a_workbook_over_the_cell_cap_stops_at_the_sheet_that_reached_it() {
        let row = |i: usize| {
            format!(r#"<row r="{i}"><c r="A{i}" t="inlineStr"><is><t>v{i}</t></is></c></row>"#)
        };
        let sheet: String = (1..=30).map(row).collect();
        let file = xlsx(&[
            sheet.as_str(),
            sheet.as_str(),
            sheet.as_str(),
            sheet.as_str(),
        ]);
        let limits = Limits {
            cells: 40,
            ..Limits::default()
        };
        let md = workbook(&file, &limits).unwrap();
        assert!(md.contains("## S2"), "{md}");
        assert!(!md.contains("## S3") && !md.contains("## S4"), "{md}");
        assert_eq!(md.matches("left out").count(), 1, "{md}");
    }

    #[test]
    fn a_sheet_over_the_cell_cap_is_cut_and_says_so() {
        let text: String = (0..100).map(|i| format!("{i},{i}\n")).collect();
        let limits = Limits {
            cells: 20,
            ..Limits::default()
        };
        let md = csv(text.as_bytes(), &limits).unwrap();
        assert!(md.contains("more than 20 cells"), "{md}");
        assert_eq!(
            md.matches("more than 20 cells").count(),
            1,
            "said once: {md}"
        );
        assert!(md.lines().count() < 20, "{md}");
    }
}
