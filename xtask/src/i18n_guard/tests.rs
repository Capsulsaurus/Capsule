use super::*;

fn texts(findings: &[Finding]) -> Vec<&str> {
    findings.iter().map(|f| f.text.as_str()).collect()
}

#[test]
fn web_flags_jsx_text_and_attributes() {
    let src = r#"
            <h1>Dashboard</h1>
            <input placeholder="Search photos" />
            <img alt="Asset" />
        "#;
    let f = web_findings(src);
    assert!(texts(&f).contains(&"Dashboard"));
    assert!(texts(&f).contains(&"Search photos"));
    assert!(texts(&f).contains(&"Asset"));
}

#[test]
fn web_catches_multiline_text() {
    let src = "<CardTitle>\n    Storage Used\n</CardTitle>";
    let f = web_findings(src);
    assert_eq!(texts(&f), vec!["Storage Used"]);
    assert_eq!(f[0].line, 1);
}

#[test]
fn web_ignores_migrated_and_expressions() {
    // FormattedMessage self-closes; interpolation lives in `{}`.
    let src = r#"
            <FormattedMessage id="dashboard.title" />
            <p>{intl.formatMessage({ id: 'x' })}</p>
            <Button>{loading ? spinner : label}</Button>
            <div className="text-sm" data-slot="x" />
            <Input placeholder={intl.formatMessage({ id: 'auth.email' })} />
        "#;
    assert_eq!(web_findings(src), Vec::new());
}

#[test]
fn web_ignores_generics_and_comparisons() {
    let src = "const x = useState<string>('');\nif (a > b) { return null; }\n";
    assert_eq!(web_findings(src), Vec::new());
}

#[test]
fn swift_flags_literal_but_not_verbatim_or_dynamic() {
    let src = r#"Text("Settings")
            Text("app.settings.title")
            Text(verbatim: "raw")
            Text(model.title)
            .navigationTitle("Albums")
        "#;
    let f = swift_findings(src);
    let t = texts(&f);
    assert!(t.contains(&"Settings"));
    assert!(t.contains(&"Albums"));
    // The dotted key IS captured; the runner filters it against the catalog.
    assert!(t.contains(&"app.settings.title"));
    // `verbatim:` and dynamic args are never captured.
    assert!(!t.contains(&"raw"));
}

#[test]
fn swift_ignores_helper_names_ending_in_a_watched_name() {
    // `barButton` ends in `Button` but is a custom helper taking an SF Symbol
    // name — the word boundary keeps it out, for the interpolation detector as
    // much as for the literal one.
    let src = r#"barButton("square.and.arrow.up", action: share)
            myText("not.matched")
            barButton("Delete \(count) Items", role: .destructive) {}
        "#;
    assert_eq!(swift_findings(src), Vec::new());
}

#[test]
fn swift_flags_an_interpolated_literal_in_a_watched_position() {
    // The plain-literal blind spot, closed by the interpolation detector.
    let src = r#"Text("Delete \(count) Items")
            .confirmationDialog("Delete \(count) Items?", isPresented: $flag) {}
        "#;
    let f = swift_findings(src);
    let t = texts(&f);
    assert!(t.contains(&r"Delete \(count) Items"), "{t:?}");
    assert!(t.contains(&r"Delete \(count) Items?"), "{t:?}");
}

#[test]
fn swift_ignores_a_migrated_interpolated_call_site() {
    // The migrated shape passes `String(localized:defaultValue:)`, so the watched
    // position holds no literal at all and the `defaultValue:` text is not captured.
    let src = r#"Button(
                String(
                    localized: "app.timeline.delete_selected.confirm",
                    defaultValue: "Delete \(count) Items"
                ),
                role: .destructive
            ) {}
        "#;
    let f = swift_findings(src);
    // Only the catalog key is captured; the runner then matches it against the catalog.
    assert_eq!(texts(&f), vec!["app.timeline.delete_selected.confirm"]);
}

