# Isolating autonomous runs

`apollo autonomous` runs from a private standalone clone of the source
repository's committed `HEAD`. The clone has its own working files, index,
refs, and Git metadata. It costs additional disk space and setup time, but
unlike a linked worktree it does not share Git metadata with the source.
The private state directory requires Unix permissions, so this mode currently
fails closed on non-Unix systems.

The controller creates the clone before constructing the runner. For a nested
workspace, it uses the same relative directory in the clone. The agent's
filesystem tools, shell working directory, prompt, validation command, and Git
operations use that directory. The run disables shared skills, host plugins,
dynamic tools, conversational memory, and Telekinesis. The supplied config is
read at startup and is not reloaded during a turn. Runtime memory and status
are stored outside the checkout, in the private run directory.

The clone contains committed files only. Staged and unstaged edits, untracked
files, and ignored files that are not committed are not copied. The task ledger
must be committed and present inside the workspace. Dependencies, credentials,
or build outputs that exist only in the source checkout are not copied; a run
that needs them must arrange them separately. The clone may therefore need a
fresh dependency setup.

## Publication and recovery

The controller checks the starting index and HEAD, stages explicit literal
paths, and checks the resulting commit's parent and paths. It refuses changes
outside a nested workspace and refuses to push when the remote branch no longer
matches the starting commit. A successful commit is pushed to the source's
configured push destination. It is not applied to the source checkout.

Checkout preparation resolves filesystem paths on both sides of its containment
checks and fails if either path cannot be resolved. Publication separately checks
Git-reported repository-relative names against the workspace prefix. That
lexical Git check limits what is staged; it is not a filesystem sandbox.

Apollo prints the clone and recovery-marker paths, or includes them in the
error if checkout preparation fails after creating the marker. It retains both
after a run, including failed or interrupted runs. Another run for the same
source workspace is refused while the marker exists, even with `--start`. Automatic
`--resume` is unavailable. Inspect the retained checkout, its status, its
commit, and the remote before deciding whether to integrate changes or retry a
push. After inspection and any manual recovery, remove the printed marker to
allow a new run. Removing it does not remove the retained checkout.

## Attribution and limits

With exclusive access to the private checkout, a published change came from
the run and its processes, rather than from the source checkout's pre-existing
work. That does not identify the model as the writer: tools, validation
commands, and Git hooks can write files too.

The checkout is not an operating-system sandbox. Shell commands, absolute
paths, symlinks, Git hooks, and other processes with the same user permissions
can reach outside the checkout or modify it. A malicious process or concurrent
writer with access to the private directory defeats the exclusive-access
assumption. The earlier shared-checkout guards remain blast-radius reductions,
not proof of authorship.
