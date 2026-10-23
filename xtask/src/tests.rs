use super::*;

#[test]
fn cargo_workspace_version_updates_and_preserves_rest() {
    let input = "[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n";
    let out = set_cargo_workspace_version(input, "0.2.0").unwrap();
    assert!(out.contains("version = \"0.2.0\""));
    assert!(
        out.contains("edition = \"2024\""),
        "unrelated keys preserved"
    );
}

#[test]
fn cargo_missing_version_errors() {
    assert!(set_cargo_workspace_version("[workspace]\nmembers = []\n", "0.2.0").is_err());
}

#[test]
fn package_json_version_updates_field_and_keeps_formatting() {
    let input =
        "{\n  \"name\": \"capsule-web\",\n  \"version\": \"0.1.0\",\n  \"private\": true\n}\n";
    let out = set_package_json_version(input, "0.2.0").unwrap();
    assert!(out.contains("\"version\": \"0.2.0\""));
    assert!(out.contains("\"name\": \"capsule-web\""));
    assert!(out.contains("\"private\": true"));
}

#[test]
fn gradle_bumps_name_and_increments_code() {
    let input = "capsule.versionName=0.1.0\ncapsule.versionCode=7\n";
    let out = set_gradle_properties_version(input, "0.2.0").unwrap();
    assert!(out.contains("capsule.versionName=0.2.0"));
    assert!(
        out.contains("capsule.versionCode=8"),
        "versionCode is monotonic"
    );
}

#[test]
fn marketing_version_updates() {
    let input = "        \"MARKETING_VERSION\": \"0.1.0\",\n";
    let out = set_marketing_version(input, "3.4.5").unwrap();
    assert_eq!(out, "        \"MARKETING_VERSION\": \"3.4.5\",\n");
}

#[test]
fn missing_targets_error() {
    assert!(set_gradle_properties_version("nope=1\n", "0.2.0").is_err());
    assert!(set_marketing_version("no setting here\n", "0.2.0").is_err());
    assert!(set_package_json_version("{}\n", "0.2.0").is_err());
}
