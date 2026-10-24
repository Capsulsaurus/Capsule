use super::*;

/// A Swift source in the layout uniffi 0.31 emits unformatted: the object's methods at column 0
/// inside the class, its requirement list in `…Protocol`, braces inside doc comments and strings.
const SWIFT: &str = r#"
/**
 * Entry point. Hangs under `{upload_base_url}/{id}`.
 */
public protocol FfiCapsuleClientProtocol: AnyObject, Sendable {

    /**
     * Authenticate.
     */
    func login(email: String, password: String) async throws  -> FfiLoginOutcome

}
/**
 * Entry point. Hangs under `{upload_base_url}/{id}`.
 */
open class FfiCapsuleClient: FfiCapsuleClientProtocol, @unchecked Sendable {
    fileprivate let handle: UInt64
    required public init(unsafeFromHandle handle: UInt64) {
        self.handle = handle
    }
    public init(noHandle: NoHandle) {
        self.handle = 0
    }
    /**
     * Build a client.
     */
public convenience init(authBaseUrl: String)throws  {
    let message = "a { in a string"
    self.init(unsafeFromHandle: 0)
}
open func login(email: String, password: String)async throws  -> FfiLoginOutcome  {
    return try await call()
}
}
public struct FfiConverterTypeFfiCapsuleClient: FfiConverter {
    public static func lift(_ handle: UInt64) throws -> FfiCapsuleClient { fatalError() }
}
/**
 * One album.
 */
public struct FfiAlbum: Equatable, Hashable {
    /**
     * Album id.
     */
    public var albumId: String
    /**
     * Keyword-named.
     */
    public var `class`: String
}

// Note that we don't yet support `indirect` for enums.
/**
 * Device tier.
 */

public enum FfiDeviceTier: Equatable, Hashable {
    /**
     * Low.
     */
    case lowRam
    case normal
}
/**
 * Evaluate.
 */
public func evaluateAlerts(input: FfiNotifyInput, now: String)throws  -> [FfiAlert]  {
    return []
}
public func uniffiEnsureCapsuleSdkInitialized() {
}
"#;

/// The Kotlin counterpart, in uniffi's unformatted Kotlin layout.
const KOTLIN: &str = r#"
public interface FfiCapsuleClientInterface {
    /**
     * Authenticate.
     */
    suspend fun `login`(`email`: kotlin.String, `password`: kotlin.String): FfiLoginOutcome
    companion object
}
/**
 * Entry point.
 */
open class FfiCapsuleClient: Disposable, AutoCloseable, FfiCapsuleClientInterface
{
    constructor(withHandle: UniffiWithHandle, handle: Long) {
        this.handle = handle
    }
    /**
     * Build a client.
     */
    constructor(`authBaseUrl`: kotlin.String) :
        this(UniffiWithHandle, 0L)
    override suspend fun `login`(`email`: kotlin.String, `password`: kotlin.String): FfiLoginOutcome {
        val brace = '{'
        return call("${email}")
    }
    companion object
}
/**
 * One album.
 */
data class FfiAlbum (
    /**
     * Album id.
     */
    var `albumId`: kotlin.String
    ,
    var `class`: kotlin.String
){
    companion object
}
enum class FfiDeviceTier {
    /**
     * Low.
     */
    LOW_RAM,
    NORMAL;
    companion object
}
sealed class FfiException: kotlin.Exception() {
    class Auth(
        val `message`: kotlin.String
        ) : FfiException() {
    }
}
/**
 * Evaluate.
 */
    @Throws(FfiException::class) fun `evaluateAlerts`(`input`: FfiNotifyInput, `now`: kotlin.String): List<FfiAlert> {
        return listOf()
    }
"#;

fn names(declarations: &[Declaration]) -> Vec<(&str, &str)> {
    declarations
        .iter()
        .map(|d| (d.kind.as_str(), d.name.as_str()))
        .collect()
}

fn find_named<'a>(declarations: &'a [Declaration], name: &str) -> &'a Declaration {
    declarations.iter().find(|d| d.name == name).unwrap()
}

