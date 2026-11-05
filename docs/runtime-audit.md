# Runtime capability and safety audit

Source review against main on 2026-11-05. The cases below describe paths
visible in code; reproduce them with integration tests before treating a fix
as complete.

## Protect autonomous workspaces

`src/autonomous.rs` stashes tracked changes before a task without restoring
them. Untracked files remain in the workspace and a later `git add -A` can
include them in an automatic commit. Git failures are not propagated to the
task status. Run tasks in an isolated worktree, or refuse dirty workspaces;
propagate Git failures and test tracked and untracked user changes, restart,
and failed push scenarios. Account for the status file itself when deciding
whether a workspace is dirty.

## Make parallel agents safely steerable

`src/tools/coding_swarm.rs` passes the same base chat ID on each deployment;
workers in `src/agent/loop_runner.rs` derive their chat IDs from it, so later
deployments can load earlier workers' histories. Workers also share a
workspace, despite the tool's isolation claim. Give each deployment unique
conversation IDs and either isolate worktrees or describe the shared workspace
accurately. A useful next step is an integration workflow that reports each
worker's changed files and conflicts before applying its result.

The runner also has one steering queue for every active turn. Concurrent turns
can consume one another's instructions. Address steering to a specific turn and
test a parent turn running alongside a worker. In `src/swarm/delegation.rs`,
limit checks and reservations are separate operations; reserve capacity
atomically and make completion and cancellation idempotent.

## Trustworthy, useful recall

`src/memory/search.rs` follows links when searching workspace memory files,
without checking that targets remain in the workspace. Confine file opens,
including against link races, and test both linked files and directories.

`src/memory/surreal.rs` filters embedding candidates by dimension but not
model identity. Two models can have the same dimensions and incompatible
embedding spaces. Carry model identity through retrieval, define how legacy
rows are handled, and test switching between same-dimension models.

Graph node IDs in `src/memory/graph.rs` use a normalized 48-character prefix;
different inputs can collide and overwrite a node. Matching also scans only an
unordered first 200 nodes. Preserve existing edge references when fixing IDs,
and replace the fixed scan with indexed or paginated retrieval. Together with
provenance and confidence, reliable graph retrieval could support a useful
agent-generated map of related decisions and unresolved hypotheses.

## Recover channel interruptions

The Discord gateway explicitly re-identifies instead of resuming after a
disconnect (`src/channels/discord_gateway.rs`), so messages in the gap can be
lost. Retain session and sequence state, implement RESUME with invalid-session
fallback, and exercise reconnect and replay against a mock gateway before
claiming reliable delivery.
