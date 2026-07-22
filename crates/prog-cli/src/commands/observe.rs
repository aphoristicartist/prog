//! Artifact observation and normalization commands.

use crate::*;

pub(crate) fn observe_artifact(
    store: &Store,
    lens_dir: &Path,
    args: &ObserveArgs,
    ctx: &mut InvocationContext,
) -> Result<DisclosureEnvelope> {
    let input = read_observation_input(args)?;
    let normalized = normalize_observation(&input.bytes, &input.mime)?;
    let lens = match &args.lens {
        Some(id) => {
            let lens = load_lens(lens_dir, id, "observe --lens")?;
            validate_lens_matches_observe(&lens, &input, &normalized)?;
            Some(lens)
        }
        None => None,
    };
    let redaction = RedactionPolicy::default();
    let redacted = RawPayload::new(normalized.payload).redact(&redaction);
    let redacted_paths = redacted.redacted_paths;
    let value_scan = redacted.value_scan;
    let payload = redacted.payload;
    let redacted_bytes = serde_json::to_vec(payload.as_value())?;
    let payload_bytes = redacted_bytes.len().try_into().unwrap_or(u64::MAX);
    let cache_key = Store::cache_key(
        "observe",
        &input.name,
        &json!({
            "kind": normalized.kind,
            "mime": input.mime,
            "redacted_sha256": hex_sha256(&redacted_bytes)
        }),
    )?;
    let payload_hash = store.put_payload(&payload)?;
    let ttl: i64 = args
        .ttl_seconds
        .try_into()
        .map_err(|_| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "ttl_seconds is too large".to_string(),
        })?;
    let mut entry = new_cache_entry(
        cache_key.clone(),
        payload_hash.clone(),
        "observe".to_string(),
        input.name.clone(),
        payload_bytes,
        ttl,
    );
    entry.provenance = Some(observation_provenance(
        &cache_key,
        &input,
        &normalized.kind,
        redacted_paths.len(),
    ));
    let (availability, capture) = complete_capture(payload_bytes, true, !redacted_paths.is_empty());
    ctx.set_capture(capture.budget.clone());
    let observation_id = record_capture(
        store,
        payload_hash.clone(),
        availability,
        capture,
        cache_key.clone(),
        "observe".to_string(),
        input.name.clone(),
        args.comparison_family.clone(),
        selection_coverage(&args.selection_scopes, args.selection_exhaustive),
        entry.provenance.clone(),
        Some(cache_key.clone()),
        !redacted_paths.is_empty(),
        None,
        Some(normalized.parser.id.to_string()),
        lens.as_ref(),
        None,
        // prog has no revalidation mechanism for observed artifacts (no
        // ETag/mtime check): the file could have changed since observation
        // and there is no proof either way, so this stays Unknown.
        prog_core::SourceValidity::Unknown,
    )?;
    entry.observation_id = Some(observation_id.clone());
    let cache_retained = store.put_entry(&cache_key, &entry)?;

    let requested_view = SliceRequest {
        path: None,
        limit: None,
        depth: None,
        fields: Vec::new(),
        omit: Vec::new(),
        extra: Extra::new(),
    };
    let slice = match &lens {
        Some(lens) => lens_slice_request(lens, &requested_view)?,
        None => requested_view,
    };
    let root_path = slice.path.clone().unwrap_or_default();
    let cursor = cache_retained
        .then(|| {
            store.create_cursor_with_extra(
                &cache_key,
                "observe",
                &input.name,
                &root_path,
                ttl,
                cursor_lens_extra(lens.as_ref()),
            )
        })
        .transpose()?;
    let mut warnings = normalized.warnings;
    if !redacted_paths.is_empty() {
        warnings.push(format!(
            "redacted {} sensitive path(s) before persistence",
            redacted_paths.len()
        ));
    }
    if !cache_retained {
        warnings.push(
            "cache retention policy evicted this payload before it could be reused".to_string(),
        );
    }
    envelope_for_payload(
        store,
        EnvelopeInput {
            value_scan: Some(value_scan),
            source_id: "observe".to_string(),
            operation: input.name.clone(),
            source_kind: Some("artifact".to_string()),
            payload,
            root_path,
            slice,
            payload_bytes,
            observation_id: Some(observation_id),
            provenance: entry.provenance.clone(),
            cache: Some(if cache_retained {
                cache_info(CacheStatus::Stored, &entry, Some(0))
            } else {
                CacheInfo {
                    status: CacheStatus::Skipped,
                    ttl_seconds: None,
                    expires_at: None,
                    age_seconds: None,
                }
            }),
            effects: None,
            auto_upgrade_audit: None,
            redacted_paths: redacted_paths.len(),
            cache_disabled_reason: None,
            warnings,
            schema_hints: BTreeMap::new(),
            next_action_operation: None,
            additional_next_actions: Vec::new(),
            observation_parser: Some(parser_metadata(&normalized.parser)),
            lens,
        },
        cursor,
        ctx.max_envelope_bytes(),
    )
}

