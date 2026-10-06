# Intent: завершить проверку надёжности jobs, webhooks и outbox

Status: ready. Definition baseline: `ac88395`; branch
`codex/jobs-reliability-followup-20261005`; 2026-10-06.

## Problem

Первый состав PR #240 исправил подготовку handler и уточнил границы доставки,
добавил real-PostgreSQL reclaim/TTL и позднее HTTP completion. Пользователь
принял следующие пять существенных улучшений; закрыть их одними пояснениями
или косметическими исправлениями недостаточно.

## Desired outcome

Замкнуть lifecycle попытки и наблюдение supervisor, практически проверить
crash/backup/restore/reconciliation, дать исполняемый пример атомарной business
deduplication, измерить восстановление под ограниченной нагрузкой и подтвердить
применимость шаблона на производном сервисе с настоящей безвредной функцией.

## Affected actors and systems

Авторы job/webhook/outbox handlers, оператор worker и восстановления,
потребитель событий, разработчик производного сервиса; существующие Rust
service/jobs-worker, PostgreSQL, JetStream и локальный webhook receiver.

## Scope and non-goals

Все пять работ входят в один принятый outcome. Использовать существующие engine,
transaction APIs, initializer/template-sync и local/CI runners. Первый состав
PR #240 и новые runtime custody/progress решения main являются базой.

Не создавать workflow platform, новую универсальную очередь, облачный стенд,
необходимость production rollout или новые quota/fairness/config механизмы без
доказанной текущей необходимости. Нет обещания exactly-once remote effect.

Допущение для reference feature: локальный счётчик прочитанных материалов по
стабильной identity операции. Он не работает с деньгами, пользовательскими
персональными данными или настоящими внешними получателями. Если такая функция
не проводит реальный путь acceptance → durable work → business effect → readback,
Definition должна заменить допущение до реализации.

## Constraints

Разрешены scoped local edits и реальные bounded proof, commit/push и создание
или обновление отдельного PR. Merge main, production changes, покупки и платные
ресурсы не разрешены. Работать в выделенном checkout и сохранять чужие изменения.
Не помещать учебные business-таблицы в runtime migrations шаблона.

Обязательны стабильная business identity и дедупликация/reconciliation на весь
разрешённый replay horizon. Lease, fencing или completed row сами по себе не
доказывают единственность remote action. Восстановленный snapshot не обязан
сделать ранее сохранённую recovery command технически недействительной.

## Success signal

Каждой из пяти работ соответствует проверяемое изменение или исполняемое
доказательство; итог содержит точный candidate, результаты локальных/CI gates,
recovery/resource measurements и ограничения. Производный сервис сохраняет
функцию, свои данные и customization при обновлении template revision и
восстановлении. Отсутствующее обязательное доказательство остаётся pending,
а не превращается в «готово».
