use super::*;

fn entry(message: &str, context: Option<&str>, source: Option<&str>) -> Entry {
    Entry {
        message: message.to_string(),
        context: context.map(str::to_string),
        source: source.map(str::to_string),
    }
}

fn english(entries: &[(&str, &str)]) -> BTreeMap<String, Entry> {
    entries
        .iter()
        .map(|(key, message)| ((*key).to_string(), entry(message, Some("A button."), None)))
        .collect()
}

fn current(message: &str, translation: &str) -> Entry {
    entry(translation, None, Some(&source_hash(message)))
}

fn catalogs(
    en: BTreeMap<String, Entry>,
    de: BTreeMap<String, Entry>,
) -> BTreeMap<String, BTreeMap<String, Entry>> {
    BTreeMap::from([("en".to_string(), en), ("de".to_string(), de)])
}

fn glossary(terms: &[&str]) -> Glossary {
    Glossary {
        verbatim: terms.iter().map(|t| (*t).to_string()).collect(),
    }
}

fn problems_of(
    catalogs: &BTreeMap<String, BTreeMap<String, Entry>>,
    glossary: &Glossary,
) -> Vec<String> {
    problems("en", catalogs, glossary)
}

#[test]
fn the_source_hash_is_sixteen_hex_digits_of_sha256() {
    // Pinned (`printf Save | sha256sum`) so a reimplementation cannot drift silently: a
    // changed hash would mark every translation in every catalog stale at once.
    assert_eq!(source_hash("Save"), "1509f561f2416598");
    assert_ne!(source_hash("Save"), source_hash("Save "));
}

#[test]
fn a_current_complete_catalog_has_no_problems() {
    let en = english(&[("app.save", "Save")]);
    let de = BTreeMap::from([("app.save".to_string(), current("Save", "Speichern"))]);
    assert!(problems_of(&catalogs(en, de), &glossary(&[])).is_empty());
}

#[test]
fn a_source_entry_without_context_is_a_problem() {
    let mut en = english(&[("app.save", "Save")]);
    en.get_mut("app.save").unwrap().context = Some("  ".to_string());
    let de = BTreeMap::from([("app.save".to_string(), current("Save", "Speichern"))]);
    let found = problems_of(&catalogs(en, de), &glossary(&[]));
    assert_eq!(found, ["en: key `app.save` has no `context`"]);
}

#[test]
fn a_key_a_translation_lacks_is_a_problem() {
    let en = english(&[("app.save", "Save"), ("app.undo", "Undo")]);
    let de = BTreeMap::from([("app.save".to_string(), current("Save", "Speichern"))]);
    let found = problems_of(&catalogs(en, de), &glossary(&[]));
    assert_eq!(found, ["de: key `app.undo` is missing"]);
}

#[test]
fn a_translation_of_an_edited_source_message_is_stale() {
    let en = english(&[("app.save", "Save all")]);
    let de = BTreeMap::from([("app.save".to_string(), current("Save", "Speichern"))]);
    let found = problems_of(&catalogs(en, de), &glossary(&[]));
    assert_eq!(
        found,
        ["de: key `app.save` is stale: its source message changed"]
    );
}

#[test]
fn a_translation_without_a_source_hash_is_a_problem() {
    let en = english(&[("app.save", "Save")]);
    let de = BTreeMap::from([("app.save".to_string(), entry("Speichern", None, None))]);
    let found = problems_of(&catalogs(en, de), &glossary(&[]));
    assert_eq!(found, ["de: key `app.save` has no `source`"]);
}

#[test]
fn a_dropped_verbatim_term_is_a_problem() {
    let message = "Capsule uses TOTP";
    let en = english(&[("app.mfa", message)]);
    let de = BTreeMap::from([(
        "app.mfa".to_string(),
        current(message, "Die App nutzt TOTP"),
    )]);
    let found = problems_of(&catalogs(en, de), &glossary(&["Capsule", "TOTP"]));
    assert_eq!(found, ["de: key `app.mfa` drops verbatim term(s): Capsule"]);
}

#[test]
fn a_verbatim_term_counts_only_as_a_whole_word_in_the_source() {
    // `capsules` and `HDR10` are not the terms `Capsule` and `HDR`.
    let message = "Two capsules in HDR10";
    let en = english(&[("app.x", message)]);
    let de = BTreeMap::from([("app.x".to_string(), current(message, "Zwei Kapseln"))]);
    assert!(problems_of(&catalogs(en, de), &glossary(&["Capsule", "HDR"])).is_empty());
}

#[test]
fn every_problem_is_reported_at_once_in_key_order() {
    let mut en = english(&[("a.one", "One"), ("b.two", "Two"), ("c.three", "Three")]);
    en.get_mut("c.three").unwrap().context = None;
    let de = BTreeMap::from([
        ("a.one".to_string(), current("Uno", "Eins")),
        ("c.three".to_string(), current("Three", "Drei")),
    ]);
    let found = problems_of(&catalogs(en, de), &glossary(&[]));
    assert_eq!(
        found,
        [
            "en: key `c.three` has no `context`",
            "de: key `a.one` is stale: its source message changed",
            "de: key `b.two` is missing",
        ]
    );
}

#[test]
fn todo_lists_missing_and_stale_keys_with_their_source_and_context() {
    let en = english(&[("a.one", "One"), ("b.two", "Two"), ("c.three", "Three")]);
    let de = BTreeMap::from([
        ("a.one".to_string(), current("One", "Eins")),
        ("b.two".to_string(), current("Zwei?", "Zwei")),
    ]);
    let items = todo(&en, &de);
    assert_eq!(
        items,
        [
            Todo {
                key: "b.two".to_string(),
                reason: TodoReason::Stale,
                message: "Two".to_string(),
                context: "A button.".to_string(),
                current: Some("Zwei".to_string()),
            },
            Todo {
                key: "c.three".to_string(),
                reason: TodoReason::Missing,
                message: "Three".to_string(),
                context: "A button.".to_string(),
                current: None,
            },
        ]
    );
}

#[test]
fn stamp_fills_an_absent_source_and_never_overwrites_one() {
    let en = english(&[("a.one", "One"), ("b.two", "Two")]);
    let mut de: Map<String, Value> = serde_json::from_str(
        r#"{
            "a.one": {"message": "Eins", "origin": "machine"},
            "b.two": {"message": "Zwei", "origin": "human", "source": "0000000000000000"}
        }"#,
    )
    .unwrap();
    assert_eq!(stamp(&en, &mut de), 1);
    assert_eq!(de["a.one"]["source"], source_hash("One"));
    assert_eq!(de["b.two"]["source"], "0000000000000000");
    assert_eq!(stamp(&en, &mut de), 0, "a second stamp is a no-op");
}

#[test]
fn stamp_leaves_a_key_the_source_lacks_alone() {
    let en = english(&[]);
    let mut de: Map<String, Value> =
        serde_json::from_str(r#"{"gone": {"message": "Weg", "origin": "machine"}}"#).unwrap();
    assert_eq!(stamp(&en, &mut de), 0);
    assert!(de["gone"].get("source").is_none());
}

#[test]
fn the_glossary_parses_its_verbatim_list() {
    let parsed = Glossary::parse(
        r#"{"verbatim": ["Capsule", "TOTP"], "terms": {"album": {"de": "Album"}}}"#,
    )
    .unwrap();
    assert_eq!(parsed.verbatim, ["Capsule", "TOTP"]);
}

#[test]
fn the_committed_glossary_parses() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let parsed = Glossary::load(&root).expect("locales/glossary.json parses");
    assert!(parsed.verbatim.iter().any(|t| t == "Capsule"));
}
