use std::path::PathBuf;

const PROTOS: &[&str] = &[
    "todo/v1/common.proto",
    "todo/v1/field.proto",
    "todo/v1/field_value.proto",
    "todo/v1/filter.proto",
    "todo/v1/item.proto",
    "todo/v1/item_service.proto",
    "todo/v1/op.proto",
    "todo/v1/relation.proto",
    "todo/v1/system_service.proto",
    "todo/v1/watch_service.proto",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("../../proto");
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-env-changed=PROTOC");

    let protos: Vec<PathBuf> = PROTOS.iter().map(|p| root.join(p)).collect();
    tonic_prost_build::configure()
        .boxed(".todo.v1.Change.resource.item")
        .file_descriptor_set_path(out.join("todo_descriptor.bin"))
        .compile_protos(&protos, &[root])?;
    Ok(())
}
