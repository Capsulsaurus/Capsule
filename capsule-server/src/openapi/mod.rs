//! Describing the extension members Capsule's problems actually carry (slice `S-C38`).
//!
//! # The regression this closes
//!
//! Kynos's `#[problem(extension)]` is a **runtime** attachment: the derive expands it to
//! `problem.with_extension(name, value)` and nothing feeds it into the emitted schema. So every
//! problem response in `openapi.json` referenced one generic `Problem` with
//! `additionalProperties: true` and **zero** occurrences of `code` — while every rejection the
//! server renders carries one, and the `409` on `POST /v1/upload` carries an `existing_asset` a
//! client is supposed to merge on.
//!
//! That is not a cosmetic omission. Capsule's whole i18n design is that the server sends a
//! stable `error.*` code and the client localizes it **offline** — and a generated client is
//! generated from this document. The field it is supposed to switch on was not in the contract
//! it was built from, which made the client half of the i18n contract unreachable except by
//! reaching past the generated type. It was also a **regression against the surface being
//! retired**: the Salvo document described `code` in six places.
//!
//! `S-C28` was statuses the code returns and the schema omits, and Kynos makes that
//! unrepresentable because status is part of the return type. This is *members* the code returns
//! and the schema omits, which Kynos does not make unrepresentable — so the class the rebuild
//! claimed to close was only half closed.
//!
//! # What this does
//!
//! Rewrites the document after Kynos builds it: every `application/problem+json` response points
//! at a Capsule problem schema instead of the bare `Problem`, and those schemas are **derived
//! from `Problem` itself** rather than restated — the base is read out of the components the
//! router just emitted, cloned, and extended. If Kynos changes what a problem contains, these
//! follow.
//!
//! Two kinds of member:
//!
//! - **`code` is universal and required.** Every rejection this crate owns declares one, and
//!   since `S-C36` every rejection it does *not* own is given one by
//!   [`crate::problem::CodedProblems`]. The two slices are one change: this one makes the
//!   document claim it, that one makes the claim true for the framework's own responses.
//! - **Everything else is per-response and optional**, listed in [`EXTRAS`]. Optional because a
//!   status carries one response in a description while an enum may reach it from several
//!   variants — `ChunkRejection`'s `409` is an offset mismatch *or* a plain conflict — so a
//!   member one variant renders is a member the response *may* carry.
//!
//! # The honest limit
//!
//! [`EXTRAS`] is a table, and a table is a second statement of a fact that lives in the type. A
//! new `#[problem(extension)]` field that is not `code` will not appear in the document until
//! somebody adds a row, and nothing here fails when they forget.
//!
//! Three things bound that. The table is small — six rows against the non-`code` fields
//! across the rejection enums — and `every_row_names_a_response_that_exists` fails on a row that has gone
//! stale, so it cannot rot in the other direction. The `code` member, which is the one the i18n
//! contract turns on and 104 of the 120 extension fields on this surface, needs no table at all.
//! And the real fix is upstream: `#[problem(extension)]` should carry a schema, which is the
//! same seam `S-C36` wanted for framework rejections. Recorded on `S-C38` rather than papered
//! over.
//!
//! # Why the served document is not this one
//!
//! [`crate::service`] builds its own description and Kynos exposes no way to edit a built
//! service's document, so `assert_conformance` validates bodies against the *untransformed*
//! schema. That direction is safe — this transform only adds properties and one required member,
//! so anything conforming to the published schema conforms to the looser one — but it does mean
//! the walk cannot catch a response that fails to carry `code`. `tests/problem.rs` asserts that
//! directly instead, over the statuses the framework renders.

use kynos::openapi::{Document, Schema};

/// The media type a problem is served as, as the document spells it.
const PROBLEM_JSON: &str = "application/problem+json";

/// The media types Capsule carries as raw bytes.
///
/// `application/octet-stream` is ciphertext and staged upload chunks; `application/cbor` is the
/// signed documents that are served byte-for-byte — a device directory, a custody receipt, an
/// upgrade intent, a wrapped master key. Neither has a JSON shape and neither is ever decoded by
/// the transport.
const RAW_BYTE_MEDIA: &[&str] = &["application/octet-stream", "application/cbor"];

