use super::*;

fn found(source: &str) -> Vec<(usize, String)> {
    inline_test_modules(source)
        .expect("source scans")
        .into_iter()
        .map(|module| (module.line, module.path))
        .collect()
}

#[test]
fn an_inline_cfg_test_tests_module_is_reported_with_its_line() {
    let source = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n";
    assert_eq!(found(source), vec![(4, "tests".to_string())]);
}

#[test]
fn a_sibling_file_declaration_is_clean() {
    assert_eq!(found("fn a() {}\n\n#[cfg(test)]\nmod tests;\n"), vec![]);
}

#[test]
fn an_inline_module_named_tests_is_reported_without_a_cfg() {
    assert_eq!(found("mod tests {}\n"), vec![(1, "tests".to_string())]);
}

#[test]
fn an_inline_module_gated_on_test_is_reported_whatever_its_name() {
    let source = "#[cfg(test)]\nmod escrow_tests {}\n\
                  #[cfg(all(test, feature = \"x\"))]\nmod gated {}\n";
    assert_eq!(
        found(source),
        vec![(2, "escrow_tests".to_string()), (4, "gated".to_string())]
    );
}

#[test]
fn a_cfg_that_also_holds_outside_tests_is_not_a_test_module() {
    let source = "#[cfg(any(test, feature = \"test-support\"))]\nmod support {}\n\
                  #[cfg(not(test))]\nmod live {}\n#[cfg(feature = \"test\")]\nmod named {}\n";
    assert_eq!(found(source), vec![]);
}

#[test]
fn visibility_doc_comments_and_other_attributes_do_not_hide_the_cfg() {
    let source = "/// Helpers.\n#[cfg(test)]\n// note\n#[allow(dead_code)]\npub(crate) mod helpers {}\n\
                  #[cfg(test)]\npub(in crate::a) mod scoped {}\n#[cfg(test)]\npub mod open {}\n";
    assert_eq!(
        found(source),
        vec![
            (5, "helpers".to_string()),
            (7, "scoped".to_string()),
            (9, "open".to_string()),
        ]
    );
}

#[test]
fn a_nested_test_module_is_reported_with_its_inline_path() {
    let source = "mod wire {\n    fn f() {}\n    #[cfg(test)]\n    mod tests {\n        mod deeper {}\n    }\n}\nmod after {}\n";
    assert_eq!(found(source), vec![(4, "wire::tests".to_string())]);
}

#[test]
fn a_module_after_a_closed_inline_module_is_not_nested_in_it() {
    let source = "mod wire { fn f() { if true {} } }\nmod tests {}\n";
    assert_eq!(found(source), vec![(2, "tests".to_string())]);
}

#[test]
fn comments_and_literals_are_not_modules() {
    let source = "// mod tests {\n/* mod tests { /* nested */ } */\n\
                  const A: &str = \"mod tests {\";\nconst B: &str = r#\"mod tests { \"# ;\n\
                  const C: &[u8] = br\"mod tests {\";\nconst D: &[u8] = b\"mod tests {\";\n";
    assert_eq!(found(source), vec![]);
}

#[test]
fn char_literals_and_lifetimes_do_not_unbalance_braces() {
    let source = "fn f<'a>(x: &'a str) -> [char; 4] { let _ = x; ['{', '\\'', '\\u{7b}', b'{' as char] }\n\
                  fn g() -> &'static str { 'outer: loop { break 'outer \"\"; } }\n\
                  mod tests {}\n";
    assert_eq!(found(source), vec![(3, "tests".to_string())]);
}

#[test]
fn a_non_ascii_char_literal_is_one_char() {
    assert_eq!(
        found("const E: char = 'é';\nmod tests {}\n"),
        vec![(2, "tests".to_string())]
    );
}

#[test]
fn a_macro_fragment_module_is_not_reported() {
    let source = "macro_rules! m { ($n:ident) => { #[cfg(test)] mod $n {} }; }\n";
    assert_eq!(found(source), vec![]);
}

#[test]
fn a_raw_identifier_is_one_identifier() {
    assert_eq!(
        found("fn r#mod() {}\nmod tests {}\n"),
        vec![(2, "tests".to_string())]
    );
}

#[test]
fn unterminated_or_unbalanced_source_is_an_error_not_a_pass() {
    assert!(inline_test_modules("const A: &str = \"open;\n").is_err());
    assert!(inline_test_modules("/* open\n").is_err());
    assert!(inline_test_modules("const A: &str = r#\"open\";\n").is_err());
    assert!(inline_test_modules("mod tests {\n").is_err());
    assert!(inline_test_modules("}\n").is_err());
}

#[test]
fn a_tree_is_checked_file_by_file_and_reports_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("crate/src");
    fs::create_dir_all(src.join("foo")).unwrap();
    fs::write(src.join("lib.rs"), "mod foo;\n#[cfg(test)]\nmod tests;\n").unwrap();
    fs::write(src.join("tests.rs"), "use super::*;\n").unwrap();
    fs::write(src.join("foo.rs"), "#[cfg(test)]\nmod tests {\n}\n").unwrap();
    fs::write(src.join("foo/notes.txt"), "mod tests {}\n").unwrap();

    let violations =
        check_source_dirs(root.path(), &[src, root.path().join("missing/src")]).unwrap();
    assert_eq!(
        violations,
        vec!["crate/src/foo.rs:2: inline test module `tests`".to_string()]
    );
}

#[test]
fn a_tree_with_no_rust_source_is_a_violation() {
    let root = tempfile::tempdir().unwrap();
    let violations = check_source_dirs(root.path(), &[root.path().join("src")]).unwrap();
    assert_eq!(violations.len(), 1);
    assert!(violations[0].contains("checked nothing"));
}
