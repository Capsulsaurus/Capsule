//! The document says what the server sends (`S-C38`).

use super::*;

/// The document, as the committed contract is generated from it.
fn document() -> Document {
    crate::openapi().expect("the router describes itself")
}

/// Every problem response in the document is a *Capsule* problem, and every Capsule problem
/// requires `code`.
///
/// Over the document rather than per handler, for the same reason `S-C34` gates the document: a
/// per-handler assertion passes on the operations somebody remembered.
#[test]
fn every_problem_response_declares_the_code_a_client_localizes() {
    let document = document();
    let json = serde_json::to_value(&document).expect("a document serializes");
    let schemas = &json["components"]["schemas"];

    let mut checked = 0_usize;
    for (path, item) in json["paths"].as_object().expect("paths") {
        for (method, operation) in item.as_object().expect("a path item") {
            let Some(responses) = operation.get("responses").and_then(|r| r.as_object()) else {
                continue;
            };
            for (status, response) in responses {
                let Some(media) = response.pointer("/content/application~1problem+json") else {
                    continue;
                };
                let reference = media["schema"]["$ref"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{method} {path} {status}: no problem schema ref"));
                let name = reference
                    .strip_prefix("#/components/schemas/")
                    .expect("a local component reference");
                assert_ne!(
                    name, BASE,
                    "{method} {path} {status} points at the framework's bare problem, so the \
                     `code` a client is supposed to switch on is invisible in the contract"
                );
                let required = schemas[name]["required"]
                    .as_array()
                    .expect("a required list");
                assert!(
                    required
                        .iter()
                        .any(|member| member == crate::problem::CODE_MEMBER),
                    "{name} does not require `code`"
                );
                assert_eq!(
                    schemas[name]["properties"]["code"]["type"], "string",
                    "{name} describes `code` as something other than a string"
                );
                checked += 1;
            }
        }
    }

    assert!(
        checked > 50,
        "only {checked} problem responses were found, which means this walk stopped seeing the \
         document rather than that the surface shrank"
    );
}

/// Every row of the table names a response that exists.
///
/// The table is a second statement of a fact that lives in the Rust types, so it can go stale in
/// two directions. This catches one of them — a row naming an operation that was renamed, or a
/// status that stopped being declared — and the module docs say plainly that nothing catches the
/// other.
#[test]
fn every_row_names_a_response_that_exists() {
    let json = serde_json::to_value(document()).expect("a document serializes");

    for extra in EXTRAS {
        let mut found = false;
        for item in json["paths"].as_object().expect("paths").values() {
            for operation in item.as_object().expect("a path item").values() {
                if operation["operationId"] != serde_json::json!(extra.operation) {
                    continue;
                }
                let key = extra.status.to_string();
                found = operation
                    .pointer(&format!(
                        "/responses/{key}/content/application~1problem+json"
                    ))
                    .is_some();
                assert!(
                    found,
                    "`{}` declares no {} problem response, so this row is stale",
                    extra.operation, extra.status
                );
            }
        }
        assert!(
            found,
            "no operation is called `{}`, so this row is stale",
            extra.operation
        );
    }
}

/// The extension members really reach the document.
#[test]
fn the_structured_members_a_client_merges_on_are_described() {
    let json = serde_json::to_value(document()).expect("a document serializes");
    let schemas = &json["components"]["schemas"];

    for extra in EXTRAS {
        for member in extra.members {
            let described = &schemas[extra.component]["properties"][member.name];
            assert!(
                !described.is_null(),
                "{} does not describe `{}`",
                extra.component,
                member.name
            );
            if member.nullable {
                assert_eq!(
                    described["type"],
                    serde_json::json!([member.json_type, "null"]),
                    "{}.{} is absent on some variants, so it has to be nullable",
                    extra.component,
                    member.name
                );
            } else {
                assert_eq!(described["type"], serde_json::json!(member.json_type));
            }
        }
    }

    // The one `S-C22` names by hand, spelled out because it is the member the SDK's merge path
    // switches on and the whole reason that slice noticed this gap.
    assert_eq!(
        schemas["DuplicateBlobProblem"]["properties"]["existing_asset"]["type"],
        "string"
    );
}

/// The Capsule problems are derived from the framework's, not restated beside it.
///
/// If Kynos changes what a problem carries, these follow — which is the property that keeps this
/// module from becoming a second, drifting definition of an RFC 9457 body.
#[test]
fn the_capsule_problems_carry_everything_the_base_one_does() {
    let json = serde_json::to_value(document()).expect("a document serializes");
    let schemas = &json["components"]["schemas"];
    let base = schemas[BASE]["properties"].as_object().expect("properties");

    for name in std::iter::once(CODED).chain(EXTRAS.iter().map(|extra| extra.component)) {
        let derived = schemas[name]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} has no properties"));
        for (member, schema) in base {
            assert_eq!(
                derived.get(member),
                Some(schema),
                "{name} lost or changed the base problem's `{member}`"
            );
        }
        for member in schemas[BASE]["required"].as_array().expect("required") {
            assert!(
                schemas[name]["required"]
                    .as_array()
                    .expect("required")
                    .contains(member),
                "{name} dropped the base problem's required `{member}`"
            );
        }
    }
}
