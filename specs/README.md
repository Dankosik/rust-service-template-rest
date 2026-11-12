# Specs

Store active task-local records for real repository work here.

Use `specs/<topic>/` only while a task needs persistent research, decision,
design, planning, or validation artifacts. After completion, move each
durable decision into code (a name, type, test, or comment on the reason) or,
when code cannot hold it, a record under `docs/decisions/`, then delete the
bundle; Git keeps the history.

Do not create synthetic or archival bundles as examples.
