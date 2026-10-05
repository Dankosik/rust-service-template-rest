# Оставшиеся оптимизации по отчёту hotpath

Статус: завершено. Независимое [повторное ревью](review.md) — PASS,
evidence сохранены, дроплет удалён. Отчёт закрывает четыре области
исходного [профилирования](../hotpath-profiling/report.md), включая случаи,
когда безопасная оптимизация не установлена. Предыдущая передача тела без
копии сохранена и является новым baseline.

## Реализация и проверка

Кандидат меняет приватную сериализацию трёх Base64 полей `Incoming` через
существующие `Base64Display`, `Serializer::collect_str` и отдельные Serde
annotations для сохранённого decoder. Поля, порядок, Base64 alphabet/padding,
None/empty, версия и переданные потребителю байты сохраняются. Generic jobs
подготовка/ошибки и duplicate bypass не меняются.

HTTP-наблюдаемость использует borrowed значения стандартных методов и
протоколов и статический
массив допустимых `StatusCode` для числовых labels. Произвольные допустимые
extension методы сохраняют исходный текст в span и прежний `_OTHER` в
метрике. Все коды 100..999 сохраняют точное числовое значение. Нового cache,
mutex, регистратора или политики telemetry нет.

Отдельно проверенный display-вариант имени span **отклонён и удалён**:
несмотря на экономию одной allocation, все шесть первых HTTP пар показали
больший peak RSS. Финальная сборка возвращает прежнее eager `format!` имени,
сохраняя только method/protocol/status изменения.

Свежие проверки на дроплете: default/integration/profile compile diagnostics,
`make fmt-check`, `make build`, `make test`,
`ALLOW_HEAVY=1 REQUIRE_DOCKER=1 make test-integration-db`, ordinary release и
release с hotpath прошли. **806 passed / 0 failed / 1 ignored** в обычных
тестах; **204 passed / 0 failed / 0 ignored** с PostgreSQL.
Ignored — существующий CI-owned Go wire-fixtures тест, не локальный pass.
После возврата прежнего имени повторно прошли matching `make fmt-check`,
`make build` и **81 HTTP тест / 0 failed / 0 ignored**. Неизменённые T1 и
database proof используются повторно в рамках ограниченного исправления.
[Validation](validation.json) содержит итог. Изменённые проверки действительно
исполнились: точные JSON/Base64 literals для binary/пустых/паддинговых и больших
полей, exported-span methods/protocols, все 900 числовых status labels,
first-winner/rollback и обе управляемые потери COMMIT acknowledgement.

Дополнительно одинаковые реальные запросы к ordinary baseline/candidate
дали одинаковые JSON HTTP logs и response status/body/request-id после
исключения времени и случайного span ID. Четыре случая: missing, query,
PROPFIND и inert webhook. Источник — реальный настроенный JsonLayer, а не
test-only замена; transcript `.artifacts/hotpath-remaining-optimizations/signal-parity.json`.
Та же проверка финальной исправленной сборки прошла повторно:
`signal-parity-repaired.json` в той же директории.

## Стенд и сравнения

DigitalOcean `rust-service-template-rest`, ID **606085569**, tag
`hotpath-profiling`; `s-8vcpu-16gb-amd`, 8 AMD vCPU /16 ГиБ, `fra1`, Ubuntu24.04,
$0,16667/час, согласованный максимум8часов/$1,34. Создан
4 октября2026 в20:57:04 МСК, лимит —5 октября04:57:04 МСК.
Rust1.98.1, hotpath CLI/library0.28.4, k6 2.3.0. Jemalloc, fatLTO и
release-профиль прежние. PostgreSQL18 с fsync/synchronous_commit on;
никакой production/Railway конфигурации не менялось.

