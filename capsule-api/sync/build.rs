fn main() {
    tonic_prost_build::configure()
        .compile_protos(
            &["proto/photolibrary/metadata/v1/metadata.proto"],
            &["proto"],
        )
        .unwrap();
}
