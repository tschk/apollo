//! Workspace plugin manifest (Hermes/OpenClaw-style packages).

mod manifest;

pub use manifest::{
    apply_workspace_manifest, load_manifest, merge_manifest_into_config, PluginManifestFile,
};
