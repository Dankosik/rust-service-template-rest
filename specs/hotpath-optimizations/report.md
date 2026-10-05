# Оптимизация передачи тела inbound webhook

Статус: код и измерения проверены; независимый [финальный review](review.md) — PASS.
Подтверждено сокращение выделений в выбранных scope на 15,8%, ускорение ответа
не заявляется. Этот отчёт основан на
сохранённым измерениям, а не по прогнозу из исходного
[отчёта о профилировании](../hotpath-profiling/report.md).

HTTP теперь передаёт уже собранный `Bytes` в `Receiver::receive_bytes`.
Проверка подписи заимствует исходные байты, а после выигрыша receipt тело
перемещается в `Incoming`. Дополнительный `body.to_vec()` на этом пути удалён.
Прежний `receive(&[u8])` сохранён и копирует тело только для нового receipt.
Обе точки входа используют один внутренний путь проверки и транзакции.

Формат JSON/Base64, сообщения и метаданные, выбор первого delivery,
атомарность receipt+job, ошибки, commit-unknown, лимиты и наблюдаемость
сохранены. Изменены два производственных файла и существующие integration
тесты. Эта оптимизация не меняет зависимости, SQL, настройки пула, allocator,
release-профиль или конфигурацию production. Исходные opt-in фичи hotpath и
несвязанные незакоммиченные изменения оставлены в рабочей копии.

## Где и как измеряли

| Параметр | Значение |
| --- | --- |
| DigitalOcean | `rust-service-template-rest`, ID `606044304`, тег `hotpath-profiling` |
| Размер / регион | `s-8vcpu-16gb-amd`, 8 AMD vCPU, 16 ГиБ, `fra1` |
| Цена и предел | $0,16667/час; разрешено максимум 8 часов / около $1,34 |
| Система | Ubuntu 24.04; всё исполнение, включая reduction измерений, на дроплете |
| Сервис | Rust 1.98.1; release, fat LTO, codegen-units=1, jemalloc 0.7.0 |
| База | PostgreSQL 18; `fsync=on`, `synchronous_commit=on`; pool=4 |
| Размещение | Сервис CPU 0–1, PostgreSQL 2–3, k6 4–7; наблюдатели CPU 3 |
| Инструменты | hotpath CLI/library 0.28.4, k6 2.3.0 |
| Окно | 5 секунд прогрева + 30 секунд постоянного открытого потока в каждом процессе |
| Данные | Синтетический JSON, тестовый HMAC-ключ; отдельная база очищается перед каждой ячейкой |

Нагрузка: новые небольшие и большие webhook — по 100 RPS; только повторы —
2000 RPS; смешанный поток — 800 RPS, из которых 70% новые webhook, 20%
повторы, 10% readiness. Повторы считаются успешными подтверждениями, а не
новыми durable заданиями. В смешанном потоке ожидается около 560 новых
заданий/с, в duplicate-only после первоначального seed — ноль новых заданий.
Тело большого webhook — **65 566 байт**, включая JSON-обёртку, SHA-256
`fdc1682dfcd0897e5a01ff2c0c0863df9af8ad418461ddd7e30021f78967f617`.
Номинальные `--body-bytes 65536` задают длину поля `data`, а не всего HTTP body.
Небольшое HTTP body содержит 1054 байта.

Обычные бинарники сравниваются отдельно от профилирующих. У первых трёх
серий порядок B/C, C/B, B/C; сценарии внутри версии идут small, large,
duplicate, mixed. Один ранний `baseline-small-r1` успел скопировать предыдущую
версию load.js при добавлении необязательной поддержки padded ID. До чтения
его результатов он исключён по несовпадению источника генератора, сохранён
с [объяснением](superseded-cell.json) и заменён `baseline-small-r1-matched`
после исходных серий. Этот повтор выполнен позже исходного candidate,
поэтому его нельзя обозначать исходным порядком B/C.