#[test]
fn lex_ignores_braces_in_comments_strings_and_char_literals() {
    let lines = lex("a {\n/* { */ \"}\" '{'\n// }\n}\nb\n").unwrap();
    let depths: Vec<usize> = lines.iter().map(|line| line.depth).collect();
    assert_eq!(depths, [0, 1, 1, 1, 0]);
}

#[test]
fn lex_marks_lines_that_start_inside_a_block_comment() {
    let lines = lex("/**\n * {\n */\nx\n").unwrap();
    let in_comment: Vec<bool> = lines.iter().map(|line| line.in_comment).collect();
    assert_eq!(in_comment, [false, true, true, false]);
}

#[test]
fn lex_refuses_unbalanced_braces() {
    assert!(lex("}\n").unwrap_err().to_string().contains("never opened"));
    assert!(
        lex("{\n")
            .unwrap_err()
            .to_string()
            .contains("do not balance")
    );
}

#[test]
fn doc_above_reads_a_block_across_blank_lines_and_directives() {
    let lines = lex("/**\n * One.\n *\n * Two.\n */\n\n#if x\nfunc f()\n").unwrap();
    assert_eq!(doc_above(&lines, 7), "One.\n\nTwo.");
    let lines = lex("/** Single. */\nfunc f()\n").unwrap();
    assert_eq!(doc_above(&lines, 1), "Single.");
    let lines = lex("let x = 1\nfunc f()\n").unwrap();
    assert_eq!(doc_above(&lines, 1), "");
}

#[test]
fn tidy_signature_spaces_and_trims() {
    assert_eq!(
        tidy_signature("open func login(email: String)async throws  -> FfiLoginOutcome  {"),
        "open func login(email: String) async throws -> FfiLoginOutcome"
    );
    assert_eq!(
        tidy_signature("data class FfiAlbum ("),
        "data class FfiAlbum"
    );
    assert_eq!(
        tidy_signature("    constructor(`a`: kotlin.String) :"),
        "constructor(`a`: kotlin.String)"
    );
}

#[test]
fn scans_the_swift_surface() {
    let declarations = scan_swift(SWIFT).unwrap();
    assert_eq!(
        names(&declarations),
        [
            ("class", "FfiCapsuleClient"),
            ("struct", "FfiAlbum"),
            ("enum", "FfiDeviceTier"),
            ("func", "evaluateAlerts"),
        ],
        "the protocol folds into its class; converters and uniffi helpers are not the surface"
    );
    let client = find_named(&declarations, "FfiCapsuleClient");
    assert_eq!(
        client.doc,
        "Entry point. Hangs under `{upload_base_url}/{id}`."
    );
    assert_eq!(
        names(&client.members),
        [("init", "init"), ("func", "login")]
    );
    assert_eq!(client.members[0].doc, "Build a client.");
    assert_eq!(
        client.members[1].signature,
        "func login(email: String, password: String) async throws -> FfiLoginOutcome",
        "the method is read from the protocol requirement, not the implementation"
    );
    let album = find_named(&declarations, "FfiAlbum");
    assert_eq!(
        names(&album.members),
        [("field", "albumId"), ("field", "class")]
    );
    assert_eq!(album.members[1].signature, "public var `class`: String");
    let tier = find_named(&declarations, "FfiDeviceTier");
    assert_eq!(tier.doc, "Device tier.");
    assert_eq!(
        names(&tier.members),
        [("case", "lowRam"), ("case", "normal")]
    );
}

