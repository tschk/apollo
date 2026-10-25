# Apollo autoresearch

`apollo autoresearch` runs bounded experiments against a numeric local metric.
It measures a baseline, asks the agent for one hypothesis at a time, validates
the candidate, keeps only improvements, and records every decision in a TOML
ledger.

Create `.apollo/autoresearch.toml` in the workspace:

```toml
objective = "Reduce warm startup latency"
metric_command = "cargo bench --bench startup -- --output-format json | jq -r .median_ms"
direction = "minimize"
validation_command = "cargo test --workspace --all-features"
validation_retries = 2
command_timeout_secs = 300
samples = 3
max_iterations = 10
max_duration_secs = 1800
ledger_path = ".apollo/autoresearch-ledger.toml"
```

Run it with:

```bash
apollo autoresearch --workspace .
apollo autoresearch --workspace . --resume
```

The metric command must print at least one finite numeric value. Commands in
the specification are trusted local code and run through `sh -c`; do not use a
specification copied from an untrusted source. The workspace must be clean at
startup. Rejected iterations are restored to their checkpoint; ignored
configuration/state files are restored to their pre-iteration contents, and
untracked files created by that iteration are removed. Build output under
`target/`, dependency trees, and other volatile build roots are treated as
disposable process state. The remaining ignored snapshot is disk-backed and
bounded to 16 MiB per file and 64 MiB total. Run autoresearch in a dedicated
worktree when experimenting with valuable local files.

Baseline validation and metric commands must not modify tracked files, change
the branch, or move `HEAD`; the run aborts and restores the baseline if they
do. The controller scrubs secret-bearing environment variables from its
subprocesses and makes its acceptance commit with repository hooks disabled.
The restricted experiment agent receives repository instructions but not
`USER.md` or `MEMORY.md`.

If `ledger_path` is inside the workspace, it must be Git-ignored; an external
ledger path is also supported. This keeps the durable ledger from becoming an
unrelated dirty change after an accepted iteration.

Accepted iterations are committed locally as `autoresearch: iteration N`.
Pushing is intentionally not automatic.

The ledger records the branch, the accepted commit, a fingerprint of the
metric/validation definition, and a unique run chat id. `--resume` refuses to
continue if the branch, HEAD, or experiment definition no longer matches the
ledger.

The autoresearch runner exposes only the runtime and filesystem tool groups to
the experiment agent. Network, messaging, memory, MCP, dynamic tools, host
plugins, and workspace skills are not ambient capabilities for this workflow;
history, personal-context injection, ZKR recall/capture, and reflection are
also disabled.

Validation and metric processes are bounded by `command_timeout_secs`, and a
validation command may be retried with `validation_retries`. The whole run is
bounded by `max_iterations` and `max_duration_secs`.

Apollo also records estimated system, history, and tool-definition context in
aggregate counters in the existing cost tracker. The estimates use four
characters per token and are useful for comparing harness configurations, not
for billing.

## Design notes

The loop follows two useful ideas from adjacent agent systems: bounded
autonomous runs with explicit quality gates and budgets (as in
[Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent)), and
capability-oriented access instead of ambient tools (as in
[Cloudflare OS](https://github.com/cloudflare/cloudflare-os)). Apollo keeps the
implementation local and Git-backed: there is no hosted worker or remote
control plane in this workflow.
