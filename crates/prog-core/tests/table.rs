//! Unit tests for the pure table-inference module (prog_core::table).

use prog_core::table::{TableFormat, detect_table, parse_table};

#[test]
fn parses_csv_with_quoted_fields_and_embedded_commas() {
    let csv = "name,role,note\nAda,engineer,\"lives in NYC, NY\"\n\"Lin, C\",manager,ok\n";
    let detection = detect_table(csv, "").unwrap();
    assert_eq!(detection.format, TableFormat::Csv);
    let table = parse_table(csv, detection.format).unwrap();
    assert_eq!(table.columns, vec!["name", "role", "note"]);
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.rows[0], vec!["Ada", "engineer", "lives in NYC, NY"]);
    // Embedded comma inside quotes is preserved, not split.
    assert_eq!(table.rows[1][0], "Lin, C");
    assert!(!table.lossy);
}

#[test]
fn csv_quoted_field_can_contain_newlines() {
    let csv = "a,b\n1,\"line1\nline2\"\n";
    let table = parse_table(csv, TableFormat::Csv).unwrap();
    assert_eq!(table.rows.len(), 1);
    assert_eq!(table.rows[0][1], "line1\nline2");
}

#[test]
fn parses_tsv_on_tabs() {
    let tsv = "id\tstatus\n1\topen\n2\tclosed\n";
    let detection = detect_table(tsv, "").unwrap();
    assert_eq!(detection.format, TableFormat::Tsv);
    let table = parse_table(tsv, detection.format).unwrap();
    assert_eq!(table.columns, vec!["id", "status"]);
    assert_eq!(table.rows[1], vec!["2", "closed"]);
}

#[test]
fn parses_markdown_table_with_alignment_markers() {
    let md = "| Name | Age |\n| --- | ---: |\n| Ada | 36 |\n| Lin | 41 |\n";
    let detection = detect_table(md, "").unwrap();
    assert_eq!(detection.format, TableFormat::Markdown);
    let table = parse_table(md, detection.format).unwrap();
    assert_eq!(table.columns, vec!["Name", "Age"]);
    assert_eq!(table.rows[0], vec!["Ada", "36"]);
    assert_eq!(table.rows[1], vec!["Lin", "41"]);
}

#[test]
fn parses_aligned_whitespace_table() {
    let text = "NAME       STATUS     AGE\nnode-1     Ready      5m\nnode-2     Ready      12m\n";
    let detection = detect_table(text, "").unwrap();
    assert_eq!(detection.format, TableFormat::Aligned);
    assert!(detection.lossy);
    let table = parse_table(text, detection.format).unwrap();
    assert_eq!(table.columns, vec!["NAME", "STATUS", "AGE"]);
    assert_eq!(table.rows[0], vec!["node-1", "Ready", "5m"]);
    // Aligned interpretation is heuristic, so it is flagged lossy even when
    // every row aligns.
    assert!(table.lossy);
}

#[test]
fn csv_mime_hint_forces_csv_detection() {
    let detection = detect_table("a,b\n1,2\n", "text/csv").unwrap();
    assert_eq!(detection.format, TableFormat::Csv);
    assert!(detection.confidence >= 0.9);
}

#[test]
fn prose_is_not_detected_as_a_table() {
    let prose = "This is a normal paragraph of prose with a single sentence.\n\
                 It does not look like a table at all even with some words here.\n";
    assert!(
        detect_table(prose, "").is_none(),
        "prose should not be detected as a table"
    );
}

#[test]
fn single_row_is_not_a_table() {
    assert!(detect_table("a,b,c\n", "").is_none());
}

#[test]
fn ragged_aligned_input_is_not_a_table() {
    // Inconsistent token counts across rows => not a clean aligned table.
    let text = "A   B\n1   2   3\n4   5\n";
    assert!(detect_table(text, "").is_none());
}

#[test]
fn cells_are_original_strings_never_coerced() {
    // INVARIANTS.md I1: projection must never fabricate values. Cells are the
    // original source strings, so "007" stays a string, not the number 7.
    let table = parse_table("x,y\n007,hello\n", TableFormat::Csv).unwrap();
    assert_eq!(table.rows[0][0], "007");
    assert_eq!(table.rows[0][1], "hello");
}

#[test]
fn ragged_csv_is_flagged_lossy_and_padded() {
    let csv = "a,b,c\n1,2,3\n4,5\n";
    let table = parse_table(csv, TableFormat::Csv).unwrap();
    assert!(table.lossy);
    assert_eq!(table.rows[1], vec!["4", "5", ""]);
}

#[test]
fn markdown_over_wide_row_is_flagged_lossy() {
    let md = "| A | B |\n| --- | --- |\n| 1 | 2 | 3 |\n";
    let table = parse_table(md, TableFormat::Markdown).unwrap();
    assert!(table.lossy, "an over-wide markdown row must be flagged lossy");
    assert_eq!(table.columns, vec!["A", "B"]);
    assert_eq!(table.rows[0], vec!["1", "2"]);
}

#[test]
fn csv_leading_utf8_bom_is_stripped_from_header() {
    let csv = "\u{FEFF}name,role\nAda,engineer\n";
    let table = parse_table(csv, TableFormat::Csv).unwrap();
    assert_eq!(table.columns[0], "name");
    assert_eq!(table.rows[0][0], "Ada");
}
