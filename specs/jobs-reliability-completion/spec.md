# Jobs reliability completion: поведенческий контракт

Status: ready. Independent Specification Review: [PASS](definition-review.md).
Authority: [Intent](intent.md). Baseline/evidence:
[source findings](research/baseline.md), HEAD `ac88395`, 2026-10-06.

## Outcome и границы

Пять работ ниже вместе закрывают принятый outcome. Первая устраняет дефект и
замыкает failure custody; остальные дают исполняемые примеры и операционное
доказательство применимости существующих контрактов. Успех отдельного теста,
queue completion или только документация не заменяют общий результат.

Сохраняются PostgreSQL jobs engine, worker/process budgets, текущие job-kind,
attempt/generation/lease, retry/snooze/release и operator contracts,
transaction API, HTTP/webhook/outbox profiles. Новая runtime business schema,
queue framework, deployment, global fairness, quota и новые knobs не входят.
Реальные effects репетиции ограничены disposable local/CI средой.

## R1. Каждая принятая попытка имеет наблюдаемый конец

Для acknowledged claim supervisor владеет handler, admission и completion
bookkeeping до их окончания или явно зарегистрированной неопределённости.
Panic при уничтожении ещё Pending handler после cancellation не должен
молча обрывать этот путь, терять slot custody или делать cleanup успешным
только потому, что tracker опустел. Поддерживаемая граница — Rust unwinding;
abort/process kill, double panic during unwind и неприостанавливаемый
блокирующий код не превращаются в гарантированно восстановимый panic.

| Триггер | Наблюдаемый исход |
| --- | --- |
| Handler ready до выбора cancellation | Сохраняется нынешний результат, включая sanitized panic failure; новый код не переписывает известный outcome на release. |
| Timeout; Pending handler не завершился за существующий grace; его Drop panics | Panic содержится внутри attempt boundary и наблюдается без payload; timeout остаётся причиной retry/exhaustion. При оставшемся бюджете выполняется обычный fenced outcome; иначе только uncertainty/lease recovery. Последующая допустимая работа продолжает исполняться. |
| Forced drain; тот же Pending Drop panic | Panic наблюдается без payload; release/refund остаётся disposition forced cancellation, если fenced persistence ещё допустима. Отсутствие acknowledgement остаётся uncertainty. Forced drain остаётся degraded shutdown по текущей политике. |
| После cancellation handler подтверждает success | Success сохраняется; ошибка/snooze/panic после cancellation берут причину cancellation по существующему правилу. |
| Неожиданный unwind/выход самого supervisor вне нормального retirement | Engine/process failure custody получает видимый failure; отсутствующий outcome не считается успехом, row восстанавливается через lease. При live failure primary exit `1`, при исключительно failed cleanup после stop — существующий degraded exit `3`; предшествующий primary failure не заменяется. |

Unexpected supervisor failure — нарушение template-owned supervision, а не
обычный contained handler panic. Нормальное окончание attempt не останавливает
engine. Не расширять deadline, не добавлять повторное бизнес-выполнение для
«восстановления» panic и не превращать known/unknown persistence в одинаковый
результат. Число удерживаемых attempt/bookkeeping единиц остаётся ограниченным
нынешним admission. Панический payload и job payload не попадают в диагностику.

Nearest falsifiers: Pending future с panicking Drop на timeout и force;
known Ready control; неожиданное окончание supervisor; последующая работа и
освобождение admission при сохранении fenced/unknown outcomes. Preparation
проверяется только для причинно подтверждённого соседнего дефекта: гипотеза
«первый poll опоздал» не даёт права менять precedence или deadlines.

## R2. Исполняемая crash/restore/reconciliation репетиция

Оператор получает воспроизводимую локальную/CI процедуру с реальными worker,
PostgreSQL и нужными retained profiles. Она проводит durable acceptance через
job, outbox публикацию/consumer и webhook receiver; механически общие этапы
можно разделить, но доказательство каждого пересекаемого effect boundary видно.
Crash означает настоящее окончание процесса без graceful drain, а restore —
восстановление настоящего backup в disposable database, не UPDATE состояния
очереди. Fault injection использует существующие fixture/proxy seams.

Репетиция различает: (a) не подтверждённую/rollback acceptance; (b) committed
job до effect; (c) effect committed/получен receiver, а completion/ACK потерян;
(d) повтор после lease expiry или operator redrive; (e) возврат БД к snapshot.
Неподтверждённую транзакцию сначала сверяют по той же operation identity.
Обычный restart сохраняет acknowledged durable intent. После restore
гарантии ограничены содержимым backup: запись, принятая позже snapshot,
может отсутствовать. Отчёт явно указывает этот RPO-разрыв; не обещает нулевую
потерю поздних acceptance без отдельного источника intent.