fn read_observation_input(args: &ObserveArgs) -> Result<ObservationInput> {
    let (bytes, name, input) = if let Some(path) = &args.file {
        let bytes = std::fs::read(path).map_err(|error| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: format!(
                "file '{}' could not be read: {error}",
                path.to_string_lossy()
            ),
        })?;
        let name = args.name.clone().unwrap_or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file")
                .to_string()
        });
        (
            bytes,
            name,
            json!({
                "kind": "file",
                "path": path.to_string_lossy()
            }),
        )
    } else if args.stdin {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|error| CoreError::BadArgs {
                operation: "observe".to_string(),
                reason: format!("stdin could not be read: {error}"),
            })?;
        (
            bytes,
            args.name.clone().unwrap_or_else(|| "stdin".to_string()),
            json!({"kind": "stdin"}),
        )
    } else {
        return Err(CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "pass --file <path> or --stdin".to_string(),
        });
    };

    let mime = args
        .mime
        .clone()
        .unwrap_or_else(|| sniff_mime_from_bytes(&bytes).to_string());
    Ok(ObservationInput {
        name,
        input,
        mime,
        bytes,
    })
}

const OBSERVATION_PARSERS: &[ObservationParser] = &[
    ObservationParser {
        id: "sarif",
        detect: detect_sarif_observation,
        parse: parse_sarif_observation,
    },
    ObservationParser {
        id: "ndjson",
        detect: detect_ndjson_observation,
        parse: parse_ndjson_observation,
    },
    ObservationParser {
        id: "json",
        detect: detect_json_observation,
        parse: parse_json_observation,
    },
    ObservationParser {
        id: "junit_xml",
        detect: detect_junit_xml_observation,
        parse: parse_junit_xml_observation,
    },
    ObservationParser {
        id: "html_basic",
        detect: detect_html_observation,
        parse: parse_html_observation,
    },
    ObservationParser {
        id: "unified_diff",
        detect: detect_diff_observation,
        parse: parse_diff_observation,
    },
    ObservationParser {
        id: "table",
        detect: detect_table_observation,
        parse: parse_table_observation,
    },
    ObservationParser {
        id: "text_fallback",
        detect: detect_text_observation,
        parse: parse_text_observation,
    },
];

pub(crate) fn normalize_observation(bytes: &[u8], mime: &str) -> Result<NormalizedObservation> {
    let mut parser_errors = Vec::new();
    for parser in OBSERVATION_PARSERS {
        let Some(matched) = (parser.detect)(bytes, mime) else {
            continue;
        };
        match (parser.parse)(bytes, mime, matched) {
            Ok(mut normalized) => {
                normalized.warnings.extend(parser_errors);
                return Ok(normalized);
            }
            Err(error) => parser_errors.push(format!("{}: {error}", parser.id)),
        }
    }

    if is_binaryish(bytes) {
        return Err(CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "input appears to be binary; pass a text, JSON, NDJSON, diff, XML, HTML, or SARIF artifact".to_string(),
        });
    }
    let mut normalized = parse_text_observation(
        bytes,
        mime,
        ParserMatch {
            confidence: 0.25,
            reason: "all specific parsers failed; using text fallback",
        },
    )?;
    normalized.warnings.extend(parser_errors);
    Ok(normalized)
}

fn detect_sarif_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    if !normalized_mime.contains("sarif") && !normalized_mime.contains("json") {
        return None;
    }
    let value = serde_json::from_slice::<Value>(bytes).ok()?;
    if value.get("runs").and_then(Value::as_array).is_some()
        && value.get("version").and_then(Value::as_str).is_some()
    {
        Some(ParserMatch {
            confidence: 0.98,
            reason: "JSON object contains SARIF version and runs",
        })
    } else {
        None
    }
}

