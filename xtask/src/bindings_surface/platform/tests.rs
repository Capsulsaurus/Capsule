use super::*;

/// A hand-written Kotlin adapter as ktlint formats it: a multi-line primary constructor, an
/// expression-bodied override, a multi-line parameter list, private helpers, a private companion.
const KOTLIN: &str = r#"package com.justin13888.capsule.hardware

import uniffi.capsule_sdk.FfiHardwareSigner

/**
 * Software [FfiHardwareSigner]. See [assertNonExportable].
 */
class SoftwareSigner(
    private val seed: ByteArray,
) : FfiHardwareSigner {
    init {
        require(seed.size == 32) { "seed must be 32 bytes, not ${seed.size}" }
    }

    private fun privateKey(keyAlias: String): ByteArray = seed

    /** The public key. */
    override fun enroll(keyAlias: String): ByteArray = classicalPublicKey(keyAlias)

    override fun signClassical(
        keyAlias: String,
        msg: ByteArray,
    ): ByteArray =
        run {
            msg
        }

    @Throws(Exception::class)
    override fun assertNonExportable(keyAlias: String) {
        throw IllegalStateException("{")
    }

    private companion object {
        val INFO = "x".toByteArray()
    }
}
"#;

/// A hand-written Swift adapter: `///` docs, a public init, public and private members.
const SWIFT: &str = r#"import CryptoKit
import Foundation

/// A software ``FfiHardwareSigner``.
///
/// See ``assertNonExportable(keyAlias:)``.
public final class SoftwareSigner: FfiHardwareSigner, @unchecked Sendable {
    private static let info = Data("{".utf8)
    private let seed: Data

    /// Build a signer.
    public init(seed: Data) {
        self.seed = seed
    }

    private func key(_ keyAlias: String) throws -> Data {
        seed
    }

    /// Enroll.
    public func enroll(keyAlias: String) throws -> Data {
        try key(keyAlias)
    }

    public func assertNonExportable(keyAlias: String) throws {
        throw FfiHardwareSignerError.Exportable(message: "software")
    }
}
"#;

#[test]
fn reads_a_kotlin_adapter_with_its_public_members_only() {
    let declarations = scan(KOTLIN, Language::Kotlin).unwrap();
    assert_eq!(declarations.len(), 1);
    let signer = &declarations[0];
    assert_eq!(signer.kind, "class");
    assert_eq!(signer.name, "SoftwareSigner");
    assert_eq!(
        signer.signature,
        "class SoftwareSigner(seed: ByteArray) : FfiHardwareSigner"
    );
    assert_eq!(
        signer.doc,
        "Software [FfiHardwareSigner]. See [assertNonExportable]."
    );
    let members: Vec<_> = signer
        .members
        .iter()
        .map(|m| (m.kind.as_str(), m.name.as_str(), m.signature.as_str()))
        .collect();
    assert_eq!(
        members,
        [
            (
                "constructor",
                "SoftwareSigner",
                "constructor(seed: ByteArray)"
            ),
            (
                "fun",
                "enroll",
                "override fun enroll(keyAlias: String): ByteArray"
            ),
            (
                "fun",
                "signClassical",
                "override fun signClassical(keyAlias: String, msg: ByteArray): ByteArray"
            ),
            (
                "fun",
                "assertNonExportable",
                "override fun assertNonExportable(keyAlias: String)"
            ),
        ]
    );
    assert_eq!(signer.members[1].doc, "The public key.");
}

#[test]
fn reads_a_swift_adapter_with_its_line_docs() {
    let declarations = scan(SWIFT, Language::Swift).unwrap();
    assert_eq!(declarations.len(), 1);
    let signer = &declarations[0];
    assert_eq!(signer.kind, "class");
    assert_eq!(
        signer.signature,
        "public final class SoftwareSigner: FfiHardwareSigner, @unchecked Sendable"
    );
    assert_eq!(
        signer.doc,
        "A software ``FfiHardwareSigner``.\n\nSee ``assertNonExportable(keyAlias:)``."
    );
    let members: Vec<_> = signer
        .members
        .iter()
        .map(|m| (m.kind.as_str(), m.name.as_str(), m.signature.as_str()))
        .collect();
    assert_eq!(
        members,
        [
            ("init", "init", "public init(seed: Data)"),
            (
                "func",
                "enroll",
                "public func enroll(keyAlias: String) throws -> Data"
            ),
            (
                "func",
                "assertNonExportable",
                "public func assertNonExportable(keyAlias: String) throws"
            ),
        ]
    );
    assert_eq!(signer.members[0].doc, "Build a signer.");
    assert_eq!(signer.members[2].doc, "");
}

