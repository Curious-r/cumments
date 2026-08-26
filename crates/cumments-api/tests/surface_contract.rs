use std::collections::BTreeSet;

use serde_yaml_ng::Value;

const HTTP_METHODS: [&str; 6] = ["get", "post", "put", "patch", "delete", "query"];

fn registry_operations() -> BTreeSet<String> {
    cumments_core::surface::HTTP_OPERATIONS
        .iter()
        .map(|operation| {
            format!(
                "{} {}\t{}",
                operation.method, operation.path, operation.operation_id
            )
        })
        .collect()
}

#[test]
fn openapi_matches_the_http_capability_manifest() {
    let manifest_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/public/openapi.yaml"
    );
    let raw = std::fs::read_to_string(manifest_path).expect("read OpenAPI document");
    let document: Value = serde_yaml_ng::from_str(&raw).expect("parse OpenAPI document");

    let mut openapi_operations = BTreeSet::new();
    let paths = document
        .get("paths")
        .and_then(Value::as_mapping)
        .expect("OpenAPI paths object");

    for (path, item) in paths {
        let path = path.as_str().expect("path key");
        for method in HTTP_METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let operation = operation.as_mapping().expect("operation object");
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("missing operationId for {method} {path}"));
            openapi_operations.insert(format!(
                "{} {}\t{}",
                method.to_ascii_uppercase(),
                path,
                operation_id
            ));
        }
    }

    let registry_operations = registry_operations();
    let missing_in_openapi: Vec<_> = registry_operations
        .difference(&openapi_operations)
        .collect();
    let missing_in_registry: Vec<_> = openapi_operations
        .difference(&registry_operations)
        .collect();

    assert!(
        missing_in_openapi.is_empty(),
        "registry operations absent from OpenAPI: {missing_in_openapi:#?}"
    );
    assert!(
        missing_in_registry.is_empty(),
        "OpenAPI operations absent from registry: {missing_in_registry:#?}"
    );
}
