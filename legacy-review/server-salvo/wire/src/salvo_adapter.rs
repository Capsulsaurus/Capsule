//! The Salvo adapter: one declaration per response enum, two impls generated from it.
//!
//! # Why a macro and not a blanket impl
//!
//! `Writer` and `EndpointOutRegister` are foreign traits and the response enums are foreign
//! types (they live in the server crates), so no adapter crate can implement one for the
//! other — the orphan rule forbids it. A macro sidesteps this without importing the
//! framework: the expansion *names* `::salvo::…` paths, which resolve at the expansion site
//! inside the server crates, so this crate stays free of every framework dependency and
//! survives the transport swap untouched.
//!
//! # The declaration
//!
//! ```text
//! capsule_wire::salvo_responses! {
//!     LoginResponses {
//!         Success(tokens) => 200, json(tokens), doc("Success", schema = TokenResponse);
//!         BadRequest {}   => 400, json(ApiError::new("Invalid request")), doc("Bad request");
//!         RateLimited(s)  => 429, retry_after(s) json(ApiError::new("Slow down")), undocumented();
//!         Unexpected(e)   =>   _, delegate(e), undocumented();
//!     }
//!     delegated {
//!         500 => "Internal server error",
//!     }
//! }
//! ```
//!
//! Each row is `Variant(bindings) => status, body…, documentation;`. The bindings are always
//! delimited — `Variant(x)`, `Variant { x, y }`, or `Variant {}` for a unit variant — so the
//! row parses without ambiguity.
//!
//! - **status** is a plain `u16`, or `_` when the row delegates to a nested taxonomy that
//!   picks the status itself. A status outside the HTTP range fails the build.
//! - **body** is one or more of `json(expr)`, `text(expr)`, `empty()`, `delegate(expr)`,
//!   `header(name, value)`, `header_option(name, optional_value)`, `retry_after(seconds)`, or
//!   `custom { |res| … }` (an escape hatch that names the response itself), applied in the
//!   order written.
//! - **documentation** is `doc("…")`, `doc("…", schema = Type)`, or `undocumented()` — the last
//!   one records an outcome the published document deliberately omits, so the gap is visible
//!   in the table instead of being invisible in a second impl.
//! - the optional trailing `delegated { status => "…" }` block documents the statuses that
//!   reach the wire through a `delegate(…)` row.
//!
//! The macro emits `impl Writer` (the wire behaviour), `impl EndpointOutRegister` (the
//! OpenAPI document), and `impl WireResponses` (the neutral [`crate::ResponseSpec`] table
//! both are derived from, and the one a non-Salvo server reads).

/// Declare a response taxonomy once and generate the Salvo `Writer` +
/// `EndpointOutRegister` impls plus the neutral [`crate::WireResponses`] table from it.
///
/// See the [module documentation](self) for the grammar.
#[macro_export]
macro_rules! salvo_responses {
    (
        $ty:ident {
            $(
                $variant:ident $fields:tt => $status:tt,
                $($body_kind:ident $body_args:tt)+ ,
                $doc_kind:ident $doc_args:tt
            );+ $(;)?
        }
        delegated {
            $( $delegated_status:literal => $delegated_description:literal ),* $(,)?
        }
    ) => {
        impl $crate::WireResponses for $ty {
            const RESPONSES: &'static [$crate::ResponseSpec] = &[
                $(
                    $crate::ResponseSpec {
                        status: $crate::__wire_status_value!($status),
                        body: $crate::__wire_body_shape!($($body_kind $body_args)+),
                        description: $crate::__wire_description!($doc_kind $doc_args),
                        schema: $crate::__wire_schema_name!($doc_kind $doc_args),
                    },
                )+
                $(
                    $crate::ResponseSpec {
                        status: ::core::option::Option::Some($delegated_status),
                        body: $crate::BodyShape::Delegated,
                        description: ::core::option::Option::Some($delegated_description),
                        schema: ::core::option::Option::None,
                    },
                )*
            ];
        }

        #[::salvo::async_trait]
        impl ::salvo::Writer for $ty {
            async fn write(
                self,
                req: &mut ::salvo::Request,
                depot: &mut ::salvo::Depot,
                res: &mut ::salvo::Response,
            ) {
                let _ = (&mut *req, &mut *depot);
                match self {
                    $(
                        Self::$variant $fields => {
                            $crate::__wire_apply_status!(res, $status);
                            $(
                                $crate::__wire_write_body!(req, depot, res, $body_kind $body_args);
                            )+
                        }
                    )+
                }
            }
        }

        impl ::salvo::oapi::EndpointOutRegister for $ty {
            fn register(
                components: &mut ::salvo::oapi::Components,
                operation: &mut ::salvo::oapi::Operation,
            ) {
                let _ = &mut *components;
                for spec in <$ty as $crate::WireResponses>::RESPONSES {
                    if let (Some(status), Some(description)) = (spec.status, spec.description) {
                        operation.responses.insert(
                            ::std::string::ToString::to_string(&status),
                            ::salvo::oapi::Response::new(description),
                        );
                    }
                }
                $(
                    $crate::__wire_register_schema!(
                        components, operation, $status, $doc_kind $doc_args
                    );
                )+
            }
        }
    };

    // No delegated statuses: the common case, forwarded to the full rule.
    ($ty:ident { $($rows:tt)* }) => {
        $crate::salvo_responses! { $ty { $($rows)* } delegated {} }
    };
}

