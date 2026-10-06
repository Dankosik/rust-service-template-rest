# Бounded evidence для Definition

Status: ready. Valid as of 2026-10-06, source HEAD `ac88395`.
Это source inspection, а не новая runtime verification. Stop condition: закрыть
фактические развилки пяти принятых работ; не повторять сравнение queue frameworks.

## Attempt lifecycle

**Факт.** [`drive`](../../../crates/infra-jobs/src/attempt.rs) (349–374) держит
`AssertUnwindSafe(future).catch_unwind()`, выбирает ready result перед timeout/force,
даёт cooperative grace и выходит, уничтожая ещё Pending handler. Исходник
resolved `futures-util 0.3.34`,
`src/future/future/catch_unwind.rs` в локальном Cargo registry, содержит
`catch_unwind` только вокруг `f.poll(cx)`; собственного защищённого Drop нет.
`Cargo.lock` — версия dependency authority. Запрошенная страница docs.rs этой
версии была недоступна; использован установленный исходник той же версии.

**Вывод.** Panic при уничтожении ещё Pending handler на timeout/force может
прервать supervisor до обычного outcome/persistence. Это не утверждение о
любом Drop после Ready: typed wrapper в
[`kind.rs`](../../../crates/infra-jobs/src/kind.rs) исполняет внутренний
`handler.run(job).await`, и уничтожение его завершившегося future внутри
wrapper poll попадает в уже существующую границу. Отдельное предположение о
медленной preparation/первом poll не принято как установленный дефект.

**Факт.** [`dispatch_known`](../../../crates/infra-jobs/src/claim.rs) (542–546)
отбрасывает возвращённый JoinHandle. [`engine::Guard`](../../../crates/infra-jobs/src/engine.rs)
(371–405) наблюдает claim/retention/listener/sampling, не attempt supervisor.
[TaskTracker 0.7.19](https://docs.rs/tokio-util/0.7.19/tokio_util/task/task_tracker/struct.TaskTracker.html)
ожидает завершения отслеживаемых задач; его `spawn` возвращает JoinHandle.
Пустой tracker не свидетельствует об успешном результате каждой задачи.

**Контрдоказательство учтено.** Main уже содержит guards для process-owned
работы и failure custody; [runtime owner](../../../docs/architecture/runtime-lifecycle.md#integrating-process-owned-work)
сохраняет primary live failure и degraded shutdown. Отдельного
`crates/infra-runtime` в этом checkout нет. Technical Design должна расширить
существующую custody, а не добавить конкурирующий lifecycle framework.

Decision effect: R1 в specification; reopen при изменении typed wrapper,
drive, tracker spawning или failure propagation. Runtime reproduction ещё
не запускалась и требуется при реализации материального дефекта.

## Durable effect и recovery

**Факт.** [Jobs execution proof](../../../test/tests/jobs/execution.rs) уже
покрывает effect + `complete_in_tx`, stale-claim rollback, unknown committed
completion без повторного business closure и rollback с поздней повторной
попыткой. [Operator proof](../../../test/tests/jobs/operator.rs) проверяет
version arbitration и provisional result до COMMIT acknowledgement.
`test/tests/support/commit_proxy.rs` предоставляет существующий fault seam.

**Факт.** [Outbox proof](../../../test/tests/messaging_outbox.rs),
`durable_consumer_effect_dedupes_same_logical_id_after_broker_window` (914+),
создаёт `messaging_effects(logical_id PRIMARY KEY)` и исполняет `INSERT ... ON
CONFLICT DO NOTHING` через реальный consumer после broker duplicate window.
Этот effect-as-marker пример полезен, но не показывает отдельный marker и
изменение существующего business aggregate в одной Tx.

**Факт.** [Outbox operator contract](../../../docs/postgres-transactional-outbox.md)
требует полного replay horizon и reconciliation после restore. Сохранённый
id/kind/state/version может снова совпасть после восстановления snapshot:
инструкция «invalidate commands» означает operator discard/reinspect, а не
автоматический epoch в schema. Не вводить epoch из одного этого наблюдения.

Decision effect: R2/R3 дополняют материальные пробелы, сохраняют существующие
transaction/consumer proofs. Не утверждаем, что реальный backup/restore уже
выполнялся или что generic dedup отсутствует. Reopen при смене queue schema,
Tx commit classification, replay/retention policy или источника receiver truth.

## Runner и производный сервис

**Факт.** [`make/template.mk`](../../../make/template.mk),
[`make/profile-postgres.mk`](../../../make/profile-postgres.mk) и
[CI integration](../../../.github/workflows/ci.yml) имеют существующие database
и messaging carriers. Они запускают реальные PostgreSQL/PgBouncer/NATS profiles;
новый отдельный cloud runner не нужен. Доступность конкретного запуска и
diagnostic measurements этой фазой не установлены.

**Факт.** [`template-sync-canary.py`](../../../scripts/tests/template-sync-canary.py)
(365–408) уже сохраняет service-owned skill, локальную architecture customization,
settings и unrelated dirty tooling при instructions-only sync; full sync
отказывает для dirty selected target. Это не пробел и не новая рекомендация.

**Граница.** Canary доказывает preservation/sync mechanics. Reference feature
добавляет наблюдаемый business effect через initialized service, обновление
действительно иной template revision и recovery/readback данных.

Decision effect: R4/R5 используют существующие carriers и дополняют поведение.
Установленные library/runtime механизмы достаточны для решения; новая queue
dependency не является живой альтернативой принятому локальному улучшению.
Если Technical Design обнаружит механизм, которого действительно нет, она
проведёт узкое crate comparison до его добавления.

## Малое согласование документации

В [`docs/outbound-webhooks.md`](../../../docs/outbound-webhooks.md) строка
«visible-ASCII header value» расходится с текущим preflight `HeaderValue`.
Это переданный coordinator факт для узкой source-confirmed consistency repair
при реализации; Definition не ужесточает уже принимаемые значения. Ни один
новый production surface или protocol policy из него не следует.
