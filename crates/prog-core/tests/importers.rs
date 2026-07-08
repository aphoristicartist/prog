use prog_core::{
    SourceKind,
    importers::{
        ImportContext, ImportExample, McpTool, import_cli_help, import_json_schema,
        import_mcp_schemas, import_openapi, refine_with_examples,
    },
};
use serde_json::{Value, json};

#[test]
fn openapi_import_builds_callable_profile_without_observed_shape() {
    let spec = json!({
        "openapi": "3.1.0",
        "info": {"title": "Issues", "version": "2026-07"},
        "servers": [{"url": "https://api.example.test/v1"}],
        "components": {
            "securitySchemes": {
                "bearerAuth": {"type": "http", "scheme": "bearer"},
                "queryKey": {"type": "apiKey", "in": "query", "name": "api_key"}
            }
        },
        "paths": {
            "/issues/{id}": {
                "parameters": [{
                    "name": "id",
                    "in": "path",
                    "required": true,
                    "schema": {"type": "string"}
                }],
                "get": {
                    "operationId": "getIssue",
                    "parameters": [{
                        "name": "include",
                        "in": "query",
                        "schema": {"type": "string"}
                    }],
                    "responses": {
                        "200": {
                            "content": {
                                "application/json": {
                                    "schema": {
                                        "type": "object",
                                        "properties": {"id": {"type": "string"}}
                                    }
                                }
                            }
                        }
                    }
                },
                "post": {
                    "operationId": "updateIssue",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {"type": "object", "properties": {"title": {"type": "string"}}}
                            }
                        }
                    },
                    "responses": {"200": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Issue"}}}}}
                }
            }
        }
    });

    let (profile, report) =
        import_openapi("issues".to_string(), &spec, &ImportContext::default()).unwrap();
    assert_eq!(profile.kind, SourceKind::Http);
    assert_eq!(report.operations_imported, 2);
    assert_eq!(report.schemas_imported, 2);
    assert_eq!(
        profile.extra["adapter"]["http"]["base_url"],
        "https://api.example.test/v1"
    );
    assert_eq!(profile.auth.len(), 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("queryKey"))
    );

    let get = operation(&profile.operations, "getissue");
    assert!(get.output_shape.is_none());
    assert!(get.declared_output_schema.is_some());
    assert!(get.effects.read_only);
    // GET is graded Proven and stored confirmation-gated; trust policy relaxes
    // it to requires_confirmation=false at call time under trust.auto_upgrade.
    assert!(get.effects.requires_confirmation);
    assert_eq!(get.effects.extra["evidence_grade"].as_str(), Some("proven"));
    assert_eq!(get.extra["invocation"]["http"]["path"], "/issues/{id}");
    assert_eq!(
        get.extra["invocation"]["http"]["query"]["include"],
        "{include}"
    );
    assert_eq!(
        get.input_schema["required"],
        Value::Array(vec![Value::String("id".to_string())])
    );

    let post = operation(&profile.operations, "updateissue");
    assert!(!post.effects.read_only);
    assert!(post.effects.requires_confirmation);
    // Non-GET methods are Unproven and stay gated (current behavior preserved).
    assert_eq!(
        post.effects.extra["evidence_grade"].as_str(),
        Some("unproven")
    );
    assert_eq!(post.extra["invocation"]["http"]["json_body"], "{body}");
    assert_eq!(
        post.declared_output_schema.as_ref().unwrap()["x-prog-ref_status"],
        "preserved_not_dereferenced"
    );
}

#[test]
fn schema_import_is_depth_bounded_and_preserves_external_refs() {
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Deep",
        "type": "object",
        "properties": {
            "external": {"$ref": "https://example.test/schema.json"},
            "nested": {"type": "object", "properties": {"a": {"type": "object", "properties": {"b": {"type": "string"}}}}}
        }
    });
    let ctx = ImportContext {
        max_schema_depth: 2,
        ..ImportContext::default()
    };

    let (profile, report) = import_json_schema("deep".to_string(), &schema, &ctx).unwrap();
    let declared = profile.operations[0]
        .declared_output_schema
        .as_ref()
        .unwrap();
    assert_eq!(
        declared["properties"]["external"]["x-prog-ref_status"],
        "preserved_not_dereferenced"
    );
    assert!(
        serde_json::to_string(declared)
            .unwrap()
            .contains("x-prog-truncated_schema")
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("without dereferencing"))
    );
}