/// The component name Kynos emits for its own problem type.
const BASE: &str = "Problem";

/// The schema every problem response points at unless [`EXTRAS`] says otherwise.
const CODED: &str = "CodedProblem";

/// One member a specific response may carry beyond `code`.
struct Member {
    /// The member's name on the wire.
    name: &'static str,
    /// Its JSON Schema type.
    json_type: &'static str,
    /// What it means, for the client that has to act on it.
    description: &'static str,
    /// Whether the member may be `null` — the shape a `Option<String>` extension renders as.
    nullable: bool,
}

/// A problem shape beyond the universal one.
struct Extra {
    /// The component name this shape is published under.
    component: &'static str,
    /// The operation that renders it.
    operation: &'static str,
    /// The status it renders it at.
    status: u16,
    /// The members it adds.
    members: &'static [Member],
}

/// Every response that carries an extension member beyond `code`.
///
/// See the module docs for why this is a table and what bounds the risk of one.
const EXTRAS: &[Extra] = &[
    Extra {
        component: "ProtocolRangeProblem",
        operation: "album_lifecycle_op",
        status: 426,
        members: PROTOCOL_RANGE,
    },
    Extra {
        component: "DuplicateBlobProblem",
        operation: "create_upload",
        status: 409,
        members: &[Member {
            name: "existing_asset",
            json_type: "string",
            description: "The asset already holding these exact bytes in the same album. \
                          Structured so a client merges rather than re-parsing a sentence \
                          (slice `S-C22`).",
            nullable: false,
        }],
    },
    Extra {
        component: "OffsetMismatchProblem",
        operation: "append_chunk",
        status: 409,
        members: &[Member {
            name: "offset",
            json_type: "integer",
            description: "The offset the server is actually at, so a client resumes from it \
                          instead of asking again.",
            nullable: false,
        }],
    },
    Extra {
        component: "DirectoryConflictProblem",
        operation: "publish_device_directory",
        status: 409,
        members: &[
            Member {
                name: "submitted",
                json_type: "integer",
                description: "The directory version the request carried.",
                nullable: false,
            },
            Member {
                name: "stored",
                json_type: "integer",
                description: "The version the server holds. A client re-signs above this one.",
                nullable: false,
            },
        ],
    },
    Extra {
        component: "StaleRevivalProblem",
        operation: "album_lifecycle_op",
        status: 409,
        members: &[Member {
            name: "chain_head",
            json_type: "string",
            description: "The manifest hash the asset's chain is actually at. Absent when the \
                          conflict is not a chain conflict, which is why it is nullable.",
            nullable: true,
        }],
    },
    Extra {
        component: "FileTooLargeProblem",
        operation: "create_drop",
        status: 413,
        members: &[Member {
            name: "limit",
            json_type: "integer",
            description: "The largest file this drop link accepts, in bytes.",
            nullable: false,
        }],
    },
];

/// The protocol window a **body-level** `426` publishes as extension members.
///
/// One row is left: `album_lifecycle_op` refuses a manifest envelope pinned outside the window
/// and still renders the range in the body. The four upload operations no longer do — since
/// issue #404 the window rides `X-Capsule-Protocol-Min`/`-Max` on every response, header-gated
/// and body-gated `426`s alike, which is where the SDK reads it; a second spelling in the body
/// is the drift the census exists to prevent. The remaining row goes when `routes/ops.rs`
/// drops its members.
const PROTOCOL_RANGE: &[Member] = &[
    Member {
        name: "protocol_min",
        json_type: "string",
        description: "The oldest protocol date this server still speaks (`YYYY-MM-DD`).",
        nullable: false,
    },
    Member {
        name: "protocol_max",
        json_type: "string",
        description: "The newest protocol date this server speaks (`YYYY-MM-DD`).",
        nullable: false,
    },
];