fn parse_sarif_observation(
    bytes: &[u8],
    mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let mut normalized = parse_json_observation(bytes, mime, matched)?;
    normalized.kind = "sarif".to_string();
    normalized.parser.id = "sarif";
    normalized.parser.label = "SARIF JSON";
    normalized.parser.path_semantics = "sarif JSON pointer";
    Ok(normalized)
}

fn detect_ndjson_observation(_bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    (normalized_mime.contains("ndjson") || normalized_mime.contains("jsonlines")).then_some(
        ParserMatch {
            confidence: 0.98,
            reason: "mime declares NDJSON or JSON Lines",
        },
    )
}

fn detect_json_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    if normalized_mime.contains("ndjson") || normalized_mime.contains("jsonlines") {
        return None;
    }
    (normalized_mime.contains("json") || sniff_mime_from_bytes(bytes) == "application/json")
        .then_some(ParserMatch {
            confidence: 0.95,
            reason: "mime or byte sniffing indicates JSON",
        })
}

fn parse_json_observation(
    bytes: &[u8],
    mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let payload = serde_json::from_slice(bytes).map_err(|error| CoreError::BadArgs {
        operation: "observe".to_string(),
        reason: format!("input with mime '{mime}' must be valid JSON: {error}"),
    })?;
    Ok(NormalizedObservation {
        kind: "json".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "json",
            label: "JSON tree",
            confidence: matched.confidence,
            lossy: false,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer",
            range_semantics: "tree nodes",
        },
        warnings: Vec::new(),
    })
}

fn parse_ndjson_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let text = std::str::from_utf8(bytes).map_err(|error| CoreError::BadArgs {
        operation: "observe".to_string(),
        reason: format!("NDJSON input must be valid UTF-8: {error}"),
    })?;
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<Value>(line).map_err(|error| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: format!("NDJSON line {} is not valid JSON: {error}", index + 1),
        })?;
        records.push(record);
    }
    let record_count = records.len();
    let line_count = text.lines().count();
    Ok(NormalizedObservation {
        kind: "ndjson".to_string(),
        payload: json!({
            "format": "ndjson",
            "records": records,
            "record_count": record_count,
            "line_count": line_count,
            "byte_count": bytes.len()
        }),
        parser: ObservationParserInfo {
            id: "ndjson",
            label: "NDJSON records",
            confidence: matched.confidence,
            lossy: false,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over /records",
            range_semantics: "line-delimited records",
        },
        warnings: Vec::new(),
    })
}

fn detect_junit_xml_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    let prefix = text_prefix(bytes).to_ascii_lowercase();
    (normalized_mime.contains("junit")
        || (normalized_mime.contains("xml")
            && (prefix.contains("<testsuite") || prefix.contains("<testsuites"))))
    .then_some(ParserMatch {
        confidence: 0.90,
        reason: "mime or XML root indicates JUnit",
    })
}

fn parse_junit_xml_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let (text, mut warnings, utf8_valid) = decode_text(bytes);
    let cases = parse_junit_cases(&text);
    let mut payload = text_payload(&text, bytes.len(), utf8_valid, "junit_xml");
    payload["testcases"] = Value::Array(cases);
    payload["testcase_count"] = json!(payload["testcases"].as_array().map_or(0, Vec::len));
    Ok(NormalizedObservation {
        kind: "junit_xml".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "junit_xml",
            label: "JUnit XML",
            confidence: matched.confidence,
            lossy: true,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over parsed testcases and /lines",
            range_semantics: "line ranges from XML text",
        },
        warnings: {
            warnings.push(
                "JUnit XML parser is lightweight and preserves raw line expansion".to_string(),
            );
            warnings
        },
    })
}

fn detect_html_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    let prefix = text_prefix(bytes).to_ascii_lowercase();
    (normalized_mime.contains("html")
        || prefix.contains("<html")
        || prefix.contains("<!doctype html"))
    .then_some(ParserMatch {
        confidence: 0.88,
        reason: "mime or text prefix indicates HTML",
    })
}