#[test]
fn skips_a_type_its_language_keeps_private() {
    let kotlin = "internal class Hidden {\n    fun x() {}\n}\nprivate class Gone\n";
    assert!(scan(kotlin, Language::Kotlin).unwrap().is_empty());
    let swift = "final class Internal {\n    public func x() {}\n}\n";
    assert!(scan(swift, Language::Swift).unwrap().is_empty());
}

#[test]
fn refuses_a_public_top_level_declaration_it_does_not_read() {
    refused("fun helper() = 1\n", Language::Kotlin, "`fun` `helper`");
    refused(
        "public func helper() {}\n",
        Language::Swift,
        "`func` `helper`",
    );
    refused(
        "private val x = 1\nwhat is this\n",
        Language::Kotlin,
        "what is this",
    );
}

#[test]
fn reads_the_committed_adapters() {
    // The real sources: a scan that stops matching them fails here, not in CI's drift check.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let kotlin = scan_dir(root, KOTLIN_DIR, Language::Kotlin).unwrap();
    let swift = scan_dir(root, SWIFT_DIR, Language::Swift).unwrap();
    let names = |declarations: &[Declaration]| -> Vec<String> {
        declarations.iter().map(|d| d.name.clone()).collect()
    };
    assert!(names(&kotlin).contains(&"StrongBoxSigner".to_string()));
    assert!(names(&kotlin).contains(&"SoftwareSigner".to_string()));
    assert!(names(&swift).contains(&"SecureEnclaveSigner".to_string()));
    for declaration in kotlin.iter().chain(&swift) {
        assert!(
            !declaration.members.is_empty(),
            "{} has no members",
            declaration.name
        );
        assert!(
            !declaration.doc.is_empty(),
            "{} has no doc",
            declaration.name
        );
    }
}

/// Scan `source`, expecting it to be refused with a message containing `needle`.
fn refused(source: &str, language: Language, needle: &str) {
    let error = format!("{:#}", scan(source, language).unwrap_err());
    assert!(error.contains(needle), "expected `{needle}` in: {error}");
}

/// `(kind, name, signature)` of each member of the one declaration in `source`.
fn members_of(source: &str, language: Language) -> Vec<(String, String, String)> {
    let declarations = scan(source, language).unwrap();
    assert_eq!(declarations.len(), 1, "{declarations:?}");
    declarations[0]
        .members
        .iter()
        .map(|m| (m.kind.clone(), m.name.clone(), m.signature.clone()))
        .collect()
}

#[test]
fn reads_a_declaration_whose_attribute_shares_its_line() {
    let swift = "@MainActor public final class A {\n    @discardableResult public func x() -> Int { 1 }\n}\n";
    let members = members_of(swift, Language::Swift);
    assert_eq!(members[0].2, "public func x() -> Int");
    let kotlin = "class A {\n    @Throws(E::class) fun x() {}\n}\n";
    let members = members_of(kotlin, Language::Kotlin);
    assert_eq!(members[0].2, "fun x()");
}

#[test]
fn a_parenthesis_in_a_literal_or_comment_swallows_nothing() {
    let kotlin =
        "class A {\n    val open = '('\n    fun y(): Int = 1 // see (note\n    fun z() {}\n}\n";
    let names: Vec<String> = members_of(kotlin, Language::Kotlin)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    assert_eq!(names, ["open", "y", "z"]);
}

#[test]
fn refuses_a_header_whose_parentheses_never_balance() {
    // A `(` inside a template's nested string, which the counter does not lex.
    let kotlin = "class A {\n    val s = \"${m[\"(\"]}\"\n    fun z() {}\n}\n";
    refused(kotlin, Language::Kotlin, "never balance");
}

#[test]
fn reads_swift_visibility_past_a_modifier_it_does_not_list() {
    let swift = "public final class A {\n    nonisolated public func x() {}\n    nonisolated func hidden() {}\n}\n";
    let members = members_of(swift, Language::Swift);
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].1, "x");
}

