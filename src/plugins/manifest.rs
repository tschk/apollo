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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_load_manifest_disabled_plugin_layer() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.plugin_layer.enabled = false;

        let result = load_manifest(dir.path(), &cfg).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_load_manifest_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();

        let result = load_manifest(dir.path(), &cfg).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_load_manifest_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();

        let apollo_dir = dir.path().join(".apollo").join("plugins");
        fs::create_dir_all(&apollo_dir).unwrap();
        let manifest_path = apollo_dir.join("manifest.json");

        let manifest_content = r#"{
            "version": 1,
            "packages": ["pkg1", "pkg2"],
            "toolsets": {
                "enabled": ["tool1"],
                "disabled": ["tool2"]
            },
            "system_prompt_suffix": "test suffix"
        }"#;
        fs::write(&manifest_path, manifest_content).unwrap();

        let result = load_manifest(dir.path(), &cfg).unwrap().unwrap();
        assert_eq!(result.version, 1);
        assert_eq!(result.packages, vec!["pkg1", "pkg2"]);
        assert_eq!(result.toolsets.enabled, vec!["tool1"]);
        assert_eq!(result.toolsets.disabled, vec!["tool2"]);
        assert_eq!(result.system_prompt_suffix, "test suffix");
    }

    #[test]
    fn test_load_manifest_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::default();

        let apollo_dir = dir.path().join(".apollo").join("plugins");
        fs::create_dir_all(&apollo_dir).unwrap();
        let manifest_path = apollo_dir.join("manifest.json");

        fs::write(&manifest_path, "invalid json").unwrap();

        let result = load_manifest(dir.path(), &cfg);
        assert!(result.is_err());
    }

    #[test]
    fn test_merge_manifest_into_config() {
        let mut cfg = Config::default();
        cfg.toolsets.enabled = vec!["existing_enabled".to_string()];
        cfg.toolsets.disabled = vec!["existing_disabled".to_string()];
        cfg.system_prompt = "base prompt".to_string();

        let manifest = PluginManifestFile {
            version: 1,
            packages: vec![],
            toolsets: ToolsetPatch {
                enabled: vec!["existing_enabled".to_string(), "new_enabled".to_string()],
                disabled: vec!["existing_disabled".to_string(), "new_disabled".to_string()],
            },
            system_prompt_suffix: "added suffix".to_string(),
        };

        merge_manifest_into_config(&mut cfg, &manifest);

        assert_eq!(
            cfg.toolsets.enabled,
            vec!["existing_enabled", "new_enabled"]
        );
        assert_eq!(
            cfg.toolsets.disabled,
            vec!["existing_disabled", "new_disabled"]
        );
        assert_eq!(cfg.system_prompt, "base prompt\nadded suffix");
    }

    #[test]
    fn test_apply_workspace_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();

        let apollo_dir = dir.path().join(".apollo").join("plugins");
        fs::create_dir_all(&apollo_dir).unwrap();
        let manifest_path = apollo_dir.join("manifest.json");

        let manifest_content = r#"{
            "system_prompt_suffix": "workspace suffix"
        }"#;
        fs::write(&manifest_path, manifest_content).unwrap();

        cfg.system_prompt = "base".to_string();
        apply_workspace_manifest(&mut cfg, dir.path());

        assert_eq!(cfg.system_prompt, "base\nworkspace suffix");
    }
}