/// Points every problem response at a schema that describes its extension members.
///
/// Does nothing if the document carries no `Problem` component, which is the state a document
/// with no failing operation would be in — an empty router rather than an error.
/// Gives every raw-byte body and response an explicit binary schema (`S-Z7`).
///
/// # What Kynos emits, and why a generator cannot use it
///
/// A `Binary<M>` body or a `Served` response is described as `"schema": {}` — the empty schema,
/// which in JSON Schema means *any instance* and is the idiomatic 3.1 spelling for "these are
/// just bytes". It is not wrong. It is also not actionable: `spargen` refuses a raw-byte media
/// type whose schema it cannot recognise as byte-shaped, because the alternative is guessing —
/// and guessing wrong on a ciphertext body means decoding a blob as UTF-8.
///
/// So the empty schema is filled in with the marker every OpenAPI generator recognises:
/// `{"type": "string", "format": "binary"}`. This **adds** description rather than removing any:
/// the set of instances is unchanged, and a reader who ignores `format` sees the same document.
///
/// `contentEncoding: base64` would also satisfy the generator and would be a **lie** — these
/// bodies are raw octets on the wire, not base64 text, and a client that believed the annotation
/// would decode every blob to nothing.
///
/// Only an *empty* schema is filled. A media object that already describes its payload is left
/// exactly as the router emitted it, so this can never overwrite a real declaration.
///
/// Owed upstream, like `S-C36`'s and `S-C38`'s seams: Kynos knows the body is bytes — that is
/// what `Binary<M>` means — so it is the natural place to say so.
pub(crate) fn describe_raw_byte_payloads(document: &mut Document) {
    for item in document.paths.items.values_mut() {
        let slots: Vec<&mut Option<Box<kynos::openapi::Operation>>> = vec![
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.options,
            &mut item.head,
            &mut item.patch,
            &mut item.trace,
            &mut item.query,
        ];
        for operation in slots.into_iter().filter_map(|slot| slot.as_deref_mut()) {
            if let Some(kynos::openapi::RefOr::Item(body)) = operation.request_body.as_mut() {
                for media in RAW_BYTE_MEDIA {
                    if let Some(entry) = body.content.get_mut(*media) {
                        fill_binary(&mut entry.schema);
                    }
                }
            }
            for response in operation.responses.responses.values_mut() {
                let kynos::openapi::RefOr::Item(response) = response else {
                    continue;
                };
                for media in RAW_BYTE_MEDIA {
                    if let Some(entry) = response.content.get_mut(*media) {
                        fill_binary(&mut entry.schema);
                    }
                }
            }
        }
    }
}

/// The binary marker, when the media object carries no schema of its own.
fn fill_binary(schema: &mut Option<Schema>) {
    // No schema at all, `true`, and an object with no keywords are the three spellings of "any
    // instance"; Kynos emits the last. Anything else is a real declaration and is left alone.
    let empty = match schema {
        None | Some(Schema::Bool(true)) => true,
        Some(Schema::Object(object)) => **object == kynos::openapi::SchemaObject::default(),
        Some(Schema::Bool(false)) => false,
    };
    if !empty {
        return;
    }
    *schema = Some(
        serde_json::from_value(serde_json::json!({
            "type": "string",
            "format": "binary",
        }))
        .expect("a literal binary schema is a schema"),
    );
}

/// Files the protocol window's three response headers under every response (issue #404).
///
/// [`crate::negotiation::Negotiation`] attaches `X-Capsule-Protocol-Min`, `-Max` and
/// `X-Capsule-Min-Client-Build` to **every** response it forwards, and it forwards everything —
/// a short-circuit from an inner interceptor, an extractor's rejection, a handler's answer.
/// Kynos describes an interceptor's `Adds` at `StatusPattern::Success` only
/// (`kynos/src/middleware/erased.rs`), so without this the document would promise the headers
/// on a `200` and stay silent on the `426` where a client most needs them.
///
/// The declarations come from [`crate::negotiation::response_header_declarations`] — the same
/// source the interceptor's own description uses — and a response that already declares a
/// header under one of these names is left exactly as the router emitted it, so this can never
/// overwrite what Kynos said.
pub(crate) fn describe_negotiation_headers(document: &mut Document) {
    let declarations = crate::negotiation::response_header_declarations();
    for item in document.paths.items.values_mut() {
        let slots: Vec<&mut Option<Box<kynos::openapi::Operation>>> = vec![
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.options,
            &mut item.head,
            &mut item.patch,
            &mut item.trace,
            &mut item.query,
        ];
        for operation in slots.into_iter().filter_map(|slot| slot.as_deref_mut()) {
            let responses = operation
                .responses
                .responses
                .values_mut()
                .chain(operation.responses.default_response.iter_mut());
            for response in responses {
                let kynos::openapi::RefOr::Item(response) = response else {
                    continue;
                };
                for (name, header) in &declarations {
                    let declared = response
                        .headers
                        .keys()
                        .any(|existing| existing.eq_ignore_ascii_case(name));
                    if !declared {
                        response.headers.insert(
                            (*name).to_owned(),
                            kynos::openapi::RefOr::Item(header.clone()),
                        );
                    }
                }
            }
        }
    }
}

