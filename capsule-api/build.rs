fn main() {
    let debug_build = std::env::var("PROFILE")
        .map(|profile| profile == "debug")
        .unwrap_or(false);

    // Check if 'openapi' feature is enabled and validate against build profile
    if std::env::var("CARGO_FEATURE_OPENAPI").is_ok() {
        if debug_build {
            println!("cargo:warning=OpenAPI feature is enabled for debug build");
        } else {
            panic!(
                "Error: The `openapi` feature cannot be enabled in release for security reasons"
            );
        }
    }

    // Feature flags
    let features = vec!["auth", "graphql", "upload", "metadata"];
    let mut has_server_feature = false;

    for feature in features.iter() {
        // println!("cargo:warning=Checking feature: {}", &feature);
        if std::env::var(format!("CARGO_FEATURE_{}", &feature.to_uppercase())).is_ok() {
            has_server_feature = true;
        }
    }

    if !has_server_feature {
        panic!("No server feature enabled. At least one of {features:?} should be enabled.");
    }
}
