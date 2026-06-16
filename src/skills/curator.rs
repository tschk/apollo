//! Skill curator — background task that periodically reviews skills and
//! suggests improvements based on usage patterns.
//!
//! Ported from hermes-agent curator pattern. Runs as a spawned tokio task
//! that wakes on interval and scans skill directories for stale/improvable
//! content.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::skills::Skill;

/// Configuration for the curator background task.
#[derive(Debug, Clone)]
pub struct CuratorConfig {
    pub enabled: bool,
    pub interval_hours: u64,
    pub min_idle_hours: u64,
    pub stale_after_days: u64,
    pub archive_after_days: u64,
    pub max_suggestions_per_run: usize,
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_hours: 24,
            min_idle_hours: 2,
            stale_after_days: 90,
            archive_after_days: 180,
            max_suggestions_per_run: 3,
        }
    }
}

/// A suggestion for improving a skill.
#[derive(Debug, Clone)]
pub struct SkillSuggestion {
    pub skill_name: String,
    pub kind: SuggestionKind,
    pub reason: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub enum SuggestionKind {
    Archive,
    Update,
    MissingDeps,
    BrokenRefs,
}

/// The curator runs as a background task.
pub struct SkillCurator {
    config: CuratorConfig,
}

impl SkillCurator {
    pub fn new(config: CuratorConfig, _workspace: PathBuf) -> Self {
        Self { config }
    }

    /// Spawn the background curation loop.
    pub fn spawn(self, skills: Arc<std::sync::RwLock<Vec<Skill>>>) {
        if !self.config.enabled {
            tracing::info!("Skill curator disabled");
            return;
        }

        let interval = Duration::from_secs(self.config.interval_hours * 3600);
        tracing::info!(
            "Skill curator started — every {} hours",
            self.config.interval_hours
        );

        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tick.tick().await;
                let suggestions = Self::audit_skills(&skills, &self.config);
                if suggestions.is_empty() {
                    continue;
                }
                for s in &suggestions {
                    tracing::info!(
                        "[curator] {:?} skill '{}': {}",
                        s.kind,
                        s.skill_name,
                        s.reason
                    );
                }
                // Inject suggestions into the skill manager for review
                if let Ok(mut skills_lock) = skills.write() {
                    for s in &suggestions {
                        Self::apply_suggestion(&mut skills_lock, s);
                    }
                }
            }
        });
    }

    /// Scan skills for issues.
    fn audit_skills(
        skills: &Arc<std::sync::RwLock<Vec<Skill>>>,
        config: &CuratorConfig,
    ) -> Vec<SkillSuggestion> {
        let mut suggestions = Vec::new();
        let skills_lock = match skills.read() {
            Ok(s) => s,
            Err(_) => return suggestions,
        };

        let now = std::time::SystemTime::now();

        for skill in skills_lock.iter() {
            if suggestions.len() >= config.max_suggestions_per_run {
                break;
            }

            // Check if SKILL.md is stale (not modified in N days)
            if let Ok(metadata) = std::fs::metadata(&skill.location) {
                if let Ok(modified) = metadata.modified() {
                    let age = now.duration_since(modified).ok();
                    if let Some(age) = age {
                        let days = age.as_secs() / 86400;
                        if days >= config.archive_after_days {
                            suggestions.push(SkillSuggestion {
                                skill_name: skill.name.clone(),
                                kind: SuggestionKind::Archive,
                                reason: format!("Not modified in {} days", days),
                                detail: "Consider archiving unused skill".to_string(),
                            });
                        } else if days >= config.stale_after_days {
                            suggestions.push(SkillSuggestion {
                                skill_name: skill.name.clone(),
                                kind: SuggestionKind::Update,
                                reason: format!("Stale — {} days since last change", days),
                                detail: "Review and update skill content".to_string(),
                            });
                        }
                    }
                }
            }

            // Check for empty description
            if skill.description.is_empty() {
                suggestions.push(SkillSuggestion {
                    skill_name: skill.name.clone(),
                    kind: SuggestionKind::Update,
                    reason: "Empty description".to_string(),
                    detail: "Add a meaningful description for skill matching".to_string(),
                });
            }
        }

        suggestions
    }

    /// Apply a suggestion — for now just log; future: archive/flag skills.
    fn apply_suggestion(_skills: &mut Vec<Skill>, s: &SkillSuggestion) {
        match s.kind {
            SuggestionKind::Archive => {
                tracing::warn!(
                    "[curator] Skill '{}' candidate for archival: {}",
                    s.skill_name,
                    s.reason
                );
            }
            _ => {
                tracing::info!(
                    "[curator] Skill '{}' improvement needed: {}",
                    s.skill_name,
                    s.reason
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::Skill;
    use std::path::PathBuf;

    fn make_skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            location: PathBuf::from("/tmp/test/SKILL.md"),
        }
    }

    #[test]
    fn test_audit_empty_descriptions() {
        let skills = Arc::new(std::sync::RwLock::new(vec![
            make_skill("good", "Has a description"),
            make_skill("bad", ""),
        ]));

        let config = CuratorConfig {
            max_suggestions_per_run: 10,
            ..Default::default()
        };

        let suggestions = SkillCurator::audit_skills(&skills, &config);
        let empty_desc = suggestions.iter().find(|s| s.skill_name == "bad");
        assert!(empty_desc.is_some());
        assert!(matches!(empty_desc.unwrap().kind, SuggestionKind::Update));

        let good = suggestions.iter().find(|s| s.skill_name == "good");
        assert!(good.is_none());
    }
}