Сервис CPU0–1, PostgreSQL2–3, k6 4–7, наблюдатели CPU3. Нагрузка постоянная
открытая, 5секунд прогрева +30секунд измерения, по три серии с чередованием
порядка. Синтетические body/HMAC; БД очищается перед каждым процессом.
Генератор и тестовые секреты зафиксированы в сценариях; paid провайдеров нет.
Всё исполнение и reduction только на этом дроплете, локально только
чтение/редактирование, Git и doctl/ssh/rsync.

Ordinary: новые small/large по100RPS, duplicate2000RPS,
webhook mixture800RPS (70%new,20%duplicate,10%readiness), SQL-free HTTP mixture
8000RPS (40%live,40%ready,10%missing,10%inert webhook). Pool=4 при сравнениях
кода. Sizing: одинаковый binary, mixed2000RPS при4/8/16 connections,
три серии4/8/16,16/8/4,8/4/16. Настройка и изменение исходников не смешиваются.

Matched hotpath profiles: large100RPS и SQL-free HTTP2000RPS, с одинаковыми
features `hotpath,hotpath-alloc,hotpath-mcp,hotpath/hotpath-prometheus`.
MCP6771/панель6770 слушают loopback и проброшены SSH; клиент выполняется на
дроплете через reverse16771. Штатные exact counters на localhost6772 снимаются
до/после основной нагрузки; CLI округления не используются для точного выигрыша.
Для большого payload fixed-width ID исключает изменение длины metadata.
Body содержит65566байт (65536 в data плюс JSON оболочка).

Отдельный profile-noname оставляет method/protocol/status/payload изменения,
но возвращает только прежнее eager name formatting. Три HTTP профиля этого
варианта нужны для собственного результата name formatting: общий выигрыш
span не может подменять его независимый вклад. Variant source/binary manifests
сохранены; исходник восстановлен перед runtime. Сборки/тесты с нагрузкой не
пересекались.

## SQL: явный результат без изменения production кода

В warmed baseline на pool1 loopback-прокси сохранил порядок frontend commands,
Execute/Sync и backend ReadyForQuery, без паролей, bind values, rows или body.
Это наблюдение протокола, его latency включает proxy и не выдаётся за скорость
прямого подключения.

| Случай | Обмены в транзакции | Возврат pool | Всего |
| --- | --- | --- | --- |
| Новый, wake due | BEGIN, receipt INSERT, job INSERT, NOTIFY, COMMIT:5 | Empty Sync/ReadyForQuery:1 | 6 |
| Новый внутри25мс debounce | BEGIN, receipt INSERT, job INSERT, COMMIT:4 | 1 | 5 |
| Authenticated duplicate | BEGIN, receipt INSERT, COMMIT:3 | 1 | 4 |

Startup/statement prepares отделены. Empty return Sync может попасть в HTTP
phase следующего запроса; счётчики связываются порядком команд и connection ID,
не phase label. Raw `events.json`, `cases.json`, reduction/summary сохранены;
читаемая копия `.artifacts/hotpath-remaining-optimizations/protocol-summary.json`.