pub(crate) fn describe_problem_extensions(document: &mut Document) {
    let Some(base) = document.components.schemas.get(BASE).cloned() else {
        return;
    };

    document
        .components
        .schemas
        .insert(CODED.to_owned(), extended(&base, CODED, &[]));
    for extra in EXTRAS {
        document.components.schemas.insert(
            extra.component.to_owned(),
            extended(&base, extra.component, extra.members),
        );
    }

    for item in document.paths.items.values_mut() {
        let slots: Vec<&mut Option<Box<kynos::openapi::Operation>>> = vec![
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.options,
            &mut item.head,
            &mut item.patch,
            &mut item.trace,
            &mut item.query,
        ];
        for operation in slots.into_iter().filter_map(|slot| slot.as_deref_mut()) {
            point_at_capsule_problems(operation);
        }
    }
}

/// Repoints one operation's problem responses.
fn point_at_capsule_problems(operation: &mut kynos::openapi::Operation) {
    let operation_id = operation.operation_id.clone().unwrap_or_default();
    for (key, response) in &mut operation.responses.responses {
        let kynos::openapi::RefOr::Item(response) = response else {
            // A referenced response is a component this crate does not emit; nothing in
            // Capsule's surface produces one, and rewriting through a reference would edit
            // every operation sharing it rather than this one.
            continue;
        };
        let Some(media) = response.content.get_mut(PROBLEM_JSON) else {
            continue;
        };
        let component = key
            .parse::<u16>()
            .ok()
            .and_then(|status| {
                EXTRAS
                    .iter()
                    .find(|extra| extra.operation == operation_id && extra.status == status)
            })
            .map_or(CODED, |extra| extra.component);
        media.schema = Some(reference(component));
    }
}

/// A `$ref` to a component schema.
fn reference(component: &str) -> Schema {
    Schema::Object(Box::new(kynos::openapi::SchemaObject {
        reference: Some(format!("#/components/schemas/{component}")),
        ..Default::default()
    }))
}

/// `Problem`, plus a required `code` and whatever `members` adds.
///
/// Derived from the emitted base rather than restated, so a change to what Kynos puts in a
/// problem reaches these without anyone editing this file.
fn extended(base: &Schema, title: &str, members: &[Member]) -> Schema {
    let Schema::Object(base) = base else {
        return base.clone();
    };
    let mut object = (**base).clone();
    object.title = Some(title.to_owned());

    object.properties.insert(
        crate::problem::CODE_MEMBER.to_owned(),
        member_schema(
            "string",
            false,
            "The stable `error.*` catalog code. The client localizes this; `detail` stays \
             English. Present on every problem this server renders.",
        ),
    );
    let required = object.required.get_or_insert_with(Vec::new);
    if !required
        .iter()
        .any(|name| name == crate::problem::CODE_MEMBER)
    {
        required.push(crate::problem::CODE_MEMBER.to_owned());
    }

    for member in members {
        object.properties.insert(
            member.name.to_owned(),
            member_schema(member.json_type, member.nullable, member.description),
        );
    }

    Schema::Object(Box::new(object))
}

/// One member's schema.
fn member_schema(json_type: &str, nullable: bool, description: &str) -> Schema {
    let types = if nullable {
        serde_json::json!([json_type, "null"])
    } else {
        serde_json::json!(json_type)
    };
    serde_json::from_value(serde_json::json!({
        "type": types,
        "description": description,
    }))
    .expect("a literal member schema is a schema")
}

#[cfg(test)]
mod tests;
