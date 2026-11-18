//! WorktreeTool — create and remove isolated git worktrees for parallel/sandboxed work.
//!
//! Each worktree gets its own branch and working directory under `.worktrees/`.
//! Use before risky refactors or parallel agent tasks — remove when done.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::traits::*;

pub struct WorktreeTool {
    workspace: PathBuf,
}

impl WorktreeTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }

    fn worktrees_dir(&self) -> PathBuf {
        self.workspace.join(".worktrees")
    }
}

#[derive(Deserialize)]
struct WorktreeArgs {
    /// Action: "add", "remove", "list"
    action: String,
    /// Worktree name (used as directory name and branch name)
    #[serde(default)]
    name: String,
    /// Base branch (default: current HEAD). Only for "add".
    #[serde(default)]
    base_branch: String,
}

#[async_trait]
impl Tool for WorktreeTool {
    fn name(&self) -> &str {
        "worktree"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "worktree".to_string(),
            description: "Manage isolated git worktrees for parallel or sandboxed work. \
                'add' creates a new branch + worktree in .worktrees/<name>. \
                'remove' deletes the worktree and its branch. \
                'list' shows all active worktrees."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "remove", "list"],
                        "description": "add = create worktree, remove = clean up, list = show all"
                    },
                    "name": {
                        "type": "string",
                        "description": "Worktree name (becomes the branch name and directory name)"
                    },
                    "base_branch": {
                        "type": "string",
                        "description": "Branch to base the new worktree on (default: current HEAD)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: &str) -> anyhow::Result<ToolResult> {
        let args: WorktreeArgs = serde_json::from_str(arguments)?;

        match args.action.as_str() {
            "list" => {
                let output = run_git(&self.workspace, &["worktree", "list", "--porcelain"]).await?;
                Ok(ToolResult::success(if output.is_empty() {
                    "No worktrees.".to_string()
                } else {
                    output
                }))
            }

            "add" => {
                if args.name.is_empty() {
                    return Ok(ToolResult::error("name is required"));
                }
                // Sanitize name
                let name: String = args
                    .name
                    .chars()
                    .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                    .collect();
                if name.is_empty() {
                    return Ok(ToolResult::error("invalid name"));
                }

                let worktree_path = self.worktrees_dir().join(&name);
                let branch = format!("worktree/{}", name);

                // Create base dir
                tokio::fs::create_dir_all(self.worktrees_dir()).await?;

                // git worktree add -b <branch> <path> [base]
                let mut cmd_args = vec!["worktree", "add", "-b"];
                let branch_ref = branch.as_str();
                let path_str = worktree_path.to_string_lossy().to_string();
                cmd_args.extend_from_slice(&[branch_ref, &path_str]);

                if !args.base_branch.is_empty() {
                    // We'll pass as separate arg below
                }

                let mut full_args = vec!["worktree", "add", "-b", branch_ref, &path_str];
                let base = args.base_branch.clone();
                if !base.is_empty() {
                    full_args.push(&base);
                }

                let output = run_git(&self.workspace, &full_args).await?;

                Ok(ToolResult::success(format!(
                    "Created worktree '{}' at {} on branch {}\n{}",
                    name,
                    worktree_path.display(),
                    branch,
                    output
                )))
            }

            "remove" => {
                if args.name.is_empty() {
                    return Ok(ToolResult::error("name is required"));
                }
                let name: String = args
                    .name
                    .chars()
                    .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
                    .collect();

                let worktree_path = self.worktrees_dir().join(&name);
                let branch = format!("worktree/{}", name);

                // Remove worktree
                let path_str = worktree_path.to_string_lossy().to_string();
                let _ = run_git(
                    &self.workspace,
                    &["worktree", "remove", "--force", &path_str],
                )
                .await;

                // Delete the branch
                let _ = run_git(&self.workspace, &["branch", "-D", &branch]).await;

                Ok(ToolResult::success(format!(
                    "Removed worktree '{}' and branch {}",
                    name, branch
                )))
            }

            other => Ok(ToolResult::error(format!("Unknown action: {}", other))),
        }
    }
}

