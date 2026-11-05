# Runtime capability and safety audit

Source review against main on 2026-11-05. The cases below describe paths
visible in code; reproduce them with integration tests before treating a fix
as complete.

## Protect autonomous workspaces

The original `apollo autonomous` implementation uses the current workspace and defaults to running
`cargo test --workspace`, then committing and pushing to `origin main` when
the agent succeeds and tests pass. In `src/autonomous.rs`, the pre-task
`git stash push` hides tracked changes without restoring them. From a nested
workspace, Git stashes tracked changes elsewhere in the enclosing repository
too. The ledger is read before the stash, so its contents can differ from the
file the agent subsequently reads.

The stash does not include untracked files. After a successful agent turn and
test command, `git add -A` can stage and commit those pre-existing files, then
push them to the configured remote. A pre-staged file outside a nested
workspace can also enter the commit if a prior `Running` state skips the stash
or the stash fails. The loop ignores stash, add, and commit failures, and can
attempt a push even when the new commit failed. Its status file can be staged
in its intermediate `Running` state.

These Git effects were reproduced in a disposable nested repository and local
bare remote, not by running a full agent turn. They demonstrate possible
disclosure to a remote configured by the operator and loss of access to local
changes. They do not establish that an untrusted party can trigger a run or
control its workspace or remote.

The interim mitigation removes automatic stashing, rejects a pre-existing
index or modified tracked workspace files, excludes pre-existing untracked
files, stages literal paths, and checks the resulting commit before pushing.
It also refuses to resume a persisted running state automatically. These
checks reduce the blast radius but **do not establish authorship**: another
process can change a file during a turn, and timing alone cannot attribute it
to the agent. `apollo autonomous` now creates a private clone for the run and
binds the runner and validation to it. Under exclusive access, changes in that
checkout are attributable to the run and its processes, not necessarily to the
model. The clone is not a sandbox against other same-user processes or shell
commands. See [isolated runs](autonomous-isolation.md) for recovery and limits.

The initial isolated-checkout implementation resolved the workspace path but
compared it against a constructed clone-root path without resolving that root
again. This relied on its freshly created path components remaining unchanged.
Containment now resolves both sides and fails closed when resolution fails;
this is a preparation-time check, not protection from later same-user writes.

## Conversation history was not sent to the model

Apollo persisted conversation turns, but its rx4 bridge wrote loaded history
only to rx4's live message projection. Rx4 builds provider requests from a
separate session log, so the model did not receive that saved history. A
real-turn regression test reproduced the omission. The bridge now appends
history to the session, and coding swarm deployments use unique conversation
IDs to prevent a later deployment from receiving an earlier worker's history.

## Make parallel agents safely steerable

Coding swarm workers share a workspace. An integration workflow could isolate
their worktrees and report changed files and conflicts before applying results.

The runner has one steering queue for every active turn. Concurrent turns can
consume one another's instructions. It also writes steering to rx4's live
message projection, but rx4 builds requests from its session log, so the model
does not receive those messages. Address steering to a specific turn, append it
to the session, and test a parent turn running alongside a worker. In
`src/swarm/delegation.rs`, limit checks and reservations are separate
operations; reserve capacity atomically and make completion and cancellation
idempotent.

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
