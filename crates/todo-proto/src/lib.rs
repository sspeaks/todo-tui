//! Generated todo.v1 protobuf code and core conversions.

pub mod convert;
pub mod request;

/// `google.*` types compiled from the vendored protos.
#[allow(clippy::all)]
pub mod google {
    pub mod r#type {
        include!(concat!(env!("OUT_DIR"), "/google.r#type.rs"));
    }
}

#[allow(clippy::all)]
pub mod todo {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/todo.v1.rs"));
    }
}

pub use todo::v1;

/// Encoded `FileDescriptorSet` of todo.v1 and all its imports, for
/// prost-reflect (proto3 JSON) and gRPC reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/todo_descriptor.bin"));

/// The `api_minor` this build serves (design 4.2).
pub const API_MINOR: u32 = 2;