/// The declared status of one row, as neutral data.
#[doc(hidden)]
#[macro_export]
macro_rules! __wire_status_value {
    (_) => {
        ::core::option::Option::None
    };
    ($status:literal) => {
        ::core::option::Option::Some($status)
    };
}

/// The body shape of one row: the first body item that produces a body wins.
#[doc(hidden)]
#[macro_export]
macro_rules! __wire_body_shape {
    (json $args:tt $($rest:tt)*) => { $crate::BodyShape::Json };
    (text $args:tt $($rest:tt)*) => { $crate::BodyShape::Text };
    (delegate $args:tt $($rest:tt)*) => { $crate::BodyShape::Delegated };
    (custom $args:tt $($rest:tt)*) => { $crate::BodyShape::Opaque };
    (empty $args:tt $($rest:tt)*) => { $crate::BodyShape::Empty };
    ($other:ident $args:tt $($rest:tt)*) => { $crate::__wire_body_shape!($($rest)*) };
    () => { $crate::BodyShape::Empty };
}

/// The published description of one row, or `None` when it is deliberately undocumented.
#[doc(hidden)]
#[macro_export]
macro_rules! __wire_description {
    (undocumented ()) => {
        ::core::option::Option::None
    };
    (doc ($description:literal $(, schema = $schema:ty)? $(,)?)) => {
        ::core::option::Option::Some($description)
    };
}

/// The payload schema name of one row, when it carries a typed JSON body.
#[doc(hidden)]
#[macro_export]
macro_rules! __wire_schema_name {
    (undocumented ()) => {
        ::core::option::Option::None
    };
    (doc ($description:literal $(,)?)) => {
        ::core::option::Option::None
    };
    (doc ($description:literal, schema = object $(,)?)) => {
        ::core::option::Option::Some("object")
    };
    (doc ($description:literal, schema = $schema:ty $(,)?)) => {
        ::core::option::Option::Some(::core::stringify!($schema))
    };
}

/// Set the row's status on the response. A status outside the HTTP range fails the build.
#[doc(hidden)]
#[macro_export]
macro_rules! __wire_apply_status {
    ($res:ident, _) => {};
    ($res:ident, $status:literal) => {
        const { ::core::assert!($crate::is_valid_status($status)) };
        if let ::core::result::Result::Ok(code) = ::salvo::http::StatusCode::from_u16($status) {
            $res.status_code(code);
        }
    };
}

/// Render one body item onto the response.
#[doc(hidden)]
#[macro_export]
macro_rules! __wire_write_body {
    ($req:ident, $depot:ident, $res:ident, json ($($payload:tt)*)) => {
        $res.render(::salvo::writing::Json($($payload)*));
    };
    ($req:ident, $depot:ident, $res:ident, text ($($payload:tt)*)) => {
        $res.render(::salvo::writing::Text::Plain($($payload)*));
    };
    ($req:ident, $depot:ident, $res:ident, empty ()) => {};
    ($req:ident, $depot:ident, $res:ident, delegate ($($inner:tt)*)) => {
        ::salvo::Writer::write($($inner)*, $req, $depot, $res).await;
    };
    ($req:ident, $depot:ident, $res:ident, header ($name:expr, $value:expr $(,)?)) => {
        let _ = $res.add_header($name, $value, true);
    };
    ($req:ident, $depot:ident, $res:ident, header_option ($name:expr, $value:expr $(,)?)) => {
        if let ::core::option::Option::Some(value) = $value {
            let _ = $res.add_header($name, value, true);
        }
    };
    ($req:ident, $depot:ident, $res:ident, retry_after ($seconds:expr)) => {
        if let ::core::result::Result::Ok(value) =
            ::std::string::ToString::to_string(&$seconds).parse()
        {
            $res.headers_mut()
                .insert(::salvo::http::header::RETRY_AFTER, value);
        }
    };
    ($req:ident, $depot:ident, $res:ident, custom { |$binding:ident| $($statements:tt)* }) => {{
        let $binding = &mut *$res;
        $($statements)*
    }};
}

/// Attach the payload schema to a documented row. Expands to nothing for rows without one.
#[doc(hidden)]
#[macro_export]
macro_rules! __wire_register_schema {
    ($components:ident, $operation:ident, $status:tt, doc ($description:literal, schema = object $(,)?)) => {
        $operation.responses.insert(
            ::std::string::ToString::to_string(&$status),
            ::salvo::oapi::Response::new($description).add_content(
                "application/json",
                ::salvo::oapi::Content::new(::salvo::oapi::Object::new()),
            ),
        );
    };
    ($components:ident, $operation:ident, $status:tt, doc ($description:literal, schema = $schema:ty $(,)?)) => {
        $operation.responses.insert(
            ::std::string::ToString::to_string(&$status),
            ::salvo::oapi::Response::new($description).add_content(
                "application/json",
                ::salvo::oapi::Content::new(<$schema as ::salvo::oapi::ToSchema>::to_schema(
                    $components,
                )),
            ),
        );
    };
    ($components:ident, $operation:ident, $status:tt, $($rest:tt)*) => {};
}