#[test]
fn refuses_public_members_added_by_an_extension() {
    refused(
        "extension A {\n    public func extra() {}\n}\n",
        Language::Swift,
        "public func extra()",
    );
    // An extension adding nothing public is not surface.
    assert!(
        scan("extension A {\n    func helper() {}\n}\n", Language::Swift)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn refuses_public_type_kinds_it_does_not_read() {
    refused(
        "public enum E {\n    case a\n}\n",
        Language::Swift,
        "`enum` `E`",
    );
    refused(
        "public protocol P {\n    func f()\n}\n",
        Language::Swift,
        "`protocol` `P`",
    );
    refused("enum class E { A }\n", Language::Kotlin, "`class` `E`");
    // A subclass reaches an open or abstract class's `protected` members, which the scan drops.
    refused(
        "open class O {\n    protected fun hook() {}\n}\n",
        Language::Kotlin,
        "`class` `O`",
    );
    refused(
        "abstract class A {\n    protected abstract fun hook()\n}\n",
        Language::Kotlin,
        "`class` `A`",
    );
    refused(
        "object O {\n    fun f() {}\n}\n",
        Language::Kotlin,
        "`object` `O`",
    );
    refused(
        "class A {\n    companion object {\n        fun make() = A()\n    }\n}\n",
        Language::Kotlin,
        "`object`",
    );
    refused(
        "public class A {\n    public subscript(i: Int) -> Int { i }\n}\n",
        Language::Swift,
        "`subscript`",
    );
}

#[test]
fn lists_a_public_constructor_property_and_hides_a_private_one() {
    let kotlin =
        "class A(\n    val strong: Boolean = true,\n    private val seed: ByteArray,\n) : B {\n}\n";
    let declarations = scan(kotlin, Language::Kotlin).unwrap();
    assert_eq!(
        declarations[0].signature,
        "class A(strong: Boolean = true, seed: ByteArray) : B"
    );
    let members: Vec<_> = declarations[0]
        .members
        .iter()
        .map(|m| (m.kind.as_str(), m.signature.as_str()))
        .collect();
    assert_eq!(
        members,
        [
            (
                "constructor",
                "constructor(strong: Boolean = true, seed: ByteArray)"
            ),
            ("field", "val strong: Boolean"),
        ]
    );
}

#[test]
fn keeps_a_function_typed_parameter_and_its_default_whole() {
    let kotlin =
        "class A(private val cb: (Int) -> Unit) : B {\n    fun run(block: () -> Unit = {}) {}\n}\n";
    let declarations = scan(kotlin, Language::Kotlin).unwrap();
    assert_eq!(declarations[0].signature, "class A(cb: (Int) -> Unit) : B");
    assert_eq!(
        declarations[0].members[1].signature,
        "fun run(block: () -> Unit = {})"
    );
    let swift = "public class A {\n    public init(cb: @escaping (Int) -> Void = { _ in }) {}\n}\n";
    assert_eq!(
        members_of(swift, Language::Swift)[0].2,
        "public init(cb: @escaping (Int) -> Void = { _ in })"
    );
}

#[test]
fn reads_a_generic_class_constructor() {
    let declarations = scan(
        "class A<T : Any>(private val t: T) : B<T> {\n}\n",
        Language::Kotlin,
    )
    .unwrap();
    assert_eq!(declarations[0].signature, "class A<T : Any>(t: T) : B<T>");
    assert_eq!(declarations[0].members[0].signature, "constructor(t: T)");
}

#[test]
fn reads_a_wrapped_supertype_list() {
    let kotlin = "class A(\n    seed: ByteArray,\n) : B,\n    C {\n    fun x() {}\n}\n";
    let declarations = scan(kotlin, Language::Kotlin).unwrap();
    assert_eq!(declarations[0].signature, "class A(seed: ByteArray) : B, C");
    assert_eq!(declarations[0].members[1].name, "x");
}

#[test]
fn refuses_a_primary_constructor_with_modifiers() {
    refused(
        "class A private constructor(x: Int) {\n}\n",
        Language::Kotlin,
        "primary constructor with modifiers",
    );
    refused(
        "class A @Inject constructor(private val s: Int) : B {\n}\n",
        Language::Kotlin,
        "primary constructor with modifiers",
    );
}

#[test]
fn refuses_members_a_public_extension_makes_public() {
    refused(
        "public extension A {\n    func extra() {}\n}\n",
        Language::Swift,
        "func extra()",
    );
    assert!(
        scan(
            "public extension A {\n    private func helper() {}\n}\n",
            Language::Swift
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn names_a_generic_or_extension_function_by_its_own_name() {
    let kotlin = "class A {\n    fun <T : Any> wrap(x: T) {}\n    fun String.shout() {}\n}\n";
    let names: Vec<String> = members_of(kotlin, Language::Kotlin)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    assert_eq!(names, ["wrap", "shout"]);
}

#[test]
fn finds_a_lowercase_class_name_after_its_keyword() {
    let declarations = scan("class s(private val t: Int) {\n}\n", Language::Kotlin).unwrap();
    assert_eq!(declarations[0].signature, "class s(t: Int)");
}

#[test]
fn reads_a_swift_line_doc_above_an_attribute() {
    let swift = "public final class A {\n    /// Does x.\n    @discardableResult\n    public func x() -> Int { 1 }\n}\n";
    let declarations = scan(swift, Language::Swift).unwrap();
    assert_eq!(declarations[0].members[0].doc, "Does x.");
}
