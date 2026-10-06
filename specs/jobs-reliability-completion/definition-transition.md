# Definition Transition

```text
status: ready
owner: Definition
result: specs/jobs-reliability-completion/spec.md
review: specs/jobs-reliability-completion/definition-review.md — PASS
movement_evidence: Intent и R1–R5 закрывают принятые пять работ; источник Pending-drop/supervisor gap проверен; baseline reuse и различие durable acceptance/effect/unknown/recovery определены; независимый review не нашёл material divergence
reopen_owner: none
next_owner: Technical Design — System / Integration Design, затем Rust Code / Ownership Design при открытом placement
```

## Authority и boundary

[Intent](intent.md), [specification](spec.md), [bounded source evidence](research/baseline.md)
и [review](definition-review.md) — актуальный вход следующему актору.
Checkout: `/Users/daniil/.codex/worktrees/jobs-reliability-followup/rust-service-template-rest`.
Baseline HEAD: `ac88395be87cba3a1e0587f533dc50a71e358c8d`.
Branch: `codex/jobs-reliability-followup-20261005`; existing PR #240.
Root остаётся continuation coordinator. Этот actor выполнил только Definition.

Пользователь разрешил все пять улучшений, local edits/bounded real proof,
commit/push и отдельный PR/create/update. Merge main, production effects и
покупки вне authority. Чужие изменения сохраняются.

Technical Design не пропускается: нужно выбрать attempt containment/custody,
backup/receiver truth и replay orchestration, business marker ownership,
существующий workload carrier, derived feature composition/upgrade path.
Технический выбор не требует подтверждения пользователем. Library/framework
research повторяется только для нового решения; default — текущие engine,
Tx APIs, fixture seams и runner. Конкретные tests выбирает Implementation.

Phase proof: static consistency; final `make docs-check` PASS (1964 total,
1159 unique, 1128 OK, 0 errors); `git diff --check` PASS.
Runtime/DB/crash/restore/load/derived-service execution не выполнялись;
проверки прежнего PR не подменяют proof нового outcome. Все ссылки и fragments
проверены после добавления review и Transition.

Reopen при изменении meaning/finality — Specification; source contradiction —
Research; requester scope/authority — Intake. Недостаток mechanism/placement
или доступного proof carrier возвращается Technical Design, не пользователю
как вопрос выбора архитектуры.