Три пары allocation-byte профилей и отдельная allocation-count пара используют
одинаковые `hotpath,hotpath-alloc,hotpath-mcp`. Живые метрики опрашиваются по
MCP через SSH-forward 6771; клиент исполняется на дроплете через обратный
forward 16771, чтобы локально не запускать Python или профилировщик.
Порт панели 6770 также проброшен. Фичи и jemalloc совпадают внутри каждой пары.

MCP округляет per-function байты. Поэтому для проверки полного body-length
добавлена отдельная пара с той же hotpath 0.28.4 и существующей cargo-фичей
`hotpath/hotpath-prometheus`: штатные точные счётчики
`hotpath_function_alloc_bytes_total`, `hotpath_function_alloc_count_total`,
`hotpath_function_calls_total` снимаются до/после основной нагрузки.
Экспортёр слушает `127.0.0.1:6772`; MCP остаётся включён. Никакого upload
нет. Эту возможность подтверждают
[официальная документация](https://hotpath.rs/) и исходники resolved crate
0.28.4. В точной паре ID дополнены до одинаковой длины, прогрев исключён
разностью счётчиков. Ни manifest, ни lockfile для этой пары не меняются.

## Поведенческая проверка

Свежие проверки кандидата на том же дроплете: `make build`, `make test`,
`ALLOW_HEAVY=1 REQUIRE_DOCKER=1 make test-integration-db`, `make fmt-check`,
обычная release-сборка и release с `hotpath,hotpath-alloc,hotpath-mcp` прошли.
Обычные тесты: **804 passed, 0 failed, 1 ignored**; PostgreSQL: **204 passed,
0 failed, 0 ignored**. Результаты — [validation.json](validation.json).
Ignored — существующий Go wire-export тест, для которого CI генерирует
`GO_WIRE_FIXTURES`; это не пройденная локальная проверка.

Изменённые реальные PostgreSQL webhook-тесты действительно исполнились:
полные независимые JSON-эталоны до нормализации PostgreSQL, бинарные body и
метаданные, Base64 padding, concurrent first-winner с разными телами,
пересечение borrowed/owned API, rollback при enqueue failure, обе стороны
потерянного commit acknowledgement и mounted HTTP admission. Для неопределённого
commit используется существующий TCP `CommitProxy`: управляемое пропускание
или потеря COMMIT/ответа. Длительная потеря связности хоста не моделировалась.
Логи находятся в `.artifacts/hotpath-optimizations/` и итоговом evidence-архиве.

CI, push, PR и deployment не выполнялись. Railway и боевые переменные не
менялись, production-данные и учётные данные на дроплет не переносились.

## Подтверждённая экономия выделений

В точной паре прогрев исключён; основной поток дал 3000 новых delivery у
baseline и 3001 у candidate. Разное число граничных итераций нормализовано
фактическим счётчиком вызовов, а padded ID обеспечивают одинаковую длину
метаданных каждой основной итерации.

| Синхронный scope | Baseline байт / вызов | Candidate байт / вызов | Выделения baseline → candidate |
| --- | ---: | ---: | ---: |
| `Incoming::new` | 65 614 | 48 | 4 → 3 |
| `enqueue::prepare` | 350 551 | 350 551 | 7 → 7 |
| Сумма двух scope | **416 165** | **350 599** | **11 → 10** |

Экономия — **65 566 байт и одно выделение на новую доставку**, около **15,8%**
в сумме этих scope. Это накопленный объём выделений за операцию, а не размер
живой кучи, stored payload или RSS. Ненулевые 48 байт конструктора — метаданные.
Точный числитель: 196 842 000 / 3000 против 144 048 / 3001 в конструкторе;
подготовка: 1 051 653 000 / 3000 против 1 052 003 551 / 3001.
Все исходные счётчики и вычисления — [comparison.json](comparison.json).

Приём HTTP в этой же точной паре выделяет одинаковые 66 232 байта / 9 allocations
в своём exclusive scope у обеих версий. Копия не перенесена в HTTP collector.
Внешние receiver/socket и enqueue scope имеют небольшие различия средних
примерно 42 и 33 байта; поэтому точную экономию 65 566 байт не распространяю
на весь запрос целиком. Три предыдущие пары MCP согласуются с результатом:
конструктор 64,1 КиБ → около 42 байт; подготовка около 342,3 КиБ неизменна;
отдельная count-пара подтверждает 4 → 3 allocations в конструкторе.

В трёх профилях p50 конструктора составил 23,58–28,82 мкс → 0,521–0,581 мкс,
p95 — 46,14–54,17 → 0,952–1,05 мкс. При этом профилируемый `prepare` стал
медленнее: p50 210,43–251,90 → 239,62–250,75 мкс, p95 357,89–380,16 →
389,12–401,15 мкс. Это сохранённое наблюдение, его причина не установлена;
исходник подготовки не менялся. Эти времена включают влияние allocation
профилировщика; нельзя складывать p95 компонентов или объявлять ускорение
обычной сборки по одному конструктору.

## Обычная release-сборка

В таблице — медиана трёх исходных серий; в скобках min–max между сериями.
Малый baseline включает описанный выше matched replacement. p50/p95 — HTTP
duration по k6, CPU — процесс сервиса из `/proc`, микросекунды на ответ.
CPU 100% означает одно ядро, сервису выделены два. В mixed HTTP latency
включает также readiness; отдельные route percentiles сохранены в JSON.

| Сценарий | p50 мс B → C | p95 мс B → C | CPU мкс/ответ B → C | Пиковый RSS МиБ B → C |
| --- | --- | --- | --- | --- |
| Новый небольшой, 100 RPS | 3,433 (3,389–3,468) → 3,292 (3,167–3,364) | 4,304 (4,243–4,360) → 4,117 (4,051–4,394) | 1006,7 (999,7–1029,7) → 990,0 (943,0–1003,0) | 26,438 (24,125–29,625) → 23,750 (23,141–26,191) |
| Новый большой, 100 RPS | 5,145 (5,117–5,391) → 5,280 (5,097–5,343) | 6,749 (6,470–7,255) → 6,719 (6,335–6,734) | 1556,7 (1519,5–1630,0) → 1540,0 (1486,2–1583,3) | 25,859 (25,352–26,352) → 25,008 (24,734–25,219) |
| Повторы, 2000 RPS | 1,088 (1,051–1,146) → 1,042 (1,020–1,152) | 1,603 (1,550–2,018) → 1,502 (1,427–1,823) | 382,5 (374,2–408,7) → 377,8 (370,7–410,5) | 24,070 (23,867–25,320) → 24,891 (24,082–25,117) |
| Mixed, 800 RPS, первые 3 пары | 2,036 (1,978–2,054) → 2,001 (1,883–2,088) | 3,233 (3,165–3,373) → 3,141 (2,904–3,315) | 506,6 (498,3–517,1) → 514,6 (481,2–521,2) | 23,625 (23,441–23,859) → 24,078 (23,914–24,246) |

Поток обе версии выдержали: новые webhook 100–100,033 RPS, повторы
2000–2000,067 RPS, mixed около 800,033 RPS. Во всех этих ячейках 0 failed
status checks, 0 dropped iterations. Генератор не насыщен: до около 119%
CPU при четырёх доступных ядрах; steal на CPU сервиса 0,049–0,459%.
Сборки и тесты с нагрузкой не пересекались.

Диапазоны задержек и CPU пересекаются. Подтверждённого устойчивого ускорения
ответа, CPU или увеличения предельной пропускной способности этот опыт не даёт.
Пиковый RSS нельзя приравнять к churn выделений: в mixed первоначально
наблюдался рост примерно 0,45 МиБ, и эта неблагоприятная серия сохранена.
Для неё отдельно выполнена заранее выбранная проверка воспроизводимости,
описанная в [operations.md](operations.md).

Дополнительные пары mixed выбраны до их результатов, порядок C/B, B/C, C/B,
те же бинарники, 800 RPS, pool=4, 5+30 секунд. Пики RSS B:
23,949 / 23,703 / 23,391 МиБ; C: 24,340 / 23,598 / 23,863 МиБ.
Во второй паре candidate ниже baseline. По всем шести парам диапазоны
23,391–23,949 и 23,598–24,340 МиБ пересекаются; медиана candidate всё ещё
примерно на 0,33 МиБ выше, но разброс 0,56–0,74 МиБ больше этого сдвига.
Первоначальное разделение пиков не воспроизвелось; рост сверх наблюдаемого
разброса не установлен. Это не доказательство отсутствия небольшого overhead
и не подтверждение снижения RSS. Все шесть пар доступны в JSON и архиве.

## Что остаётся и что предлагается дальше

Удалённая копия тела — подтверждённый результат этой реализации. Пропускная
способность, CPU и RSS не должны получать обещанный процент ускорения от
снижения allocator churn. Значительная стоимость `prepare` сохранилась:
350 551 байт / 7 allocations на большой payload в точном контроле.
Следующее возможное изменение — потоковая Base64-сериализация без временной
String; общий выигрыш пока гипотеза, поскольку рост выходного JSON Vec может
съесть экономию. Оно требует отдельной сопоставимой проверки и не внедрено.

Пул SQLx остаётся 4. Исходное профилирование показало большой выигрыш от 16
соединений при 2000 RPS, но общий бюджет соединений зависит от базы и числа
процессов. Глобальное повышение default, сокращение SQL round trips,
ослабление durability и отключение telemetry не входят в этот кандидат.
Полные цифры и порядок остальных узких мест — в исходном отчёте.

Production SLO, предельная ёмкость большого payload, долгий leak-тест,
реальные внешние провайдеры и новая CPU-stack атрибуция не измерялись.
Здесь выбран allocation-focused контроль; CPU измерен для всего процесса,
а CPU-профили прежнего исследования не считаются новым подтверждением.
MCP lock/SQL/Tokio ответы, Prometheus и `pg_stat_statements` сохранены как
диагностика; новые SQL/lock ускорения не заявляются. Wait-event sampling не
является измерением длительности блокировки. Production не изменялся,
реальная доля маршрутов Railway этими синтетическими потоками не доказывается.

## Фиксированные входы и повторение

Ветка: `codex/hotpath-optimizations-20261004`; push/PR не выполнялись.
HEAD `67be869acea112af271ec8ba621cbc50ae9d36b7` сам по себе не описывает
грязный baseline. Воспроизводимый вход — сохранённый исходный архив и delta.

| Файл | Baseline Git blob | Candidate Git blob |
| --- | --- | --- |
| `infra-webhooks/src/inbound.rs` | `d32c1e5865b8a4ba83a45ab31aa7501071819eb9` | `ad981d30e1590dcb6ea0b31842f633b288626965` |
| `infra-http/src/webhooks.rs` | `c4b581f1a378a0e6b464fb7503375dce3b17c638` | `64bce8377a68db1ecb97c637159c8afc466a8910` |
| `test/tests/webhooks/inbound.rs` | `9d88093fd91e6ae7e679ade8a048596fdd0e96e3` | `184aec41c05b7cd26ce9d1d22476b7549eb01143` |

Baseline archive SHA-256:
`03989134f24224b2ab95709881ee5ec5d89f7c8e3dabe06ad0b778bccf23d82f`.
`Cargo.lock` SHA-256 обеих версий:
`9c81010b0ed1de342ee6ace36518dce57549ee2cd5042d1da9bc811c19f0a873`.
Обычные бинарники SHA-256 B/C:
`8f332751555f1df28798ce55d1543eb375de06a95ac44e1f800f4c6e2ad1a09a` /
`0374ad55ff17738412ea1e5356e394d57878e432118939c531b43d6b30420dbc`.
Полный manifest, feature tree и входы каждого прогона находятся в архиве.

Перед новым платным ресурсом снова согласовать размер, регион, стоимость и
предел времени. На компьютере выполняются только doctl, ssh, rsync и Git.
Например, после согласования:

```bash
/opt/homebrew/bin/rtk proxy doctl compute droplet create rust-service-template-rest \
  --region fra1 --size s-8vcpu-16gb-amd --image ubuntu-24-04-x64 \
  --ssh-keys 57909910 --tag-name hotpath-profiling --wait --output json
# BENCH_IP и BENCH_ID взять из ответа, не выбирать по общему тегу.
/opt/homebrew/bin/rtk proxy ssh -i ~/.ssh/digitalocean-bench -o ForwardAgent=no root@BENCH_IP \
  'mkdir -p /root/optimization/baseline /root/optimization/evidence /root/profiling/scripts'
/opt/homebrew/bin/rtk proxy rsync -av \
  -e 'ssh -i ~/.ssh/digitalocean-bench -o ForwardAgent=no' \
  .artifacts/hotpath-optimizations/baseline-source.tar.gz \
  .artifacts/hotpath-optimizations/optimization.patch \
  root@BENCH_IP:/root/optimization/evidence/
/opt/homebrew/bin/rtk proxy rsync -av \
  -e 'ssh -i ~/.ssh/digitalocean-bench -o ForwardAgent=no' \
  specs/hotpath-optimizations/scripts/ root@BENCH_IP:/root/profiling/scripts/
/opt/homebrew/bin/rtk proxy rsync -av \
  -e 'ssh -i ~/.ssh/digitalocean-bench -o ForwardAgent=no' \
  specs/hotpath-profiling/scripts/bootstrap.sh root@BENCH_IP:/root/profiling/scripts/
```

Следующий блок выполняется **в SSH-сессии на дроплете**. Bootstrap ставит CLI
командой `cargo install hotpath --version '^0.28' --locked --features tui`,
проверяет 0.28.4 и при необходимости ставит именно её; агент init не запускается.

```bash
tar -xzf /root/optimization/evidence/baseline-source.tar.gz -C /root/optimization/baseline
bash /root/profiling/scripts/bootstrap.sh
bash /root/profiling/scripts/setup-baseline.sh
source /root/.cargo/env
cd /root/optimization/baseline
git apply /root/optimization/evidence/optimization.patch
mkdir -p /root/optimization/candidate-delta
cp --parents crates/infra-webhooks/src/inbound.rs crates/infra-http/src/webhooks.rs \
  test/tests/webhooks/inbound.rs /root/optimization/candidate-delta/
bash /root/profiling/scripts/validate-candidate.sh
```

Перед профилями держать следующую **локальную SSH-сессию** открытой:

```bash
/opt/homebrew/bin/rtk proxy ssh -N -i ~/.ssh/digitalocean-bench \
  -o IdentitiesOnly=yes -o ForwardAgent=no -o ExitOnForwardFailure=yes \
  -L127.0.0.1:6771:localhost:6771 -L127.0.0.1:6770:localhost:6770 \
  -R127.0.0.1:16771:127.0.0.1:6771 root@BENCH_IP
```

После этого **на дроплете**, без параллельных сборок/тестов/нагрузок:

```bash
bash /root/profiling/scripts/compare.sh
bash /root/profiling/scripts/finish-precision.sh
bash /root/profiling/scripts/mixed-rss.sh
python3 /root/profiling/scripts/reduce-validation.py
python3 /root/profiling/scripts/analyze.py
```

На новом дроплете scripts уже фиксированы: историческую excluded ячейку и её
replacement повторять не требуется. `compare.sh` даёт 24 ordinary + 6 byte
profile + 2 count profile ячейки; precision добавляет 2, RSS — 6.
В текущем исследовании сохранена 41 ячейка: 40 пригодных и одна excluded.

`run.py` задаёт тестовые app variables, DSN и HMAC; body/rate/mix/pool — в
`parameters.json` каждой ячейки. Существенные hotpath variables:
`HOTPATH_OUTPUT_FORMAT=json`, `HOTPATH_OUTPUT_PATH=<cell>/hotpath.json`,
`HOTPATH_FUNCTIONS_LIMIT=0`, `HOTPATH_THREADS_LIMIT=0`, `HOTPATH_SOURCE_ROOT=''`,
`HOTPATH_METRICS_PORT=6770`, `HOTPATH_MCP_PORT=6771`,
`HOTPATH_ALLOC_METRIC=bytes` (отдельные count ячейки — `count`),
`HOTPATH_PROMETHEUS_HOST=127.0.0.1`, `HOTPATH_PROMETHEUS_PORT=6772`.
CPU sampling в этих сборках не включено. MCP сохраняет живой и финальный
снимки; raw precision дополнительно сохраняет `hotpath-before/after.prom`.

Основной архив: `.artifacts/hotpath-optimizations/evidence.tar.gz`, SHA-256
`f50e41e5cd767696cdfb9616744e109d7ca39d0cab67acabcb50a9f03de61dd7`,
Git blob `4e6449882478746dd82cb9db8c0965ae1f2a1b7d`.
Он содержит полные входы/выходы 41 ячейки, MCP и точные Prometheus counter
снимки, `/proc` и PostgreSQL наблюдения, build/test логи, scripts, manifests,
baseline source archive и optimization.patch. Отдельные source archive и
patch лежат рядом для команд выше. Неудачные попытки link-check из-за
неполной удалённой копии/командной строки тоже сохранены; итоговый
`make docs-check MARKDOWN_FILES=<все Markdown этой задачи>` проверил 82 ссылки:
79 OK, 0 ошибок, 3 external excluded. Shellcheck всех shell scripts пройден.

После выгрузки evidence на рабочий компьютер удалить только BENCH_ID и
проверить API readback; публичный тег используется другими сессиями.

## Завершение

Независимый финальный review — PASS, замечаний нет. Все evidence выгружены;
локальный Git blob архива совпал с дроплетом. Финальная проверка 14 Markdown
файлов с review: 85 ссылок, 82 OK, 0 ошибок, 3 external excluded;
receipt `.artifacts/hotpath-optimizations/docs-check-final.log`.

**Дроплет 606044304 удалён.** Команда удаления принята в
`2026-10-04 16:07:17 UTC`; API readback в `16:08:42 UTC` (19:08:42 МСК)
вернул **404 / resource could not be found**. Первое немедленное чтение ещё
видело объект во время асинхронного удаления; подтверждением служит 404.
Receipts: `.artifacts/hotpath-optimizations/deletion-command.txt`,
`deletion-readback.txt`, `pre-deletion.json`. Локальный SSH tunnel закрыт.
Создан в `13:56:27 UTC`; до подтверждения удаления прошло около 2 ч 12 мин.
Арифметическая оценка по $0,16667/час — **около $0,37**, ниже согласованного
предела; это не платёжная квитанция DigitalOcean. Другие дроплеты по общему
тегу не удалялись. Завершение принято root в пределах реализации и
синтетических измерений; CI, push, PR и deployment остаются невыполненными.

```text
unit: Completion
verdict: Accepted
candidate: the three fixed source blobs and optimization-only patch recorded above
evidence: fresh build/tests, exact allocation target, matched controls, retained archive, API 404 cleanup
review: fresh independent Implementation Review PASS
next_owner: none for this outcome
```
