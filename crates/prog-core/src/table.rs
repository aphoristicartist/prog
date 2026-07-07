//! Semantic table inference for non-JSON text artifacts.
//!
//! Pure and dependency-free (the module is a future Kani target alongside
//! redaction/shape). Provides readers for CSV/TSV (RFC 4180, with quoted
//! fields), GitHub-flavored markdown tables, and aligned/whitespace tables
//! (e.g. `kubectl get`, `ls -l`). Cells are stored as their original strings,
//! so disclosure projection never fabricates a value (INVARIANTS.md I1);
//! ambiguous parsing is flagged `lossy` rather than guessed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableFormat {
    Csv,
    Tsv,
    Markdown,
    Aligned,
}

impl TableFormat {
    pub fn id(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Markdown => "markdown",
            Self::Aligned => "aligned",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Csv => "CSV (RFC 4180)",
            Self::Tsv => "TSV (tab-separated)",
            Self::Markdown => "Markdown table",
            Self::Aligned => "Aligned/whitespace table",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTable {
    pub format: TableFormat,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// True when parsing made a lossy structural assumption (e.g. an aligned
    /// table with ragged rows, or a row whose cell count differs from the
    /// header). Cells are still the original strings.
    pub lossy: bool,
    pub confidence: f64,
}

impl ParsedTable {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TableDetection {
    pub format: TableFormat,
    pub confidence: f64,
    pub lossy: bool,
    pub reason: &'static str,
}

/// Detect the most likely table format in `text`, given an optional `mime`
/// hint. Returns `None` when the text does not look like a table (fewer than
/// two columns or fewer than two rows after parsing).
pub fn detect_table(text: &str, mime: &str) -> Option<TableDetection> {
    let mime = mime.to_ascii_lowercase();
    let nonempty_lines = text.lines().filter(|line| !line.trim().is_empty()).count();

    // Explicit MIME hints win, but the content must still parse to a real table.
    if mime.contains("csv")
        && let Some(table) = parse_delimited(text, ',')
    {
        return Some(detection(
            TableFormat::Csv,
            0.95,
            table.lossy,
            "text/csv MIME hint",
        ));
    }
    if (mime.contains("tab-separated") || mime.contains("tsv") || mime.contains("tab+separated"))
        && let Some(table) = parse_delimited(text, '\t')
    {
        return Some(detection(
            TableFormat::Tsv,
            0.95,
            table.lossy,
            "tab-separated MIME hint",
        ));
    }

    // GitHub-flavored markdown table: header row + separator row of dashes.
    if looks_like_markdown_table(text)
        && let Some(table) = parse_markdown(text)
    {
        return Some(detection(
            TableFormat::Markdown,
            0.9,
            table.lossy,
            "markdown header and separator row",
        ));
    }

    // Comma- or tab-delimited content with a consistent column count.
    if nonempty_lines >= 2 {
        if let Some(table) = parse_delimited(text, ',') {
            let first_line_commas = text
                .lines()
                .next()
                .map(|l| l.matches(',').count())
                .unwrap_or(0);
            if first_line_commas >= 1 && !table.columns.is_empty() {
                return Some(detection(
                    TableFormat::Csv,
                    0.7,
                    table.lossy,
                    "comma-delimited rows with a consistent shape",
                ));
            }
        }
        if let Some(table) = parse_delimited(text, '\t') {
            let first_line_tabs = text
                .lines()
                .next()
                .map(|l| l.matches('\t').count())
                .unwrap_or(0);
            if first_line_tabs >= 1 && !table.columns.is_empty() {
                return Some(detection(
                    TableFormat::Tsv,
                    0.7,
                    table.lossy,
                    "tab-delimited rows with a consistent shape",
                ));
            }
        }
    }

    // Aligned/whitespace table (heuristic, lossy).
    if nonempty_lines >= 2 && parse_aligned(text).is_some() {
        return Some(detection(
            TableFormat::Aligned,
            0.45,
            true,
            "whitespace-aligned columns",
        ));
    }

    None
}

/// Parse `text` as the given format, or `None` if it does not yield a table
/// with at least one column and one data row.
pub fn parse_table(text: &str, format: TableFormat) -> Option<ParsedTable> {
    let table = match format {
        TableFormat::Csv => parse_delimited(text, ',')?,
        TableFormat::Tsv => parse_delimited(text, '\t')?,
        TableFormat::Markdown => parse_markdown(text)?,
        TableFormat::Aligned => parse_aligned(text)?,
    };
    Some(table)
}

fn detection(
    format: TableFormat,
    confidence: f64,
    lossy: bool,
    reason: &'static str,
) -> TableDetection {
    TableDetection {
        format,
        confidence,
        lossy,
        reason,
    }
}

/// RFC 4180 reader for `delimiter`-separated records. Handles quoted fields
/// (with embedded delimiters, newlines, and escaped `""` quotes). The first
/// record is treated as the header row.
fn parse_delimited(text: &str, delimiter: char) -> Option<ParsedTable> {
    let records = read_delimited(text, delimiter);
    let header = records.first()?;
    if header.len() < 2 {
        return None;
    }
    let columns = header.clone();
    let width = columns.len();
    let mut rows = Vec::new();
    let mut lossy = false;
    for record in records.iter().skip(1) {
        if record.len() == 1 && record[0].trim().is_empty() {
            continue;
        }
        if record.len() != width {
            lossy = true;
        }
        let mut row = record.clone();
        row.resize(width, String::new());
        rows.push(row);
    }
    if rows.is_empty() {
        return None;
    }
    Some(ParsedTable {
        format: delimited_format(delimiter),
        columns,
        rows,
        lossy,
        confidence: 0.7,
    })
}

fn delimited_format(delimiter: char) -> TableFormat {
    if delimiter == '\t' {
        TableFormat::Tsv
    } else {
        TableFormat::Csv
    }
}

fn read_delimited(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' => {
                    if matches!(chars.peek(), Some('"')) {
                        field.push('"');
                        chars.next();
                    } else {
                        in_quotes = false;
                    }
                }
                _ => field.push(ch),
            }
            continue;
        }
        match ch {
            '"' => in_quotes = true,
            ch if ch == delimiter => {
                record.push(std::mem::take(&mut field));
            }
            '\n' => {
                if field.ends_with('\r') {
                    field.pop();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            '\r' => {}
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

fn looks_like_markdown_table(text: &str) -> bool {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty()).take(2);
    let Some(header) = lines.next() else {
        return false;
    };
    let Some(separator) = lines.next() else {
        return false;
    };
    header.contains('|') && is_markdown_separator_row(separator)
}

fn is_markdown_separator_row(line: &str) -> bool {
    let cells = split_markdown_cells(line);
    if cells.len() < 2 {
        return false;
    }
    cells
        .iter()
        .all(|cell| !cell.is_empty() && cell.chars().all(|c| matches!(c, ':' | '-')))
        && cells.iter().any(|cell| cell.contains('-'))
}

fn parse_markdown(text: &str) -> Option<ParsedTable> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header_line = lines.next()?;
    let separator_line = lines.next()?;
    if !is_markdown_separator_row(separator_line) {
        return None;
    }
    let columns = split_markdown_cells(header_line);
    if columns.len() < 2 {
        return None;
    }
    let width = columns.len();
    let mut rows = Vec::new();
    for line in lines {
        if !line.contains('|') {
            continue;
        }
        let mut cells = split_markdown_cells(line);
        cells.resize(width, String::new());
        rows.push(cells);
    }
    if rows.is_empty() {
        return None;
    }
    Some(ParsedTable {
        format: TableFormat::Markdown,
        columns,
        rows,
        lossy: false,
        confidence: 0.9,
    })
}

fn split_markdown_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let stripped = trimmed.trim_matches('|');
    stripped
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// Aligned/whitespace table reader. Splits each line on whitespace and maps
/// columns positionally. To avoid mis-reading prose as a table, every data row
/// must have the same token count as the header; otherwise the input is treated
/// as non-tabular. The interpretation is still heuristic, so it is flagged
/// lossy.
fn parse_aligned(text: &str) -> Option<ParsedTable> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header_line = lines.next()?;
    if !has_aligned_gaps(header_line) {
        return None;
    }
    let header_tokens = split_aligned_tokens(header_line);
    if header_tokens.len() < 2 {
        return None;
    }
    let columns: Vec<String> = header_tokens.iter().map(|t| t.to_string()).collect();
    let width = columns.len();
    let mut rows = Vec::new();
    for line in lines {
        let tokens = split_aligned_tokens(line);
        if tokens.is_empty() {
            continue;
        }
        if tokens.len() != width {
            return None;
        }
        rows.push(tokens.iter().map(|t| t.to_string()).collect());
    }
    if rows.is_empty() {
        return None;
    }
    Some(ParsedTable {
        format: TableFormat::Aligned,
        columns,
        rows,
        lossy: true,
        confidence: 0.45,
    })
}

fn split_aligned_tokens(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

fn has_aligned_gaps(line: &str) -> bool {
    // A real aligned header has at least one run of 2+ spaces between tokens.
    let mut run = 0usize;
    let mut found_gap = false;
    for ch in line.chars() {
        if ch == ' ' {
            run += 1;
            if run >= 2 {
                found_gap = true;
            }
        } else {
            run = 0;
        }
    }
    found_gap
}
