//! Generates Rust types from the vendored Argo protobuf subset.
//!
//! We add serde derives so the same generated structs round-trip to the
//! camelCase YAML/JSON shape Argo expects. `skip_serializing_if` keeps the
//! emitted YAML free of empty strings / null / [] noise (Argo rejects some
//! of those), via the generic `crate::ser::skip` helper.

use std::io::Result;

fn main() -> Result<()> {
    let proto = "proto/cargo_athena/api/v1/argo.proto";
    println!("cargo:rerun-if-changed={proto}");
    println!("cargo:rerun-if-changed=proto");

    let mut config = prost_build::Config::new();
    config
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .type_attribute(".", "#[serde(rename_all = \"camelCase\")]")
        .field_attribute(
            ".",
            "#[serde(default, skip_serializing_if = \"crate::ser::skip\")]",
        );

    config.compile_protos(&[proto], &["proto"])?;
    Ok(())
}