fn parse_html_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let (text, mut warnings, utf8_valid) = decode_text(bytes);
    let mut payload = text_payload(&text, bytes.len(), utf8_valid, "html");
    payload["title"] = json!(extract_tag_text(&text, "title"));
    payload["headings"] = Value::Array(
        ["h1", "h2", "h3"]
            .into_iter()
            .flat_map(|tag| extract_all_tag_text(&text, tag))
            .map(Value::String)
            .collect(),
    );
    payload["links"] = Value::Array(extract_links(&text));
    Ok(NormalizedObservation {
        kind: "html".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "html_basic",
            label: "Basic HTML",
            confidence: matched.confidence,
            lossy: true,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over title/headings/links and /lines",
            range_semantics: "line ranges from HTML source",
        },
        warnings: {
            warnings.push("HTML parser does not render or execute the document".to_string());
            warnings
        },
    })
}

fn detect_diff_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let normalized_mime = mime.to_ascii_lowercase();
    let prefix = text_prefix(bytes);
    (normalized_mime.contains("diff")
        || normalized_mime.contains("patch")
        || prefix.starts_with("diff --git")
        || prefix.contains("\n--- ")
        || prefix.contains("\n+++ "))
    .then_some(ParserMatch {
        confidence: 0.86,
        reason: "mime or diff markers indicate unified diff",
    })
}

fn parse_diff_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let (text, warnings, utf8_valid) = decode_text(bytes);
    let mut payload = text_payload(&text, bytes.len(), utf8_valid, "unified_diff");
    payload["files"] = Value::Array(parse_diff_files(&text));
    Ok(NormalizedObservation {
        kind: "unified_diff".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "unified_diff",
            label: "Unified diff",
            confidence: matched.confidence,
            lossy: false,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over diff files and /lines",
            range_semantics: "line ranges from diff text",
        },
        warnings,
    })
}

fn detect_text_observation(bytes: &[u8], _mime: &str) -> Option<ParserMatch> {
    (!is_binaryish(bytes)).then_some(ParserMatch {
        confidence: 0.50,
        reason: "fallback text parser accepted non-binary bytes",
    })
}

fn detect_table_observation(bytes: &[u8], mime: &str) -> Option<ParserMatch> {
    let text = std::str::from_utf8(bytes).ok()?;
    let detection = prog_core::table::detect_table(text, mime)?;
    Some(ParserMatch {
        confidence: detection.confidence,
        reason: detection.reason,
    })
}

fn parse_table_observation(
    bytes: &[u8],
    mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    let (text, mut warnings, utf8_valid) = decode_text(bytes);
    let detection =
        prog_core::table::detect_table(&text, mime).ok_or_else(|| CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "input matched a table detector but could not be re-detected during parse"
                .to_string(),
        })?;
    let table = prog_core::table::parse_table(&text, detection.format).ok_or_else(|| {
        CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "table detected but no rows parsed".to_string(),
        }
    })?;
    let lossy = table.lossy || !utf8_valid;
    let payload = json!({
        "format": table.format.id(),
        "columns": table.columns,
        "rows": table.rows,
        "row_count": table.row_count(),
        "column_count": table.column_count(),
        "byte_count": bytes.len(),
        "truncated": false,
        "utf8_valid": utf8_valid,
        "lossy": lossy
    });
    if table.lossy {
        warnings.push(format!(
            "table parsed as {} with a lossy structural assumption; cells are original strings",
            table.format.id()
        ));
    }
    Ok(NormalizedObservation {
        kind: "table".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "table",
            label: table.format.label(),
            confidence: matched.confidence,
            lossy,
            fallback: false,
            reason: matched.reason,
            path_semantics: "json pointer over /rows",
            range_semantics: "row indices over /rows",
        },
        warnings,
    })
}

fn parse_text_observation(
    bytes: &[u8],
    _mime: &str,
    matched: ParserMatch,
) -> Result<NormalizedObservation> {
    if is_binaryish(bytes) {
        return Err(CoreError::BadArgs {
            operation: "observe".to_string(),
            reason: "input appears to be binary; pass a text, JSON, or NDJSON artifact".to_string(),
        });
    }

    let (text, warnings, utf8_valid) = decode_text(bytes);
    let mut payload = text_payload(&text, bytes.len(), utf8_valid, "text");
    payload["repeated_stack_traces"] = json!(count_repeated_stack_trace_lines(&text));
    Ok(NormalizedObservation {
        kind: "text".to_string(),
        payload,
        parser: ObservationParserInfo {
            id: "text_fallback",
            label: "Text fallback",
            confidence: matched.confidence,
            lossy: !utf8_valid,
            fallback: true,
            reason: matched.reason,
            path_semantics: "json pointer over /lines",
            range_semantics: "line ranges from text",
        },
        warnings,
    })
}