#[test]
fn swift_flags_a_string_localized_key_that_is_not_a_catalog_key() {
    let src = r#"String(localized: "Photos")
            String(localized: "app.media.photo")
        "#;
    let f = swift_findings(src);
    let t = texts(&f);
    // Both are captured; the runner keeps only the one that is not a catalog key.
    assert!(t.contains(&"Photos"));
    assert!(t.contains(&"app.media.photo"));
}

#[test]
fn kotlin_flags_text_and_content_description() {
    let src = r#"Text("Hello")
            Text(stringResource(R.string.no_data_available))
            Text(obj.title)
            Icon(x, contentDescription = "Back arrow")
            Icon(x, contentDescription = obj.title)
        "#;
    let f = kotlin_findings(src);
    let t = texts(&f);
    assert!(t.contains(&"Hello"));
    assert!(t.contains(&"Back arrow"));
    assert_eq!(t.len(), 2, "stringResource and dynamic args are ignored");
}

#[test]
fn swift_computed_properties_are_scanned() {
    // The shape that hid twenty-two English strings: display text returned
    // from a `String` property, reaching the screen through a variable so
    // no call site ever carries the literal.
    let src = r#"
            var title: String {
                switch self {
                case .places: "Places and Trips"
                case .people: "People and Pets"
                }
            }
        "#;
    let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
    assert!(texts.contains(&"Places and Trips".to_string()));
    assert!(texts.contains(&"People and Pets".to_string()));
}

#[test]
fn swift_computed_properties_ignore_identifier_like_values() {
    // A `String` property is not automatically display text. Symbol names, raw values
    // and file formats do not spell a lowercase letter in position two; a `String`
    // property that is not named like display text is not scanned at all.
    let src = r#"
            var title: String {
                switch self {
                case .heic: "HEIC"
                case .dng: "DNG"
                case .hdr10: "HDR10"
                case .hlg: "HLG"
                case .photo: "app.media.photo"
                case .masterKey: "key.fill"
                }
            }
            var systemImage: String {
                switch self {
                case .places: "Map Pin Icon"
                }
            }
        "#;
    assert_eq!(swift_findings(src), Vec::new());
}

#[test]
fn swift_computed_properties_catch_a_single_word_string() {
    // Issue #394: the detector's own documented example. The literal rule used to
    // require a space, so `case .places: "Places"` — the exact shape the doc comment
    // advertises as the motivating bug — could not be caught.
    let src = r#"
            var title: String {
                switch self {
                case .places: "Places"
                case .people: "People"
                }
            }
        "#;
    let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
    assert_eq!(texts, vec!["Places".to_string(), "People".to_string()]);
}

#[test]
fn swift_display_functions_are_scanned() {
    // `var` was required, so a `func` returning display text was invisible — the
    // blind spot that hid "Dolby Vision" in `AssetInfoFormatting.hdrName(_:)`.
    let src = r#"
            static func hdrName(_ format: HDRFormat) -> String {
                switch format {
                case .hdr10: "HDR10"
                case .dolbyVision: "Dolby Vision"
                case .hlg: "HLG"
                }
            }
        "#;
    let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
    assert_eq!(texts, vec!["Dolby Vision".to_string()]);
}

#[test]
fn swift_display_members_are_scanned_beyond_the_switch() {
    // A property returns display text in more shapes than a `switch`: an explicit
    // `return`, an implicit one, and a dictionary value. Only `case` was scanned.
    // `heading`, `summary` and `text` are also new stems — `var heading` was not
    // matched at all before.
    let src = r#"
            var heading: String {
                if isEmpty { return "Nothing here yet" }
                "Your library"
            }
            var summary: String {
                let names: [Kind: String] = [.places: "Places and trips"]
                return names[kind] ?? ""
            }
            var promptText: String {
                "Choose an album"
            }
        "#;
    let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
    assert_eq!(
        texts,
        vec![
            "Nothing here yet".to_string(),
            "Your library".to_string(),
            "Places and trips".to_string(),
            "Choose an album".to_string(),
        ]
    );
}

