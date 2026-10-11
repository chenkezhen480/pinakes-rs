//! Writing Markdown: blocks separated by blank lines, list items kept together, GitHub tables.

/// Whitespace runs as one space, trimmed.
pub fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Text that would start a block of another kind (`# `, `> `, `- `, `|`, a fence, a rule, a
/// setext underline) gets a backslash, so it stays text; an ordered marker (`1.`) gets it
/// before its `.` or `)`, since a backslash escapes only punctuation.
pub fn escape_start(text: &str) -> String {
    let t = text.trim_start();
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && matches!(t.as_bytes().get(digits), Some(b'.' | b')')) {
        return format!("{}\\{}", &t[..digits], &t[digits..]);
    }
    let line_of = |c: char| t.len() >= 3 && t.chars().all(|x| x == c || x == ' ');
    let marker = t.starts_with('#')
        || t.starts_with('>')
        || t.starts_with('|')
        || t.starts_with("- ")
        || t.starts_with("+ ")
        || t.starts_with("* ")
        || t.starts_with("```")
        || t.starts_with("~~~")
        || line_of('-')
        || line_of('*')
        || line_of('_')
        || (t.starts_with('=') && t.chars().all(|x| x == '=' || x == ' '));
    if marker {
        format!("\\{t}")
    } else {
        t.to_owned()
    }
}

/// A paragraph of lines kept apart (hard breaks), each one escaped: a line after a break could
/// start a heading, a fence or a list as well as the first.
pub fn broken_lines(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| escape_start(l))
        .collect::<Vec<_>>()
        .join("  \n")
}

/// A table cell's text: one line, pipes escaped.
pub fn cell(text: &str) -> String {
    collapse(text).replace('|', "\\|")
}

/// Most columns of one table written, and most cells.
pub const TABLE_COLUMNS: usize = 64;
pub const TABLE_CELLS: usize = 200_000;

#[derive(PartialEq, Clone, Copy)]
enum Last {
    None,
    Block,
    Item,
}

/// A Markdown document being written.
pub struct Md {
    out: String,
    last: Last,
}

impl Default for Md {
    fn default() -> Self {
        Self::new()
    }
}

impl Md {
    pub fn new() -> Self {
        Self {
            out: String::new(),
            last: Last::None,
        }
    }

    fn block_start(&mut self) {
        if self.last != Last::None {
            self.out.push_str("\n\n");
        }
    }

    pub fn heading(&mut self, level: usize, text: &str) {
        let text = collapse(text);
        if text.is_empty() {
            return;
        }
        self.block_start();
        self.out.push_str(&"#".repeat(level.clamp(1, 6)));
        self.out.push(' ');
        self.out.push_str(&text);
        self.last = Last::Block;
    }

    /// A paragraph of inline Markdown; its lines are kept (hard breaks are written as two
    /// trailing spaces by the caller, if wanted).
    pub fn paragraph(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.block_start();
        self.out.push_str(text);
        self.last = Last::Block;
    }

    /// A list item at `depth` (0 for the outermost): `- ` or, with a number, `1. `.
    pub fn item(&mut self, depth: usize, number: Option<usize>, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        match self.last {
            Last::Item => self.out.push('\n'),
            Last::Block => self.out.push_str("\n\n"),
            Last::None => {}
        }
        self.out.push_str(&"  ".repeat(depth.min(8)));
        match number {
            Some(n) => self.out.push_str(&format!("{n}. ")),
            None => self.out.push_str("- "),
        }
        self.out.push_str(&text.replace('\n', " "));
        self.last = Last::Item;
    }

    /// Ends a list, so a list right after it starts apart.
    pub fn end_list(&mut self) {
        if self.last == Last::Item {
            self.last = Last::Block;
        }
    }