fn text_payload(text: &str, byte_count: usize, utf8_valid: bool, format: &str) -> Value {
    let lines = text
        .lines()
        .enumerate()
        .map(|(index, line)| {
            json!({
                "number": index + 1,
                "text": redact_observed_text(line)
            })
        })
        .collect::<Vec<_>>();
    let line_count = lines.len();
    let head = lines
        .iter()
        .take(10)
        .map(|line| line["text"].clone())
        .collect::<Vec<_>>();
    let tail_start = lines.len().saturating_sub(10).max(head.len());
    let tail = lines
        .iter()
        .skip(tail_start)
        .map(|line| line["text"].clone())
        .collect::<Vec<_>>();

    json!({
        "format": format,
        "head": head,
        "tail": tail,
        "lines": lines,
        "line_count": line_count,
        "byte_count": byte_count,
        "utf8_valid": utf8_valid
    })
}

fn parser_metadata(parser: &ObservationParserInfo) -> Value {
    json!({
        "id": parser.id,
        "label": parser.label,
        "confidence": parser.confidence,
        "lossy": parser.lossy,
        "fallback": parser.fallback,
        "reason": parser.reason,
        "path_semantics": parser.path_semantics,
        "range_semantics": parser.range_semantics
    })
}

fn decode_text(bytes: &[u8]) -> (String, Vec<String>, bool) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), Vec::new(), true),
        Err(_) => (
            String::from_utf8_lossy(bytes).to_string(),
            vec!["input was not valid UTF-8; replacement characters were used".to_string()],
            false,
        ),
    }
}

fn text_prefix(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_string()
}

fn parse_junit_cases(text: &str) -> Vec<Value> {
    text.match_indices("<testcase")
        .map(|(start, _)| {
            let opening_end = text[start..]
                .find('>')
                .map(|offset| start + offset)
                .unwrap_or(text.len());
            let tag = &text[start..opening_end];
            let self_closing = tag.trim_end().ends_with('/');
            let case_end = if self_closing {
                opening_end.saturating_add(1)
            } else {
                text[opening_end..]
                    .find("</testcase>")
                    .map(|offset| opening_end + offset + "</testcase>".len())
                    .unwrap_or(opening_end.saturating_add(1))
            }
            .min(text.len());
            let body = &text[opening_end.min(text.len())..case_end];
            let failure = extract_tag_text(body, "failure")
                .map(|value| value.chars().take(2_000).collect::<String>());
            let error = extract_tag_text(body, "error")
                .map(|value| value.chars().take(2_000).collect::<String>());
            let skipped = body.to_ascii_lowercase().contains("<skipped");
            let status = if error.is_some() {
                "error"
            } else if failure.is_some() {
                "failed"
            } else if skipped {
                "skipped"
            } else {
                "passed"
            };
            json!({
                "name": extract_attr(tag, "name"),
                "classname": extract_attr(tag, "classname"),
                "time": extract_attr(tag, "time"),
                "status": status,
                "failure": failure,
                "error": error,
                "line_start": line_number_at(text, start),
                "line_end": line_number_at(text, case_end)
            })
        })
        .collect()
}

fn parse_diff_files(text: &str) -> Vec<Value> {
    let mut files = Vec::new();
    let mut current: Option<Map<String, Value>> = None;
    let mut hunk: Option<Map<String, Value>> = None;
    for (index, line) in text.lines().enumerate() {
        if let Some(path) = line.strip_prefix("diff --git ") {
            finish_diff_hunk(&mut current, &mut hunk, index);
            if let Some(file) = current.take() {
                files.push(Value::Object(file));
            }
            let mut file = Map::new();
            file.insert("header".to_string(), json!(path));
            file.insert("line_start".to_string(), json!(index + 1));
            file.insert("hunks".to_string(), Value::Array(Vec::new()));
            current = Some(file);
        } else if line.starts_with("@@") {
            finish_diff_hunk(&mut current, &mut hunk, index);
            let mut next = Map::new();
            next.insert("header".to_string(), json!(line));
            next.insert("line_start".to_string(), json!(index + 1));
            next.insert("lines".to_string(), Value::Array(Vec::new()));
            hunk = Some(next);
        } else if let Some(hunk) = hunk.as_mut()
            && let Some(lines) = hunk.get_mut("lines").and_then(Value::as_array_mut)
        {
            lines.push(Value::String(line.to_string()));
        }
    }
    finish_diff_hunk(&mut current, &mut hunk, text.lines().count());
    if let Some(file) = current {
        files.push(Value::Object(file));
    }
    files
}