#[test]
fn mcp_import_keeps_declared_schema_prior_and_fails_closed_without_read_hint() {
    let tools = vec![
        McpTool {
            name: "search_docs".to_string(),
            description: Some("Search docs".to_string()),
            input_schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
            output_schema: Some(
                json!({"type": "object", "properties": {"hits": {"type": "array"}}}),
            ),
            read_only_hint: Some(true),
            destructive_hint: None,
            annotations: None,
        },
        McpTool {
            name: "delete_doc".to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
            output_schema: None,
            read_only_hint: None,
            destructive_hint: None,
            annotations: None,
        },
    ];

    let (profile, report) =
        import_mcp_schemas("docs".to_string(), &tools, &[], &ImportContext::default()).unwrap();
    assert_eq!(report.schemas_imported, 1);
    let search = operation(&profile.operations, "search_docs");
    assert!(search.output_shape.is_none());
    assert!(search.declared_output_schema.is_some());
    assert_eq!(search.extra["schema_prior"]["observed"], false);
    assert!(search.effects.read_only);
    // readOnlyHint=true (no contradiction) is Proven and stored gated.
    assert!(search.effects.requires_confirmation);
    assert_eq!(
        search.effects.extra["evidence_grade"].as_str(),
        Some("proven")
    );

    let delete = operation(&profile.operations, "delete_doc");
    assert!(!delete.effects.read_only);
    assert!(delete.effects.mutating);
    assert!(delete.effects.requires_confirmation);
    assert_eq!(
        delete.effects.extra["evidence_grade"].as_str(),
        Some("unproven")
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("readOnlyHint"))
    );
}

#[test]
fn cli_help_import_is_conservative_and_does_not_invent_read_only_ops() {
    let help = "\
Usage: taskctl <COMMAND>

Commands:
  list      list tasks
  delete    delete a task

Options:
  -h, --help
";
    let (profile, report) = import_cli_help(
        "taskctl".to_string(),
        help,
        "taskctl --profile prod",
        &ImportContext::default(),
    )
    .unwrap();
    assert_eq!(report.operations_imported, 2);
    assert_eq!(profile.kind, SourceKind::Cli);
    assert!(!profile.trust.allow_shell);
    for operation in &profile.operations {
        assert!(!operation.effects.read_only);
        assert!(operation.effects.mutating);
        assert!(operation.effects.requires_confirmation);
        assert!(!operation.effects.shell);
    }
    let list = operation(&profile.operations, "taskctl_list");
    assert_eq!(list.extra["invocation"]["cli"]["command"], "taskctl");
    assert_eq!(list.extra["invocation"]["cli"]["args"][0], "--profile");
    assert_eq!(list.extra["invocation"]["cli"]["args"][1], "prod");
    assert_eq!(list.extra["invocation"]["cli"]["args"][2], "list");
}

#[test]
fn checked_in_examples_refine_observed_shape_monotonically_without_secrets() {
    let schema = json!({
        "type": "object",
        "properties": {"id": {"type": "integer"}}
    });
    let (mut profile, _) =
        import_json_schema("items".to_string(), &schema, &ImportContext::default()).unwrap();
    let operation = profile.operations[0].id.clone();
    let report = refine_with_examples(
        &mut profile,
        &[
            ImportExample {
                operation: operation.clone(),
                args: json!({"id": "1"}),
                output: json!({"id": 1, "state": "open", "token": "secret"}),
            },
            ImportExample {
                operation,
                args: json!({"id": "2"}),
                output: json!({"id": 2, "title": "second"}),
            },
        ],
        &ImportContext::default(),
    );

    assert_eq!(report.examples_inferred, 2);
    let shape = serde_json::to_value(profile.operations[0].output_shape.as_ref().unwrap()).unwrap();
    assert_eq!(shape["kind"], "object");
    assert!(shape["fields"]["state"]["optional"].as_bool().unwrap());
    assert!(shape["fields"]["title"]["optional"].as_bool().unwrap());
    assert_eq!(shape["fields"]["token"]["shape"]["kind"], "sensitive");
    assert_eq!(
        profile.operations[0].extra["example_observations"]["count"],
        2
    );
    assert!(!serde_json::to_string(&profile).unwrap().contains("secret"));
}

