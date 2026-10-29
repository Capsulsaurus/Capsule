use super::*;

const EMITTED: &str = "/* tslint:disable */
/* eslint-disable */
/**
 * Whether the link is passphrase-protected.
 */
export function shareIsPassphraseProtected(wrapped_scope_b64: string): boolean;
export class ShareScope {
  private constructor();
  free(): void;
  scopeKind(): string;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly __wbg_sharescope_free: (a: number, b: number) => void;
}
";

#[test]
fn keeps_the_public_declarations_verbatim() {
    let out = digest(EMITTED).unwrap();
    assert!(out.contains(
        "/**\n * Whether the link is passphrase-protected.\n */\nexport function \
         shareIsPassphraseProtected(wrapped_scope_b64: string): boolean;\n"
    ));
    assert!(out.contains("export class ShareScope {\n  private constructor();\n"));
}

#[test]
fn drops_the_loader_abi_and_the_lint_banners() {
    let out = digest(EMITTED).unwrap();
    assert!(!out.contains("InitInput"));
    assert!(!out.contains("__wbg_"));
    assert!(!out.contains("tslint"));
    assert!(!out.contains("eslint"));
}

#[test]
fn opens_with_the_banner_and_ends_with_one_newline() {
    let out = digest(EMITTED).unwrap();
    assert!(out.starts_with(BANNER));
    assert!(out.ends_with("}\n"));
    assert!(!out.ends_with("\n\n"));
}

#[test]
fn is_deterministic() {
    assert_eq!(digest(EMITTED).unwrap(), digest(EMITTED).unwrap());
}

#[test]
fn refuses_a_layout_without_the_loader_boundary() {
    let err = digest("export function f(): void;\n").unwrap_err();
    assert!(err.to_string().contains("export type InitInput"));
}

#[test]
fn refuses_a_surface_that_exports_nothing() {
    let err = digest("/* tslint:disable */\nexport type InitInput = BufferSource;\n").unwrap_err();
    assert!(err.to_string().contains("exports nothing"));
}

#[test]
fn check_fails_on_drift_and_passes_when_current() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("capsule-web/src/generated/wasm")).unwrap();
    fs::create_dir_all(root.path().join("capsule-wasm")).unwrap();
    fs::write(root.path().join(SOURCE), EMITTED).unwrap();

    assert!(run(root.path(), true).is_err(), "no committed digest yet");
    run(root.path(), false).unwrap();
    run(root.path(), true).unwrap();

    fs::write(
        root.path().join(SOURCE),
        EMITTED.replace("scopeKind(): string;", "scopeKind(): number;"),
    )
    .unwrap();
    let err = run(root.path(), true).unwrap_err();
    assert!(err.to_string().contains("mise run wasm-surface"));
}

#[test]
fn names_the_build_step_when_the_declaration_file_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let err = run(root.path(), false).unwrap_err();
    assert!(format!("{err:#}").contains("mise run build-wasm"));
}