fn finish_diff_hunk(
    file: &mut Option<Map<String, Value>>,
    hunk: &mut Option<Map<String, Value>>,
    end_index: usize,
) {
    let Some(mut completed) = hunk.take() else {
        return;
    };
    completed.insert("line_end".to_string(), json!(end_index.max(1)));
    if let Some(file) = file.as_mut()
        && let Some(hunks) = file.get_mut("hunks").and_then(Value::as_array_mut)
    {
        hunks.push(Value::Object(completed));
        file.insert("line_end".to_string(), json!(end_index.max(1)));
    }
}

fn extract_tag_text(text: &str, tag: &str) -> Option<String> {
    extract_all_tag_text(text, tag).into_iter().next()
}

fn extract_all_tag_text(text: &str, tag: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = lower[offset..].find(&open) {
        let absolute_start = offset + start;
        let Some(tag_end) = lower[absolute_start..].find('>') else {
            break;
        };
        let content_start = absolute_start + tag_end + 1;
        let Some(end) = lower[content_start..].find(&close) else {
            break;
        };
        let content_end = content_start + end;
        values.push(
            strip_tags(&text[content_start..content_end])
                .trim()
                .to_string(),
        );
        offset = content_end + close.len();
    }
    values
}

fn extract_links(text: &str) -> Vec<Value> {
    let lower = text.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut offset = 0usize;
    while let Some(start) = lower[offset..].find("<a ") {
        let absolute_start = offset + start;
        let Some(tag_end) = lower[absolute_start..].find('>') else {
            break;
        };
        let tag = &text[absolute_start..absolute_start + tag_end];
        links.push(json!({
            "href": extract_attr(tag, "href"),
            "line_start": line_number_at(text, absolute_start)
        }));
        offset = absolute_start + tag_end + 1;
    }
    links
}

fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needles = [
            format!(" {attr}={quote}"),
            format!("\t{attr}={quote}"),
            format!("\n{attr}={quote}"),
        ];
        if let Some((start, needle)) = needles
            .iter()
            .filter_map(|needle| tag.find(needle).map(|start| (start, needle)))
            .next()
        {
            let value_start = start + needle.len();
            let value_end = tag[value_start..]
                .find(quote)
                .map(|offset| value_start + offset)
                .unwrap_or(tag.len());
            return Some(tag[value_start..value_end].to_string());
        }
    }
    None
}

fn strip_tags(raw: &str) -> String {
    let mut output = String::new();
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn line_number_at(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset.min(text.len())].lines().count()
}

fn count_repeated_stack_trace_lines(text: &str) -> usize {
    let mut counts = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with("at ") || line.starts_with("File \"") {
            *counts.entry(line.to_string()).or_insert(0usize) += 1;
        }
    }
    counts.values().filter(|count| **count > 1).count()
}

pub(crate) fn sniff_mime_from_bytes(bytes: &[u8]) -> &'static str {
    if bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| byte == b'{' || byte == b'[')
    {
        "application/json"
    } else {
        "text/plain"
    }
}

fn is_binaryish(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.contains(&0) {
        return true;
    }
    let suspicious = bytes
        .iter()
        .filter(|byte| byte.is_ascii_control() && !matches!(byte, b'\n' | b'\r' | b'\t'))
        .count();
    suspicious.saturating_mul(10) > bytes.len()
}

pub(crate) fn redact_observed_text(line: &str) -> String {
    prog_core::redact_sensitive_text(line).0
}

fn observation_provenance(
    cache_key: &str,
    input: &ObservationInput,
    kind: &str,
    redacted_paths: usize,
) -> CallProvenance {
    let mut extra = Extra::new();
    extra.insert(
        "observe".to_string(),
        json!({
            "name": &input.name,
            "input": &input.input,
            "mime": &input.mime,
            "kind": kind,
            "input_bytes": input.bytes.len(),
            "redacted_paths": redacted_paths
        }),
    );
    CallProvenance {
        source_call_id: format!(
            "observe_{}",
            Utc::now()
                .timestamp_nanos_opt()
                .unwrap_or_else(|| Utc::now().timestamp_micros())
        ),
        cache_key: Some(cache_key.to_string()),
        captured_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        status: Some("observed".to_string()),
        duration_ms: None,
        extra,
    }
}