#[test]
fn swift_display_members_do_not_report_a_literal_twice() {
    // A `func` nested in a `var` body is scanned by both members, and the dictionary
    // and `case` patterns overlap. Findings are keyed by absolute offset, so the
    // literal is one violation, not two.
    let src = r#"
            var title: String {
                func headingFor(_ k: Kind) -> String {
                    return "Places and trips"
                }
                return headingFor(kind)
            }
        "#;
    let findings = swift_findings(src);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].text, "Places and trips");
}

#[test]
fn swift_display_members_ignore_a_localized_default_value() {
    // `String(localized:defaultValue:)` is the *migrated* shape: the key is checked
    // against the catalog and the default value is the English source the ICU
    // arguments hang off. Neither is a hardcoded literal, and the dictionary-value
    // pattern must not read `defaultValue:` as a dictionary key.
    let src = r#"
            var title: String {
                String(
                    localized: "app.timeline.delete_selected.confirm",
                    defaultValue: "Delete Items",
                    comment: "Confirm button"
                )
            }
        "#;
    let texts: Vec<String> = swift_computed_property_findings(src)
        .into_iter()
        .map(|f| f.text)
        .collect();
    assert_eq!(texts, Vec::<String>::new());
}

/// The watched positions, one per alternative of the shared list.
fn swift_text_positions() -> Vec<&'static str> {
    SWIFT_TEXT_POSITIONS.split('|').collect()
}

#[test]
fn every_watched_api_position_is_a_bare_identifier() {
    // The shared list is spliced into two regexes, so a stray space, an empty
    // alternative or a regex metacharacter in it would silently widen or break both.
    let positions = swift_text_positions();
    assert!(positions.len() >= 15, "{positions:?}");
    for position in &positions {
        assert!(
            !position.is_empty()
                && position
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "`{position}` is not a bare identifier"
        );
    }
    let mut unique = positions.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), positions.len(), "a position is listed twice");
}

#[test]
fn every_watched_api_position_is_caught_in_both_regexes() {
    // The fixture is *derived* from the shared list rather than written out, so a
    // position cannot be added without a case: `confirmationDialog` sat in the
    // interpolation regex and not the literal one for two slices (#394) precisely
    // because the two lists were maintained by hand. `searchable` and `tabItem` had
    // no fixture at all until this test, so a broken alternative would have been
    // silent.
    for position in swift_text_positions() {
        let literal = format!("Watched {position} text");
        let interpolated = format!("Watched {position} \\(count)");
        let src =
            format!("view\n    {position}(\"{literal}\")\n    {position}(\"{interpolated}\")\n");
        let findings = swift_findings(&src);
        let found = texts(&findings);
        assert!(
            found.contains(&literal.as_str()),
            "{position}: the plain literal was not caught, got {found:?}"
        );
        assert!(
            found.contains(&interpolated.as_str()),
            "{position}: the interpolated literal was not caught, got {found:?}"
        );
        // And the word boundary still holds, for both regexes: a helper that merely
        // ends in a watched name is not a watched position.
        let helper = format!("view\n    my{position}(\"{literal}\")\n");
        assert_eq!(
            swift_findings(&helper),
            Vec::new(),
            "{position}: the word boundary was lost"
        );
    }
}

#[test]
fn swift_watches_every_api_position_in_both_regexes() {
    // The derived fixture above proves each alternative matches; this one proves the
    // real call shapes do, with the arguments SwiftUI actually puts after the string.
    // `confirmationDialog` was in the interpolation regex and not the literal one;
    // `help`, `accessibilityValue`, `ContentUnavailableView` and `tabItem` were in
    // neither. One shared list now, so the two cannot disagree again.
    let src = r#"
            .help("Show the import log")
            ContentUnavailableView("No photos yet", systemImage: "photo")
            .accessibilityValue("Three of ten")
            .confirmationDialog("Delete this?", isPresented: $flag) {}
            .help("Imported \(count) files")
        "#;
    let f = swift_findings(src);
    let t = texts(&f);
    for expected in [
        "Show the import log",
        "No photos yet",
        "Three of ten",
        "Delete this?",
    ] {
        assert!(t.contains(&expected), "{expected} missing from {t:?}");
    }
    assert!(t.contains(&r"Imported \(count) files"), "{t:?}");
}