#[test]
fn scans_the_kotlin_surface() {
    let declarations = scan_kotlin(KOTLIN).unwrap();
    assert_eq!(
        names(&declarations),
        [
            ("class", "FfiCapsuleClient"),
            ("data class", "FfiAlbum"),
            ("enum class", "FfiDeviceTier"),
            ("sealed class", "FfiException"),
            ("fun", "evaluateAlerts"),
        ]
    );
    let client = find_named(&declarations, "FfiCapsuleClient");
    assert_eq!(
        names(&client.members),
        [("constructor", "constructor"), ("fun", "login")]
    );
    assert_eq!(
        client.members[1].signature,
        "suspend fun login(email: kotlin.String, password: kotlin.String): FfiLoginOutcome",
        "backtick quoting is removed"
    );
    let album = find_named(&declarations, "FfiAlbum");
    assert_eq!(
        names(&album.members),
        [("field", "albumId"), ("field", "class")]
    );
    assert_eq!(album.members[0].doc, "Album id.");
    let tier = find_named(&declarations, "FfiDeviceTier");
    assert_eq!(
        names(&tier.members),
        [("entry", "LOW_RAM"), ("entry", "NORMAL")]
    );
    let error = find_named(&declarations, "FfiException");
    assert_eq!(names(&error.members), [("variant", "Auth")]);
    let evaluate = find_named(&declarations, "evaluateAlerts");
    assert_eq!(evaluate.doc, "Evaluate.");
    assert!(
        evaluate
            .signature
            .starts_with("@Throws(FfiException::class) fun evaluateAlerts(")
    );
}

#[test]
fn the_fixtures_agree_across_languages_apart_from_the_required_table() {
    let swift = scan_swift(SWIFT).unwrap();
    let kotlin = scan_kotlin(KOTLIN).unwrap();
    let err = assert_surface(&swift, &kotlin).unwrap_err().to_string();
    // The fixtures are small, so the first required name is missing: that is the failure, not
    // a disagreement between the two languages.
    assert!(err.contains("binding surface missing"), "{err}");
}

/// A surface carrying every required name, as both languages would declare it.
fn complete(kotlin: bool) -> Vec<Declaration> {
    REQUIRED
        .iter()
        .map(|(swift, kotlin_name)| Declaration {
            kind: "class".into(),
            name: if kotlin {
                (*kotlin_name).into()
            } else {
                (*swift).into()
            },
            signature: String::new(),
            doc: String::new(),
            members: Vec::new(),
        })
        .collect()
}

#[test]
fn accepts_a_complete_surface_that_agrees() {
    assert_surface(&complete(false), &complete(true)).unwrap();
}

#[test]
fn refuses_a_missing_required_declaration() {
    let mut kotlin = complete(true);
    kotlin.retain(|d| d.name != "evaluateAlerts");
    let err = assert_surface(&complete(false), &kotlin)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`evaluateAlerts` in the Kotlin source"),
        "{err}"
    );
}

#[test]
fn refuses_languages_that_disagree() {
    let mut swift = complete(false);
    swift[0].members.push(Declaration {
        kind: "func".into(),
        name: "onlyInSwift".into(),
        signature: String::new(),
        doc: String::new(),
        members: Vec::new(),
    });
    let err = assert_surface(&swift, &complete(true))
        .unwrap_err()
        .to_string();
    assert!(err.contains("onlyInSwift"), "{err}");
}

#[test]
fn refuses_an_empty_surface() {
    assert!(assert_surface(&[], &complete(true)).is_err());
}

#[test]
fn kotlin_spells_an_error_type_as_an_exception() {
    assert_eq!(kotlin_type_name("FfiError"), "FfiException");
    assert_eq!(
        kotlin_type_name("FfiHardwareSignerError"),
        "FfiHardwareSignerException"
    );
    assert_eq!(kotlin_type_name("FfiAlbum"), "FfiAlbum");
}

#[test]
fn reads_the_locked_uniffi_version() {
    let lock = "[[package]]\nname = \"uniffi\"\nversion = \"0.31.1\"\n";
    assert_eq!(uniffi_version(lock).unwrap(), "0.31.1");
    assert!(uniffi_version("[[package]]\nname = \"uniffi_core\"\nversion = \"1\"\n").is_err());
}

#[test]
fn check_fails_on_drift_and_names_the_task() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("Cargo.lock"),
        "name = \"uniffi\"\nversion = \"0.31.1\"\n",
    )
    .unwrap();
    let err = run(root.path(), true).unwrap_err();
    assert!(
        format!("{err:#}").contains("mise run gen-bindings"),
        "{err:#}"
    );
}