    /// A table whose first row is its header. Rows are padded to the widest; a table with no
    /// text at all is left out.
    pub fn table(&mut self, rows: &[Vec<String>]) {
        // Rows that hold nothing are left out, and a table is bounded however its source is
        // shaped: at most `TABLE_COLUMNS` columns and `TABLE_CELLS` cells written (a row of a
        // million cells over a million empty rows would otherwise be padded to 10^12).
        let rows: Vec<&Vec<String>> = rows
            .iter()
            .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
            .collect();
        let full = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let width = full.min(TABLE_COLUMNS);
        if width == 0 {
            return;
        }
        let fit = (TABLE_CELLS / width).max(1);
        let cut = rows.len() > fit;
        let rows = &rows[..rows.len().min(fit)];
        self.block_start();
        let line = |row: &[String]| {
            let mut s = String::from("|");
            for i in 0..width {
                s.push(' ');
                s.push_str(&cell(row.get(i).map(String::as_str).unwrap_or("")));
                s.push_str(" |");
            }
            s
        };
        self.out.push_str(&line(rows[0]));
        self.out.push('\n');
        self.out.push('|');
        self.out.push_str(&" --- |".repeat(width));
        for row in &rows[1..] {
            self.out.push('\n');
            self.out.push_str(&line(row));
        }
        self.last = Last::Block;
        if full > width {
            self.paragraph(&format!(
                "(Columns past the first {TABLE_COLUMNS} are left out.)"
            ));
        }
        if cut {
            self.paragraph(&format!(
                "(The rest of this table is left out: it has more than {TABLE_CELLS} cells.)"
            ));
        }
    }

    /// A fenced code block.
    pub fn code(&mut self, text: &str) {
        let text = text.trim_matches('\n');
        if text.trim().is_empty() {
            return;
        }
        self.block_start();
        let fence = if text.contains("```") { "~~~~" } else { "```" };
        self.out.push_str(fence);
        self.out.push('\n');
        self.out.push_str(text);
        self.out.push('\n');
        self.out.push_str(fence);
        self.last = Last::Block;
    }

    /// A block quote of inline Markdown.
    pub fn quote(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        self.block_start();
        let quoted: Vec<String> = text.lines().map(|l| format!("> {l}")).collect();
        self.out.push_str(&quoted.join("\n"));
        self.last = Last::Block;
    }

    pub fn rule(&mut self) {
        self.block_start();
        self.out.push_str("---");
        self.last = Last::Block;
    }

    pub fn len(&self) -> usize {
        self.out.len()
    }

    pub fn finish(mut self) -> String {
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_lists_and_tables() {
        let mut md = Md::new();
        md.heading(1, "  Returns \n policy ");
        md.paragraph("Goods may be returned.");
        md.item(0, None, "one");
        md.item(1, Some(1), "two");
        md.table(&[vec!["Item".into(), "Days".into()], vec!["a|b".into()]]);
        assert_eq!(
            md.finish(),
            "# Returns policy\n\nGoods may be returned.\n\n- one\n  1. two\n\n| Item | Days |\n| --- | --- |\n| a\\|b |  |\n"
        );
        assert_eq!(escape_start("# not a heading"), "\\# not a heading");
        // A table is bounded however it is shaped.
        let mut md = Md::new();
        let mut wide = vec![String::new(); 1_000_000];
        wide[0] = "x".into();
        let mut rows = vec![wide];
        rows.extend((0..100_000).map(|i| vec![format!("r{i}")]));
        md.table(&rows);
        let text = md.finish();
        assert!(text.len() < 2_000_000, "{}", text.len());
        assert!(text.contains("Columns past the first 64"));
        assert!(text.contains("more than 200000 cells"));
        assert_eq!(escape_start("1. first"), "1\\. first");
        assert_eq!(escape_start("3) ship it"), "3\\) ship it");
        assert_eq!(escape_start("~~~"), "\\~~~");
        assert_eq!(escape_start("==="), "\\===");
        assert_eq!(escape_start("* * *"), "\\* * *");
        // Every line of a broken paragraph is escaped, not only the first.
        let lines = [
            "Note".to_owned(),
            "# 1 priority".to_owned(),
            "```".to_owned(),
        ];
        assert_eq!(broken_lines(&lines), "Note  \n\\# 1 priority  \n\\```");
        assert_eq!(escape_start("2024 sales"), "2024 sales");
    }
}
