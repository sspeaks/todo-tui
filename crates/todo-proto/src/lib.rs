//! Generated todo.v1 protobuf code and core conversions.

pub mod convert;
pub mod export;
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
    pub mod export {
        pub mod v1 {
            include!(concat!(env!("OUT_DIR"), "/todo.export.v1.rs"));
        }
    }
}

pub use todo::v1;

/// Encoded `FileDescriptorSet` of todo.v1 and all its imports, for
/// prost-reflect (proto3 JSON) and gRPC reflection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/todo_descriptor.bin"));

/// The `api_minor` this build serves (design 4.2). 2 added the M2 RPCs as
/// stubs; 3 is the first that implements them. 4 added the M3 RPCs as
/// stubs; 5 is the first that implements them. 6 added the Item's
/// subtask counts and parent title. 7 added recurrence rules and the M5
/// RPCs (SetRecurrence, ParseRecurrence) as stubs; 8 is the first that
/// implements them, reports generated instances and runs the scheduler.
/// 9 added the config: GetConfig, ReloadConfig and Change.config. 10 added
/// query text: ViewService.ParseQuery and View.query. 11 added the view
/// edits: CreateView, UpdateView, DeleteView and MoveView. 12 added
/// SystemService.Backup. 13 added Export and Import. 14 added the
/// Item's energy. 15 added GetActivityReport and the [l48] config.
pub const API_MINOR: u32 = 15;