Перед replay оператор останавливает старых writers/consumers для восстановленной
БД, сверяет совместимость migrations/kinds и заново инспектирует очередь.
Сохранённые до restore команды и receipts отбрасывает даже если id/kind/state/
version снова совпали; эта процедура не полагается на техническую rejection
такой команды. Epoch/schema change для этого не требуется.

Receiver/consumer evidence, переживающее producer restore, является независимой
истиной внешнего эффекта. На репетиции receiver хранит stable logical identity
и результат в своём durable fixture store, который не откатывается вместе с
producer database. Старое completed в queue, lease fencing, broker duplicate
window и HTTP 2xx по отдельности не заменяют этот readback.

Для каждой восстановленной логической операции итоговая таблица сверки
показывает acceptance/snapshot membership, queue state/generation, receiver
или business effect identity, решение и receipt: effect подтверждён и повтор
безопасно поглощён; effect отсутствует и разрешён same-identity replay; либо
effect неизвестен и операция остаётся на ручной reconciliation без blind replay.
Последняя ветвь намеренно воспроизводится недоступностью receiver readback:
отсутствие ответа не считается доказательством отсутствия эффекта.

Репетиция завершается после остановки workload, reconciliation всех выбранных
identities, bounded shutdown и cleanup только её disposable ресурсов. Сохраняет
revision, versions/config, backup boundary, fault milestones, before/after
durable observations, recovery duration, команды запуска и фактический cleanup.
Положительная ветвь обязана достигать подтверждённого результата без повторного
business effect; ambiguous ветвь обязана явно удержать неопределённость. Это
локальное operational proof, не production DR certification или SLA.

## R3. Business dedup — отдельный marker и настоящая mutation

Исполняемый recipe содержит marker логической операции и отдельный изменяемый
business aggregate: например, счётчик чтений материала. Identity включает
нужный business scope и не равна attempt generation, delivery attempt или
новому queue UUID после replay. Повтор с тем же immutable содержимым возвращает
ранее установленный результат/признаётся duplicate; та же identity с другим
содержимым отвергается как конфликт без второй mutation.

Marker admission и business mutation совершаются на одном caller-owned `Tx`.
При новом marker агрегат меняется ровно один раз; проигравший concurrent
same-identity caller наблюдает duplicate после судьбы победителя. Rollback
отменяет и marker, и mutation, поэтому последующий legitimate replay может
сделать effect. `CommitUnknown` не запускает closure повторно вслепую:
исполняется readback/reconciliation той же identity, затем безопасная
same-identity попытка допускается только по результату такого решения.
Пример использует существующую Tx классификацию и не объявляет транспортную
ошибку доказательством rollback.

Delivery acknowledgement/completion следует за committed effect. Для job,
который делает `complete_in_tx`, stale claim откатывает предшествующую mutation;
для consumer acknowledgement происходит после committed Tx. Учебные tables
и marker owner живут в example/fixture или производном сервисе, а не в
универсальной runtime migration. Нынешний effect-as-PK outbox пример остаётся
валидным и не требует параллельной копии каждого уже достаточного теста.

Marker в reference example не имеет TTL: retained failed jobs/redrive/restore
дают неограниченный по умолчанию replay horizon. Completed queue TTL и broker
duplicate-window expiry могут удалить транспортную память, но не business
identity. Recipe демонстрирует тот же business result после transport TTL,
нового transport/job identity и разрешённого redrive. Если adopter хочет
конечный marker TTL, он обязан ограничить replay до удаления marker либо
иметь reconciliation authority на весь более длинный horizon; это указание
границы применения, не новый template config.

Nearest falsifiers: rollback оставил marker; concurrent duplicates дважды
изменили агрегат; lost COMMIT породил слепой replay; конфликтующий payload
был принят; transport retention/redrive обошёл business identity. Достаточные
существующие proofs переиспользуются, новые cases закрывают именно эти gaps.

## R4. Ограниченная диагностическая нагрузка и измеримый recovery

Существующий local/CI integration carrier получает воспроизводимый workload
с baseline и faults: остановленный worker и backlog, занятый PostgreSQL pool,
недоступный NATS, затем освобождение/restart и drain. Это диагностическое
упражнение, не benchmark всех возможных deployment размеров.

Принятый reference envelope: конечная пачка 128 логических операций с payload
не более 1 KiB; не более четырёх локальных attempt slots; producer останавливается
после пачки; одна активная fault за раз не дольше 5 секунд; recovery observation
не дольше 180 секунд после снятия fault. Весь отдельный сценарий не дольше
5 минут. Это выбранные границы эксперимента, не measured capacity и не
production SLO. Technical Design определяет допустимую mode-aware pool
конфигурацию и policy внутри этого envelope до запуска; не подгоняет границы
после неуспеха. Для backlog worker не запускается до подтверждённого накопления.

Receipt фиксирует exact workload, baseline/fault sample, environment/revision,
queue depth и oldest age, claimed/running/terminal/unknown counts, attempts,
pool used/wait/failure, owned admission и completion bookkeeping, process RSS
и CPU sample, recovery time и конечные business counts. Нельзя объявить hard
memory cap по одному RSS peak: hard resource assertions относятся к имеющимся
admission/pool bounds; RSS/CPU — измеренная характеристика этого запуска.