fn operation<'a>(
    operations: &'a [prog_core::OperationProfile],
    id: &str,
) -> &'a prog_core::OperationProfile {
    operations
        .iter()
        .find(|operation| operation.id == id)
        .expect("operation should be imported")
}

#[test]
fn mcp_destructive_contradiction_tightens_to_unproven_and_gated() {
    // readOnlyHint=true AND destructiveHint=true is contradictory: it must
    // tighten to Unproven/mutating/gated (monotone tightening) and never relax.
    let tools = vec![McpTool {
        name: "conflicting".to_string(),
        description: None,
        input_schema: json!({"type": "object"}),
        output_schema: None,
        read_only_hint: Some(true),
        destructive_hint: Some(true),
        annotations: None,
    }];
    let (profile, _) =
        import_mcp_schemas("docs".to_string(), &tools, &[], &ImportContext::default()).unwrap();
    let op = operation(&profile.operations, "conflicting");
    assert!(!op.effects.read_only);
    assert!(op.effects.mutating);
    assert!(op.effects.requires_confirmation);
    assert_eq!(
        op.effects.extra["evidence_grade"].as_str(),
        Some("unproven")
    );
}

#[test]
fn mcp_resource_is_proven_and_stored_gated() {
    let tools = vec![McpTool {
        name: "search".to_string(),
        description: None,
        input_schema: json!({"type": "object"}),
        output_schema: None,
        read_only_hint: Some(true),
        destructive_hint: None,
        annotations: None,
    }];
    let resources = vec![prog_core::importers::McpResource {
        name: "doc".to_string(),
        description: None,
        mime_type: Some("application/json".to_string()),
    }];
    let (profile, _) = import_mcp_schemas(
        "docs".to_string(),
        &tools,
        &resources,
        &ImportContext::default(),
    )
    .unwrap();
    let resource = operation(&profile.operations, "resource_doc");
    assert!(resource.effects.read_only);
    // MCP resource is spec-defined read-only -> Proven, stored gated.
    assert!(resource.effects.requires_confirmation);
    assert_eq!(
        resource.effects.extra["evidence_grade"].as_str(),
        Some("proven")
    );
}

#[test]
fn json_schema_synthesized_op_is_assumed_and_gated() {
    // JSON Schema synthesized ops have no explicit effect descriptor: the
    // importer only infers read-only, so they are graded Assumed and HARD-fenced
    // — Assumed NEVER relaxes, so storing them gated keeps them callable only
    // with --yes even under trust.auto_upgrade=true.
    let schema = json!({"type": "object", "properties": {"id": {"type": "integer"}}});
    let (profile, _) =
        import_json_schema("items".to_string(), &schema, &ImportContext::default()).unwrap();
    let op = &profile.operations[0];
    assert!(op.effects.read_only);
    assert!(op.effects.requires_confirmation);
    assert_eq!(op.effects.extra["evidence_grade"].as_str(), Some("assumed"));
}

#[test]
fn cli_help_ops_are_unproven_and_gated() {
    let help = "\
Commands:
  list      list tasks
  delete    delete a task

Options:
  -h, --help
";
    let (profile, _) = import_cli_help(
        "taskctl".to_string(),
        help,
        "taskctl",
        &ImportContext::default(),
    )
    .unwrap();
    for op in &profile.operations {
        assert_eq!(
            op.effects.extra["evidence_grade"].as_str(),
            Some("unproven")
        );
        assert!(op.effects.requires_confirmation);
    }
}
