//! JSON manifest under `workspace/.apollo/plugins/manifest.json`.

use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

use crate::config::Config;
use crate::tools::toolsets::apply_package_manifest;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginManifestFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub packages: Vec<String>,
    #[serde(default)]
    pub toolsets: ToolsetPatch,
    #[serde(default)]
    pub system_prompt_suffix: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolsetPatch {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub disabled: Vec<String>,
}

pub fn load_manifest(workspace: &Path, cfg: &Config) -> anyhow::Result<Option<PluginManifestFile>> {
    if !cfg.plugin_layer.enabled {
        return Ok(None);
    }
    let path = workspace
        .join(".apollo")
        .join(&cfg.plugin_layer.manifest_path);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read plugin manifest {}", path.display()))?;
    let m: PluginManifestFile = serde_json::from_str(&raw)
        .with_context(|| format!("parse plugin manifest {}", path.display()))?;
    Ok(Some(m))
}

pub fn merge_manifest_into_config(cfg: &mut Config, manifest: &PluginManifestFile) {
    apply_package_manifest(&mut cfg.toolsets, &manifest.packages);
    for e in &manifest.toolsets.enabled {
        if !cfg.toolsets.enabled.contains(e) {
            cfg.toolsets.enabled.push(e.clone());
        }
    }
    for d in &manifest.toolsets.disabled {
        if !cfg.toolsets.disabled.contains(d) {
            cfg.toolsets.disabled.push(d.clone());
        }
    }
    if !manifest.system_prompt_suffix.trim().is_empty() {
        cfg.system_prompt.push('\n');
        cfg.system_prompt
            .push_str(manifest.system_prompt_suffix.trim());
    }
}

pub fn apply_workspace_manifest(cfg: &mut Config, workspace: &Path) {
    match load_manifest(workspace, cfg) {
        Ok(Some(m)) => merge_manifest_into_config(cfg, &m),
        Ok(None) => {}
        Err(e) => tracing::warn!("plugin manifest skipped: {:#}", e),
    }
}
