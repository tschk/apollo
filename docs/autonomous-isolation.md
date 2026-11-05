# Isolating autonomous runs

This is a design, not a description of the current implementation. Until
isolation lands, the risks in [the runtime audit](runtime-audit.md#protect-autonomous-workspaces)
still apply. Guards that reject dirty state or limit Git paths reduce the blast
radius, but they cannot tell whether an agent or another process made a change
to a shared checkout.

## Execution workspace

An autonomous run needs a dedicated checkout created from a named, committed
base. The controller creates it before constructing the agent, and binds the
runner, filesystem tools, shell working directory, prompt, skills, plugins,
validation command, and Git operations to the corresponding directory in that
checkout. For a workspace nested inside a repository, preserve its relative
path and restrict changes eligible for publication to that subtree.

A separate local clone is preferred to a linked worktree. A linked worktree
shares the source repository's refs, object database, and other Git metadata.
A clone costs additional disk and setup time, but its index, refs, and working
files are independent. A local clone still reads committed objects from the
source. It must not mutate the source checkout or its index. Record the source
commit and the clone location before running, and stop rather than reuse an
ambiguous or dirty clone after an interruption.

The checkout begins with committed files only. It does not copy staged or
unstaged edits, untracked files, ignored files such as `.env`, or local build
outputs. If the task ledger or a required dependency exists only in those
files, the run must stop with an actionable error rather than silently copy
them. Setup and runtime files created by Apollo before the agent starts are
not automatically eligible for a commit. Keep status and recovery metadata
outside the checkout's publishable files.

## Publication and recovery

The run records its base commit and identifies the files it changed in its
exclusive checkout. Stage only explicit literal paths for those files; never
use `git add -A`. Reject an unexpected staged entry, an unexpected HEAD change,
or a change outside the workspace. Check every Git command's result. Before
pushing, compare the commit's changed paths with the recorded set and refuse
to push any unaccounted path. Push the verified commit with an explicit
refspec; never push an unrelated local branch after a failed commit.

The source checkout remains untouched. A successful result is a commit in the
isolated checkout and, when configured, a push to the selected remote branch.
It is not automatically applied to the source checkout. Tell the operator the
commit and checkout location so they can inspect or integrate the result.
Retain failed or interrupted work for inspection; do not automatically discard
it, reset it, or replay a push whose result is uncertain. Recovery must verify
the checkout, base, branch, and recorded phase before continuing. A concurrent
run must not reuse the same checkout.

## Limits and verification

Exclusive ownership of the checkout is a condition of the attribution claim.
Use a per-run checkout and prevent the controller from sharing it across runs.
An isolated checkout is not an operating-system sandbox: unrestricted shell
commands, plugins, absolute paths, symlinks, and other processes with the same
user permissions can still reach outside it. Do not describe this design as
protection against a malicious agent or another process with filesystem access.

Test the controller against a disposable repository with a nested workspace
and a local bare remote. Include staged changes outside the workspace, tracked
edits, and untracked and ignored private files before a run. Verify that the
original files, index, and stash remain unchanged; only accounted-for changes
in the isolated workspace reach the commit and remote. Cover unusual filenames,
pre-staged entries, a changed HEAD, failure before and after commit, failed
push, restart, and concurrent run attempts. Test that agent tools and the test
command actually use the isolated workspace, not just that Git does.

The cost is another checkout and dependency setup, more disk and startup time,
and an explicit integration step for local results. Measure those costs on a
representative repository before publishing numbers.