Безопасное сокращение этих обменов при выбранных неизменных правилах пока не
установлено. Receipt+job CTE требует сериализовать даже duplicate до решения
арбитража. Ранняя wake reservation меняет debounce при failed/duplicate insert.
Gate через await INSERT задерживает уже готовое eligible задание за зависшим
insert и может создать цикл с unique-key row lock. NOTIFY+COMMIT raw batching
обходит bookkeeping/commit-unknown SQLx transaction owner. After-release hook
не отменяет последующий driver ping. Это конкретные отклонённые механизмы,
подробные schedules и supported API evidence — в
[Technical Design](design.md#sql-alternatives-and-evidence-closure).

SQL/driver/wake исходники оставлены baseline; SQL speedup не заявляется.
Открыть область снова при новом поддержанном driver capability или изменении
accepted политики, а не обходить atomicity/uncertainty для меньшего числа RPC.

## Точные allocation профили кандидата

Три пары large-профилей после исключения прогрева дают одинаковые значения
на новую доставку. Метаданные fixed-width одинаковы; denominator — реальные
вызовы из штатных counters. Это allocated turnover, не live heap или RSS.

| Scope | Baseline байт/вызов | Candidate байт/вызов | Allocations B → C |
| --- | ---: | ---: | ---: |
| Incoming construction | 48 | 48 | 3 → 3 |
| Generic enqueue preparation | 350551 | 296439 | 7 → 10 |
| Сумма двух непересекающихся scope | 350599 | 296487 | 10 → 13 |

Сериализация экономит **54112байт/доставку, около15,4%** в подготовке.
Удалены временные encoded Strings, но выходной Vec растёт иначе: число
allocation/reallocation увеличилось на3. Это реальная цена кандидата,
сохраняемая вместе с CPU/RSS контролями. Ранее устранённая копия тела не
возвращена: construction остаётся48байт, HTTP collector в точной паре также
сохраняет прежние66232байта/9allocations.

В SQL-free HTTP mixture baseline make_span — около2579,1байт и23,6allocations
на запрос, полный candidate —2556,7байт и20,6. Metric record — около410,6 →
386,6байт и5,017 →4,017allocations. Небольшие дробные значения обусловлены
смесью маршрутов/редкими recorder событиями, исходные числители сохранены.

Независимый name-only контроль profile-noname даёт make_span около2573байт
и21,6allocations. Поэтому новый способ имени имеет собственный вклад около
**16,3байт и1allocation**, method/protocol borrowed значения — оставшиеся
около6,1байт и2allocations. Общая экономия span около22,4байт/3allocations;
числовой status label метрики экономит24байта/1allocation. Эти значения не
являются процентом CPU всего HTTP пути; для него используются ordinary runs.

Display-имя не входит в финальный код. Для принятой span части подтверждено
около **6,1 байта / 2 allocations** экономии на запрос, для status label —
**24 байта / 1 allocation**. Финальная оценка обычной сборки приведена ниже.

## Ordinary release: первый кандидат и сохранённые неудачные результаты

В каждой строке медиана и min–max отдельных серий, не percentile объединённых
сэмплов. Small/HTTP включают3исходные+3заранее выбранные RSS follow-up пары;
остальные строки —3исходные пары. CPU denominator — ожидаемые полезные
HTTP ответы: failed status checks0 в этих сравнениях; fast failures не входят
в обещанный успех. `comparison.json` сохраняет все исходные observations,
route-specific percentiles, количество ошибок/drops, порядок и параметры.

| Workload | p50мс B→C | p95мс B→C | CPUмкс/ответ B→C | Peak RSS МиБ B→C |
| --- | --- | --- | --- | --- |
| Small100RPS,6пар | 3,230(3,120–3,385)→3,106(3,042–3,150) | 4,217(4,075–6,125)→4,038(4,002–4,099) | 948,2(916,7–1073,0)→906,7(893,0–926,7) | 25,438(23,969–29,750)→27,371(23,406–29,750) |
| Large100RPS,3пары | 5,211(5,134–5,246)→5,149(5,124–5,168) | 7,197(7,121–7,277)→7,275(7,274–7,283) | 1462,8(1459,5–1516,2)→1436,2(1423,3–1509,5) | 25,570(25,273–25,652)→25,691(25,094–26,180) |
| Duplicate2000RPS,3пары | 0,930(0,892–0,934)→0,940(0,929–0,953) | 1,350(1,246–1,416)→1,351(1,313–1,372) | 348,5(333,2–351,3)→349,0(343,7–353,5) | 24,391(24,172–24,555)→24,527(24,145–24,629) |
| Webhook mix800RPS,3пары | 1,812(1,720–1,858)→1,767(1,712–1,799) | 2,981(2,893–3,179)→2,890(2,810–3,239) | 450,0(441,2–469,6)→443,3(433,7–450,8) | 23,965(23,773–24,227)→24,125(23,516–24,355) |
| SQL-free HTTP8000RPS,6пар | 0,219(0,202–0,234)→0,211(0,203–0,222) | 12,447(9,537–14,325)→11,528(9,556–12,498) | 85,4(81,8–87,8)→83,5(82,7–85,7) | 20,666(20,215–20,938)→21,174(20,727–21,230) |

Снижения некоторых медиан недостаточно для обещания устойчивого CPU/latency
ускорения: соответствующие диапазоны пересекаются. Большой payload p95
немного выше при уменьшении allocated bytes; исходные значения сохранены.
Полезный поток: new100–100,033RPS, duplicate около2000,033RPS, mix около800;
HTTP achieved7992,97–7997,50RPS B и7994,83–8000,03RPS C. В HTTP-контролях
неожиданных статусов/transport errors нет, но generator dropped iterations
составили862 B против580 C по6сериям; ни одна из них не исключена.
Генератор занимает273–293% из400% на HTTP8k. Максимальный service-steal
в follow-up small B —0,72%, этот неблагоприятный прогон тоже сохранён.

В первоначальных3small сериях peak RSS B23,969–25,375МиБ и
C26,625–29,375МиБ не пересекались. Root до результатов выбрал ровно3новые
пары small+HTTP, порядокC/B,B/C,C/B, те же5+30s/бинарники/параметры,
никаких повторов других сценариев. Follow-up small B27,625/29,750/25,500МиБ,
C28,000/23,406/29,750МиБ показывает обратную вторую пару и пересечение
диапазонов. End RSS уже исходных серий пересекался: доказательств retained
growth/leak нет. Initial peak separation остаётся наблюдением; по6сериям
рост small сверх большого observed spread не установлен, но positive median
small+1,93МиБ остаётся в отчёте. Для HTTP это объяснение непригодно:
**все шесть пар C>B**, а отдельный follow-up сам имеет непересекающиеся
диапазоны B20,664–20,938 и C21,023–21,230МиБ. Первое независимое ревью
вернуло FAIL по HTTP adoption; рост не замаскирован объединением диапазонов.

### Исправление и прямой контроль финальной сборки

В трёх отдельных парах полный кандидат против варианта с прежним eager
именем последний имел меньший peak RSS каждый раз: 20,957/21,105/21,000 →
20,871/20,996/20,633МиБ. Это поддерживает удаление display-имени, но не
доказывает внутреннюю причину поведения jemalloc/subscriber.

После удаления проведены три новые прямые baseline/final пары HTTP8000RPS
с прежними настройками и чередованием порядка. `noname` в evidence — имя
**финального** ordinary binary, а не отключённая наблюдаемость.

| Показатель | Baseline: медиана (min–max) | Финальный код: медиана (min–max) |
| --- | --- | --- |
| p50, мс | 0,230 (0,224–0,236) | 0,211 (0,208–0,222) |
| p95, мс | 14,804 (13,472–15,665) | 12,668 (12,496–13,965) |
| CPU, мкс/полезный ответ | 86,64 (84,05–87,09) | 81,90 (81,26–83,28) |
| Peak RSS, МиБ | 20,820 (20,711–20,852) | 20,395 (20,277–21,121) |
| Полезные ответы/с | 7983,6 (7980,0–7995,6) | 7997,1 (7994,9–7997,7) |
| Dropped iterations, сумма | 1287 | 308 |
| Неожиданные статусы/transport errors | 0 | 0 |

Для peak RSS диапазоны пересекаются и третья пара имеет final>B; прежний
повторяемый односторонний рост не воспроизведён. Неблагоприятный final peak
21,121МиБ сохранён. CPU медиана ниже примерно на5,5%, но это короткий
локальный эксперимент: различались steal и давление генератора, поэтому
процент не обещается production. Малый истинный overhead, длительный рост
heap и другой subscriber этим опытом не исключаются. Unchanged payload
controls выше сохраняются как доказательства T1, а не переименовываются в
новые final runs. Все **87** ячеек сохранены, исключённых нет.

## Подбор пула: измеренная настройка синтетического deployment

Readback PostgreSQL: max_connections100, reserved_connections0,
superuser_reserved_connections3. Несмотря на fixture superuser app, бюджет
консервативно не использует эти3слота. Другие рабочие нагрузки отсутствуют;
для observer/разового admin оставлено2слота, дополнительный operational
reserve10. Доступный application budget: **100−3−2−10=85**.
Стенд: один service pool, processing workers/LISTEN отсутствуют, rollout
overlap отсутствует. Дополнительно резервируется1 dedicated migrator slot:
`16+1<=85` (для выбранного ниже8: `8+1<=85`). Во время измерения migration/build/tests не работают; readiness
использует общий pool. Это бюджет максимумов, не сумма текущих idle slots.

Повторный sizing выполнен на одном **финальном `noname` binary**, отдельно
от сравнения кода. Числа ниже относятся только к этим девяти ячейкам.

| Pool | p50 мс: диапазон3серий | p95 мс | Ответы/с | Mean acquire wait мс | Acquire-adjusted tx mean мс | PostgreSQL CPU% |
| ---: | --- | --- | --- | --- | --- | --- |
| 4 | 2,078–2,140 | 77,69–106,70 | 1995,7–2000,1 | 10,128–14,944 | 1,613–1,634 | 88,76–89,24 |
| 8 | 1,931–1,966 | 3,896–4,174 | 2000,03 | 0,379–0,575 | 1,771–1,797 | 92,23–92,95 |
| 16 | 1,880–1,925 | 3,501–4,081 | 2000,03 | 0,170–0,572 | 1,797–1,882 | 93,84–94,51 |

Полезные ответы имеют ожидаемые200/204, failed checks0. Pool4 дал
0/131/72 dropped iterations; pool8/16 —0 во всех сериях. Минимальный
воспроизводимо полезный measured choice для финального стенда — **8**:
медиана p95 pool4=101,670мс → pool8=4,070мс, примерно25раз. Дополнительные
8 соединений дают лишь p95 median3,760мс с пересечением диапазонов8/16.
Это эффект конфигурации, не выигрыш Rust delta.

Service CPU pool4/8/16 соответственно380,1–383,8 /373,3–385,0 /
369,0–375,3мкс на полезный ответ; peak RSS30,68–32,45 /25,54–26,44 /
25,78–26,54МиБ. PostgreSQL CPU100% — одно ядро, БД выделено два.
Cgroup memory включает page cache:1166–1387МиБ, не RSS процесса PostgreSQL.

Предыдущие девять sizing ячеек на отклонённом display-name кандидате также
сохранены: pool4 p95135,05–148,59мс/wait43,84–47,90мс, pool8
14,29–77,02мс/1,815–9,143мс, pool16 5,36–13,78мс/0,256–2,623мс;
drops4=689/859/841,8=0/0/0,16=1/0/0. На том наборе выбор был16.
Сильная межсерийная изменчивость не доказана эффектом удаления имени span:
нельзя приписывать её исходникам или заменять эти результаты новым набором.
Выбор8 ограничен финальным бинарником, текущим стендом и коротким сценарием;
при другом storage/topology или production он требует собственного измерения.

Точность wait/transaction: разности Prometheus sum/count исключают прогрев,
в каждой ячейке population acquire и committed transaction совпадает.
Разность их средних даёт acquire-adjusted transaction interval. Это не весь
lease/occupancy: asynchronous return Sync идёт после commit. В bounded proxy
trace его отдельное завершение также сохранено. Percentiles не вычитались,
вложенные SQL/spans не складывались. SQL/transaction wall time не назван CPU.

Конкретная не-secret настройка — `synthetic-pool.env` рядом с отчётом;
существующий APP override не создаёт нового config owner. Для production
подставить реальные бюджеты: `A=M−Rdb−other_workload_maxima−reserve`; в каждой
допустимой конфигурации steady/rollout require
`sum(replicas_i*pool_max_i)+dedicated_maxima<=A`. Не считать reserve дважды.
Учитывать одновременно старые/новые replicas, workers и отдельный LISTEN,
migrator и другие процессы. Workers требуют минимум N+2, combined
jobs+outbox N+5, outbox-only3 по текущей архитектуре. Engine listeners
разделяются, readiness отдельного соединения не требует. Неизвестные
production inputs остаются неизвестными; Railway не менялся. Global default4
и диапазон1..500 сохранены.

## Что подтверждено и что осталось ограничением

По убыванию измеренного влияния: насыщение малого пула устраняется настройкой
8 на этом стенде; сериализация уменьшает allocated turnover большого webhook
на54112байт; HTTP method/protocol/status устраняют небольшие постоянные
allocation. Устойчивое ускорение большого webhook по latency/CPU не доказано.
SQL обмены измерены, безопасный supported способ их сократить при нынешних
контрактах не найден. Display-name отклонён по RSS, его экономия не включена
в результат внедрения.

MCP timing/SQL — async wall time, не CPU; точный CPU получен из `/proc` для
ordinary binary. Профилирование native sampled CPU в этой серии не включено.
В HTTP MCP mutex/rwlock datasets пусты (total_count0); они показывают только
инструментированные операции и не исключают внутренние locks библиотек.
Отдельное ожидание row locks/WAL и
полный lease occupancy не изолированы; acquire-adjusted mean и return ping
выше имеют явно ограниченную область. Нет production p95/SLO, многорепличного
rollout, worker/outbox нагрузки, long-duration memory proof или внешних
провайдеров. CI и существующий Go ignore остаются CI-owned; push/PR не делались.

## Идентичность и повторение

Исходный baseline — полный архив dirty рабочей копии **до** этих изменений,
включая предыдущую body-transfer оптимизацию. Его SHA256:
`aae64f3624831b7ef5a60491810d812a3034511f346b7811de2b88d6b0c0e2e7`.
HEAD исходного репозитория сам по себе не воспроизводит baseline.
Финальные Git blobs:

- inbound `f1e3cc4769d97b56be6bea1855a305a2a0a1c21e`;
- observe `002a082fdf8ee704905926c244418d2e27ff7b0c`;
- integration test `37ed0d081960012dde17cf97751f42fd292f6e1d`.

Ordinary baseline SHA256 `2f9c608afb17692b9a14b805ef3ac28ecec57e5ee6da3265087cdf2d89e754af`;
final `noname` `1199a37c8ed56b74a9a2b7e33bb9d805a401977a7a1b37de66efac1b628ae049`.
Profile baseline `d9f9352e555221c394a36ea3e91737dfb9f2f34685c07dc184da0617c77e4d79`;
final `profile-noname` `cd829babc685667cf50d3925d6f6c037f86fc465e4dfe7995a25e72bd5dc38b7`.
Rejected binary/patch и все raw данные также сохраняются в evidence archive;
`candidate` в исторических ячейках обозначает именно отклонённую сборку.

Все следующие команды выполнять **только на новом согласованном дроплете**.
Подготовить Ubuntu24.04, Rust1.98.1, Docker, PostgreSQL fixture и k6 через
[setup-baseline.sh](scripts/setup-baseline.sh). Перед этим распаковать
`remaining/evidence/baseline-source.tar.gz` в `/root/remaining/baseline`,
скопировать task scripts в `/root/profiling/scripts`, выполнить сохранённый
[bootstrap.sh](../hotpath-profiling/scripts/bootstrap.sh) на дроплете.
CLI устанавливается указанной пользователем
командой и проверить точную версию: range в будущем может выбрать иной patch.

```bash
cargo install hotpath --version '^0.28' --locked --features tui
hotpath --version # должен быть 0.28.4; иначе установить --version '=0.28.4'
bash /root/profiling/scripts/setup-baseline.sh
cd /root/remaining/baseline
git apply /root/remaining/evidence/optimization-final.patch
bash /root/profiling/scripts/validate-candidate.sh
bash /root/profiling/scripts/repeat-final.sh
python3 /root/profiling/scripts/summarize.py
python3 /root/profiling/scripts/reduce-alloc.py
python3 /root/profiling/scripts/reduce-db.py
python3 /root/profiling/scripts/analyze.py
```

SSH с компьютера, отдельно перед profile runs:

```bash
ssh -N -i ~/.ssh/digitalocean-bench -o IdentitiesOnly=yes -o ForwardAgent=no \
  -o ExitOnForwardFailure=yes \
  -L 127.0.0.1:6771:localhost:6771 -L 127.0.0.1:6770:localhost:6770 \
  -R 127.0.0.1:16771:127.0.0.1:6771 root@NEW_DROPLET_IP
```

[run.py](scripts/run.py) фиксирует полный environment каждого сценария:
pool4 для кода, fake HMAC и DSN, JSON logging/info, health logs false,
OTel parentbased_traceidratio, CPU placement и MCP requests. Основные hotpath
переменные: `HOTPATH_OUTPUT_FORMAT=json`, `HOTPATH_OUTPUT_PATH=<cell>/hotpath.json`,
`HOTPATH_FUNCTIONS_LIMIT=0`, `HOTPATH_THREADS_LIMIT=0`, `HOTPATH_ALLOC_METRIC=bytes`,
`HOTPATH_METRICS_PORT=6770`, `HOTPATH_MCP_PORT=6771`,
`HOTPATH_PROMETHEUS_HOST=127.0.0.1`, `HOTPATH_PROMETHEUS_PORT=6772`.
Features профиля перечислены выше; обычный binary собирается без них.
Исторический campaign с display-name не запускать на финальном коде для
воспроизведения отклонённого механизма: для него сохранён отдельный старый patch.

## Сохранение и очистка

Архив `.artifacts/hotpath-remaining-optimizations/evidence.tar.gz` (259МиБ)
содержит87raw ячеек, baseline source archive, оба patch, binary/source manifests,
все измеренные binaries, runtime/config/protocol/MCP/build/test receipts и
runner/reduction scripts. SHA256 посчитан на дроплете:
`2e3170a3f2a07267fbf4cf1fa6632e781e297d4c5c1cbc59a0baa979753630c0`.
После rsync локальный Git blob совпал с удалённым:
`84e5cad2a72fb47af027f7b0a8c9fe84333962ac`; отдельный baseline archive также
совпал (`ed936fab60ab4c6d2f16c928691e26f4ccea8e12`). Custody завершена
4 октября2026 около21:00UTC /5 октября00:00МСК. Scoped docs-check и shellcheck
прошли на дроплете. Raw результаты отклонённого кандидата не перезаписаны.

**Дроплет 606085569 удалён** командой `doctl compute droplet delete 606085569 --force`.
Удаление запрошено
4 октября21:01:48UTC; сразу readback ещё возвращал active. После ожидания
повторный запрос получил **API404**, подтверждён4 октября21:02:22UTC /
5 октября00:02:22МСК. Receipt — `droplet-delete-readback.txt` рядом с отчётом;
pre-delete metadata и первый промежуточный readback также сохранены.
SSH tunnel закрыт. Ни shared tag, ни чужие ресурсы не использовались.

От создания до подтверждения прошло3ч05м18с, в пределах8часов. Ориентир
по согласованной пропорциональной ставке — около$0,52, не billing invoice.
Последний scoped docs-check:119total/70unique,104OK/0errors/15offline-excluded;
cleanup обновил только фактическую запись операции без новых ссылок или кода.
Production/Railway, remote Git, push и PR не менялись.