async fn run_git(workspace: &PathBuf, args: &[&str]) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).current_dir(workspace);
    // Checkout runs hooks and filters. Withhold apollo's configuration and AI/channel
    // keys, not arbitrary secret-looking names: Git needs GIT_CONFIG_KEY_*, GitHub
    // and Hugging Face tokens, and may use custom credentials in filters/helpers.
    for (name, _) in std::env::vars_os() {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("APOLLO_")
            || matches!(
                upper.as_str(),
                "ANTHROPIC_API_KEY"
                    | "OPENAI_API_KEY"
                    | "GOOGLE_API_KEY"
                    | "GOOGLE_GENERATIVE_AI_API_KEY"
                    | "GEMINI_API_KEY"
                    | "XAI_API_KEY"
                    | "DEEPSEEK_API_KEY"
                    | "GROQ_API_KEY"
                    | "TOGETHER_API_KEY"
                    | "MISTRAL_API_KEY"
                    | "FIREWORKS_API_KEY"
                    | "OPENROUTER_API_KEY"
                    | "PERPLEXITY_API_KEY"
                    | "COHERE_API_KEY"
                    | "CO_API_KEY"
                    | "CEREBRAS_API_KEY"
                    | "VENICE_API_KEY"
                    | "MOONSHOT_API_KEY"
                    | "MINIMAX_API_KEY"
                    | "SILICONFLOW_API_KEY"
                    | "AI_GATEWAY_API_KEY"
                    | "CLOUDFLARE_API_KEY"
                    | "VOYAGE_API_KEY"
                    | "BRAVE_SEARCH_API_KEY"
                    | "TELEGRAM_BOT_TOKEN"
                    | "DISCORD_BOT_TOKEN"
                    | "DISCORD_TOKEN"
                    | "SLACK_BOT_TOKEN"
                    | "SLACK_APP_TOKEN"
            )
        {
            cmd.env_remove(name);
        }
    }
    let output = cmd.output().await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() && !stderr.is_empty() {
        return Err(anyhow::anyhow!("git error: {}", stderr.trim()));
    }

    Ok(if stdout.is_empty() { stderr } else { stdout })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn git_worktree_environment_boundary() {
        // Isolate environment changes from other tests and use synthetic credentials only.
        if std::env::var_os("APOLLO_WORKTREE_TEST_CHILD").is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tools::worktree::tests::git_worktree_environment_boundary",
                    "--nocapture",
                ])
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap())
                .env("HOME", home.path())
                .env("APOLLO_WORKTREE_TEST_CHILD", "1")
                .env("ANTHROPIC_API_KEY", "worktree-provider-test")
                .env("APOLLO_TELEGRAM_TOKEN", "worktree-channel-test")
                .env("GH_TOKEN", "worktree-gh-test")
                .env("GITHUB_TOKEN", "worktree-github-test")
                .env("HF_TOKEN", "worktree-hf-test")
                .env("FILTER_SECRET", "worktree-filter-test")
                .env("SSH_AUTH_SOCK", "/worktree-test/agent.sock")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "apollo.test-config")
                .env("GIT_CONFIG_VALUE_0", "worktree-config-test")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child test failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let root = tempfile::tempdir().unwrap();
            let repo = root.path().to_path_buf();
            run_git(&repo, &["init"]).await.unwrap();
            run_git(&repo, &["config", "user.email", "worktree@example.invalid"])
                .await
                .unwrap();
            run_git(&repo, &["config", "user.name", "Worktree test"])
                .await
                .unwrap();
            run_git(&repo, &["config", "commit.gpgsign", "false"])
                .await
                .unwrap();
            run_git(
                &repo,
                &[
                    "config",
                    "credential.helper",
                    "!f() { printf 'username=test\\npassword=%s\\n' \"$GH_TOKEN\"; }; f",
                ],
            )
            .await
            .unwrap();
            tokio::fs::write(repo.join("seed.txt"), "stored\n")
                .await
                .unwrap();
            tokio::fs::write(repo.join(".gitattributes"), "*.txt filter=probe\n")
                .await
                .unwrap();
            run_git(&repo, &["add", "."]).await.unwrap();
            run_git(&repo, &["commit", "-m", "fixture"])
                .await
                .unwrap();

            let hooks = repo.join("test-hooks");
            tokio::fs::create_dir(&hooks).await.unwrap();
            let hook = hooks.join("post-checkout");
            tokio::fs::write(
                &hook,
                "#!/bin/sh\n\
                 test \"${GH_TOKEN-}\" = worktree-gh-test || exit 1\n\
                 test \"${GITHUB_TOKEN-}\" = worktree-github-test || exit 1\n\
                 test \"${HF_TOKEN-}\" = worktree-hf-test || exit 1\n\
                 test \"${SSH_AUTH_SOCK-}\" = /worktree-test/agent.sock || exit 1\n\
                 test \"$(git config apollo.test-config)\" = worktree-config-test || exit 1\n\
                 printf 'protocol=https\\nhost=worktree.invalid\\n\\n' | git credential fill | grep -q '^password=worktree-gh-test$' || exit 1\n\
                 test -z \"${ANTHROPIC_API_KEY-}${APOLLO_TELEGRAM_TOKEN-}${APOLLO_WORKTREE_TEST_CHILD-}\" || exit 1\n\
                 printf 'hook-ok\\n' > hook-observed\n",
            )
            .await
            .unwrap();
            tokio::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
                .await
                .unwrap();
            run_git(&repo, &["config", "core.hooksPath", hooks.to_str().unwrap()])
                .await
                .unwrap();
            run_git(&repo, &["config", "filter.probe.clean", "cat"])
                .await
                .unwrap();
            run_git(
                &repo,
                &[
                    "config",
                    "filter.probe.smudge",
                    "test \"${FILTER_SECRET-}\" = worktree-filter-test && \
                     test -z \"${ANTHROPIC_API_KEY-}${APOLLO_TELEGRAM_TOKEN-}\" && \
                     cat >/dev/null && printf 'filtered\\n'",
                ],
            )
            .await
            .unwrap();
            run_git(&repo, &["config", "filter.probe.required", "true"])
                .await
                .unwrap();

            let tool = WorktreeTool::new(repo.clone());
            let added = tool
                .execute(r#"{"action":"add","name":"probe","base_branch":"HEAD"}"#)
                .await
                .unwrap();
            assert!(!added.is_error, "{}", added.output);
            let worktree = repo.join(".worktrees/probe");
            assert_eq!(
                tokio::fs::read_to_string(worktree.join("seed.txt"))
                    .await
                    .unwrap(),
                "filtered\n"
            );
            assert_eq!(
                tokio::fs::read_to_string(worktree.join("hook-observed"))
                    .await
                    .unwrap(),
                "hook-ok\n"
            );
            assert_eq!(
                run_git(&worktree, &["branch", "--show-current"])
                    .await
                    .unwrap()
                    .trim(),
                "worktree/probe"
            );
            let listed = tool.execute(r#"{"action":"list"}"#).await.unwrap();
            assert!(listed.output.contains(worktree.to_str().unwrap()));
            let removed = tool
                .execute(r#"{"action":"remove","name":"probe"}"#)
                .await
                .unwrap();
            assert!(!removed.is_error, "{}", removed.output);
            assert!(!worktree.exists());
            assert!(run_git(&repo, &["branch", "--list", "worktree/probe"])
                .await
                .unwrap()
                .is_empty());
        });
    }
}