#[test]
fn the_committed_allowlist_parses_and_covers_only_known_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root");
    let entries = load_allowlist(root).expect("the committed allowlist parses");
    assert!(!entries.is_empty(), "the committed allowlist has entries");
    for (file, _) in &entries {
        assert!(
            !file.starts_with("capsule-cli/"),
            "the CLI is English-only and unscanned, so it has no allowlist entry: {file}"
        );
        assert!(
            root.join(file).exists(),
            "allowlist references a file that no longer exists: {file} \
                 (delete the entry when the file goes away)"
        );
    }
}

#[test]
fn line_numbers_are_one_based() {
    let src = "line1\nline2 Text(\"x\")\n";
    assert_eq!(line_of(src, 0), 1);
    assert_eq!(line_of(src, 6), 2);
}

#[test]
fn swift_key_parameters_are_scanned() {
    let src = r#"
        SettingsRow(titleKey: "app.settings.storage.title", labelKey: "app.common.done")
        EmptyState(emptyDescriptionKey: "app.import.plan.empty.description")
        "#;
    let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
    assert!(texts.contains(&"app.settings.storage.title".to_string()));
    assert!(texts.contains(&"app.common.done".to_string()));
    assert!(texts.contains(&"app.import.plan.empty.description".to_string()));
}

/// The whole point of widening the gate: a key nobody added to the catalog
/// used to compile, render as its own raw text, and fail nothing.
#[test]
fn swift_key_parameters_catch_a_key_that_was_never_added() {
    let src = r#"Row(titleKey: "app.settings.totally.made.up")"#;
    let texts: Vec<String> = swift_findings(src).into_iter().map(|f| f.text).collect();
    assert_eq!(texts, vec!["app.settings.totally.made.up".to_string()]);
}

/// A switch over an enum whose case ends in `Key` is not a call site. This
/// exact shape — `case .masterKey: "key.fill"` returning an SF Symbol —
/// was the first false positive the widened detector produced.
#[test]
fn swift_key_parameters_ignore_enum_case_patterns() {
    let src = r#"
        public var symbolName: String {
            switch self {
            case .masterKey: "key.fill"
            case .userIdentityKey: "person.badge.key.fill"
            }
        }
        "#;
    assert_eq!(swift_key_parameter_findings(src), Vec::new());
}

/// A parameter that merely contains "key" is not a catalog reference.
#[test]
fn swift_key_parameters_require_the_label_to_end_in_key() {
    let src = r#"Signer(publicKeyPem: "-----BEGIN PUBLIC KEY-----", keyring: "default")"#;
    assert_eq!(swift_key_parameter_findings(src), Vec::new());
}

#[test]
fn swift_error_codes_reads_the_raw_value_switch() {
    let src = r#"
        public var rawValue: String {
            switch self {
            case .protocolVersionUnsupported: "error.protocol.version_unsupported"
            case .authInvalidCredentials: "error.auth.invalid_credentials"
            case let .unknown(raw): raw
            }
        }
        "#;
    let codes = swift_error_codes(src);
    assert_eq!(codes.len(), 2);
    assert!(codes.contains("error.protocol.version_unsupported"));
    assert!(codes.contains("error.auth.invalid_credentials"));
}

/// The scanner must not mistake an ordinary catalog key for an error code —
/// the check's whole value is that its two sets are the *same* contract.
#[test]
fn swift_error_codes_ignores_ui_keys_and_prose() {
    let src = r#"
        Text("app.settings.title")
        // an error.something mentioned in a comment, unquoted
        Label("app.error.banner", systemImage: "x")
        "#;
    assert!(swift_error_codes(src).is_empty());
}

/// A real regression this guards: a typo'd raw value still compiles, still
/// round-trips through `init(rawValue:)`, and fails only at lookup time.
#[test]
fn swift_error_codes_captures_a_typo_verbatim() {
    let src = r#"case .authRateLimited: "error.auth.rate_limted""#;
    let codes = swift_error_codes(src);
    assert!(codes.contains("error.auth.rate_limted"));
}