Успех: после восстановления все 128 принятых operations в положительном
сценарии достигают подтверждённого business результата в заданном окне без
повторного эффекта; backlog перестаёт расти после stop producer и затем
опустошается; занятость admission/bookkeeping возвращается к idle; pool не
превышает настроенную capacity; нет осиротевших owned tasks или unbounded
накопления завершённых результатов. В outage отсутствие прогресса видно и не
маскируется success. Исчерпание попыток или recovery deadline — failed
сценарий с диагностикой, а не silently skipped proof.

При обнаружении дефекта исправляется его текущая причина. Новые quotas,
fairness, CPU pool или config knobs требуют конкретного измеренного нарушения
этого outcome и узкого возврата к Technical Design; из одного роста backlog
при остановленном worker они не следуют. Не размножать full builds по
workload/database комбинациям.

## R5. Производный сервис с работающей business feature

Из initializer получается disposable derived service с retained jobs,
PostgreSQL и нужными для общей репетиции messaging/webhook profiles. Его
безвредная функция принимает `operation_id`, `article_id` и неизменяемое
содержимое операции «материал прочитан», подтверждает durable acceptance,
асинхронно увеличивает счётчик статьи через R3 и позволяет прочесть результат.
Повторная доставка операции не увеличивает счётчик снова. Это реальный
business путь через composed service/worker, а не только прямой SQL из теста.
Feature доступна лишь локальному упражнению; её transport/wiring выбирает
Technical Design, не добавляя public production policy шаблона.

Два различных operation_id одной статьи дают два чтения; повтор одного — одно;
один operation_id с другой статьёй/содержимым отвергается. Committed accepted
operation переживает worker restart, а после producer restore применяется R2,
включая границу snapshot и независимую сверку effect. Состояния acceptance,
effect и recovery остаются различимы при чтении evidence.

Доказательство обновления использует две точные различные template revisions:
исходную совместимую и candidate этого outcome. Между ними service-owned
feature и customization уже существуют и работают. Обновление по штатному
template-sync/adoption пути сохраняет business source/schema/data и
service-owned instructions/architecture. Если feature composition требует
ручного integration step вне portable ownership, этот шаг явно исполняется
и записывается; простое копирование новой версии с потерей customization
не считается успешным upgrade. Поведение R3 и recovery R2 подтверждается
после обновления, а marker/schema ownership остаётся service-owned.

Не дублировать existing template-sync canary ради ещё одного сохранения
`.service-owned`: новый oracle — работа и recovery настоящей функции после
template revision change. Результат содержит воспроизводимый carrier/fixture,
свои service changes, старый/новый template identity, upgrade steps и readbacks.
Создание или публикация отдельного remote repository не требуется.

## Совместное поведение и finality

Representative scenario: accepted чтение становится job/outbox intent;
receiver применяет marker+counter; ACK или process теряется; оператор
восстанавливает producer snapshot, отбрасывает старую команду, заново
инспектирует и сверяет stable identity с receiver. Same-identity replay
поглощается без второго счётчика даже после transport TTL. После template
upgrade feature даёт те же результаты. Если receiver недоступен, queue state
не служит разрешением нового remote action.

Пример local transaction не доказывает generic exactly-once HTTP effect:
внешний receiver обязан иметь собственный stable identity/reconciliation
contract на весь replay horizon. Успешная репетиция этого receiver не переносит
его гарантии на произвольного партнёра. Паника или release попытки также не
свидетельствует, что earlier remote/blocking work не произошло.

## Proof boundary и дальнейший владелец

Definition закрывает meaning, constraints и falsifiers; runtime proof здесь
не выполнялся. Technical Design должна закрыть R1 containment/custody mechanism,
R2 receiver/snapshot truth placement, R3 business ownership, R4 existing runner
integration и R5 upgrade/composition path. Конкретные тесты выбираются при
Implementation, без отдельной фазы test plan. Final validation следует
repository budget и обязательным real-database/CI owners; один matching build
переиспользуется, достаточное доказательство не запускается заново без причины.

Обязательное завершение: реализация всех пяти требований; реальные bounded
rehearsals/measurements; независимый review assembled candidate; точный commit
и PR/CI evidence в пределах ранее разрешённой публикации. Недоступный нужный
runner оставляет соответствующий result pending. Merge и production вне scope.
Малая source-confirmed правка stale webhook validation prose сохраняет нынешний
HeaderValue contract и не вводит новый фильтр.

Reopen: новый user outcome → Intake; изменение behavior/finality/replay horizon
→ Specification; спорный source факт → Research; недостаточный mechanism,
placement, runner или budget arithmetic → Technical Design. Нет оставшихся
user-owned вопросов, требующих технического выбора пользователем.
