# Обновление экземпляров public и internal

Это инструкция владельца для двух **экземпляров сервера Plombir Git** на одном
хосте. Репозитории пользователей внутри них сохраняются вместе с `data`.
Инструкция относится к уже настроенной схеме ниже, а не к произвольной установке
из deploy bundle. Общая документация: [Deployment Guide](README.md).

| Параметр | public | internal |
|---|---|---|
| Каталог экземпляра | `/disk2/plombir/git/public` | `/disk2/plombir/git/internal` |
| Compose project и container name | `plombir-public` | `plombir-internal` |
| Локальный образ | `plombir-public:local` | `plombir-internal:local` |
| UID/GID процесса и владельца данных | `20001:20001` | `20002:20002` |
| HTTP для локальных проверок | `http://127.0.0.1:30443` | `http://127.0.0.1:10443` |
| Резервные копии | `/disk2/plombir/backups/public` | `/disk2/plombir/backups/internal` |

Скрипты хоста находятся в `/disk2/plombir/bin/`: `common.sh`, `start.sh`,
`stop.sh`, `backup.sh`. Они устанавливаются владельцем отдельно и **не входят**
в этот репозиторий. `common.sh` задаёт `select_instance` и `dc`; start/stop/backup
принимают имя одного экземпляра. Перед применением убедиться, что их поведение
совпадает с описанным здесь. Установленный `plombir-public` обслуживает только
public; аналога с делегированными правами для internal сейчас нет.

## 1. Исходное состояние и опасный старый путь

После обновлений 11 ноября 2026 года оба экземпляра проверены на:

```text
version: 0.4.2
source_commit: af6ef3f68bd2541b9d79f96dd5fba287e89d1fba
```

Это зафиксированный результат того выпуска, а не обозначение последней версии.
Для следующего обновления выбрать новый опубликованный тег и его полный SHA.

**Не запускать старый `/disk2/plombir/bin/update.sh` ни для public, ни для
internal, ни без аргументов.** На момент составления инструкции `common.sh`
всё ещё содержит `v0.4.1` / `0342827a064022fb10a5652f822d0a3c7bd5602e`.
Старый update-скрипт не проверяет направление обновления и способен собрать и
запустить 0.4.1, вернув исправленные в 0.4.2 уязвимости. Два аргумента тегов у
него выбирают **оба** экземпляра — это не синтаксис «экземпляр + версия».

Ближайшая отдельная операция владельца под root: сохранить копию `common.sh`,
исправить обе строки пина на уже установленный выпуск и проверить синтаксис:

```text
RELEASE_TAG=v0.4.2
RELEASE_COMMIT=af6ef3f68bd2541b9d79f96dd5fba287e89d1fba
```

```bash
/bin/bash -n /disk2/plombir/bin/common.sh
```

Сохранить владельца root, права и метку SELinux файла. Новый пин устраняет
конкретную ловушку 0.4.1, но не добавляет старому `update.sh` проверку запрета
отката. Для следующих обновлений использовать процедуры ниже. Сам документ
никаких файлов на сервере не изменяет.

## 2. Что подготовить перед каждым выпуском

1. Выбрать **опубликованный release tag и полный commit SHA**, проверить их
   соответствие и успешный Release workflow. Прочитать заметки нового выпуска,
   требования к миграциям и ключам шифрования. Для 0.4.2 это
   [release notes](../docs/releases/v0.4.2.md); относительно 0.4.1 новых миграций
   нет. Этот факт не переносится на будущие версии.
2. Проверить текущие `/readyz`, `/health` и `/api/v1/instance` каждого экземпляра.
   Версию брать из **`/health.version`**: в `instance` её нет, а старый вывод
   helper status показывает `version: null` / `app_version: null`.
3. Работать последовательно: закончить public и его проверки, затем internal.
   Не выполнять параллельно другие сборки, обновления, restart или backup на
   этом хосте. У public и internal разные lock-файлы, общей блокировки нет.
4. Для действий под root использовать tmux/screen. Проверить `df -h /disk2`
   и `free -h`: сборка идёт на том же хосте, что и живые сервисы. Оставить запас
   под новый образ **и полную холодную копию** данных. Не чистить старые образы
   и копии ради продолжения уже начатого обновления.
5. Убедиться, что есть проверенная процедура восстановления. Копия должна
   включать БД, репозитории, `.env`, TOML, Compose и ключ шифрования. В этой
   схеме сохраняется весь `data` вместе с конфигурацией. Если ключ или иное
   состояние вынесены за эти пути, отдельно включить их в резервирование.

Не менять регистрацию, не создавать тестовые аккаунты и не запускать сценарии
security recheck на рабочих экземплярах. Для таких тестов нужен одноразовый
экземпляр с отдельными данными.

## 3. Public: установленный ограниченный helper

Этот путь доступен deploy-пользователю через уже установленный wrapper. Он не
даёт доступа к internal и не требует расширять sudoers. Подставить **оба**
значения выбранного выпуска; ниже приведён проверенный пример 0.4.2:

```bash
TARGET_TAG=v0.4.2
TARGET_COMMIT=af6ef3f68bd2541b9d79f96dd5fba287e89d1fba
HTTP_PORT=30443

/usr/local/bin/plombir-public status
/usr/local/bin/plombir-public update "$TARGET_TAG" "$TARGET_COMMIT"
/usr/local/bin/plombir-public status
curl --noproxy '*' -fsS http://127.0.0.1:30443/health
```

Helper сверяет тег с SHA, запрещает переход на коммит, не являющийся потомком
текущего, сохраняет прежний образ, собирает новый, делает холодную копию и
запускает миграцию. Проверка прав TOML выполняется и до сборки, и перед backup.
Путь recovery record печатается в выводе; сохранять его вместе с журналом.

В проверенной ревизии helper конфиг public принадлежит `20001:20001`, имеет
режим `0400` или `0600`, один hard link и read-only bind mount. Старую ревизию,
требовавшую владельца root для TOML, применять нельзя.

После команды выполнить все финальные проверки из раздела 6. Сообщение
`Already running requested commit` само по себе **не подтверждает** успешное
завершение предыдущей попытки: этот путь не повторяет все финальные проверки.

Не прерывать helper Ctrl-C. Его обработчик обычных ошибок не обеспечивает
восстановление при KeyboardInterrupt/SIGTERM. При любом отказе сохранить
вывод и перейти к разделу 7, а не запускать update повторно.

## 4. Internal: подготовка под root

Ни временный `update-internal-v042.py`, ни его старый SHA не являются способом
обновления на следующий выпуск: тот скрипт закреплён именно на 0.4.2. Ниже —
ручная последовательность через штатные root-скрипты хоста. Она также подходит
для public, если установленного проверенного helper нет: в `select_instance`
выбрать `public` и сверить таблицу параметров выше.

Открыть отдельный Bash в root-сессии tmux/screen. Все следующие блоки выполнять
в **одной этой оболочке**, по порядку. После ошибки не выполнять следующий
блок: `set -e` может завершить вложенный Bash, и его переменные будут потеряны.

```bash
/usr/bin/env -i HOME=/root PATH=/usr/sbin:/usr/bin:/sbin:/bin \
  LANG=C.UTF-8 LC_ALL=C.UTF-8 /bin/bash --noprofile --norc
set -euo pipefail
umask 077
export GIT_TERMINAL_PROMPT=0 GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null

. /disk2/plombir/bin/common.sh
need_root
guard_disk
select_instance internal

# Менять вместе: это пример уже выпущенной 0.4.2.
TARGET_TAG=v0.4.2
TARGET_COMMIT=af6ef3f68bd2541b9d79f96dd5fba287e89d1fba

g() {
  git -c gc.auto=0 -c core.hooksPath=/dev/null -c credential.helper= \
    -C "$BASE/src" "$@"
}
check_config() {
  /usr/bin/python3 -I - "$BASE/plombir-git.toml" "$EXPECTED_UID" <<'PY'
import os, stat, sys
path, uid = sys.argv[1], int(sys.argv[2])
s = os.lstat(path)
assert stat.S_ISREG(s.st_mode) and s.st_uid == uid and s.st_gid == uid
assert stat.S_IMODE(s.st_mode) in (0o400, 0o600) and s.st_nlink == 1
assert 'system.posix_acl_access' not in os.listxattr(path, follow_symlinks=False)
print('Config ownership/mode/link count/ACL: OK')
PY
}

printf 'instance=%s base=%s project=%s uid=%s port=%s\n' \
  "$INSTANCE" "$BASE" "$PROJECT" "$EXPECTED_UID" "$HTTP_PORT"
check_config
dc config --quiet
dc config --format json | /usr/bin/python3 -I -c '
import json, sys
s = json.load(sys.stdin)["services"]["plombir-git"]
print(json.dumps({k:s.get(k) for k in ("image","user","volumes")}, indent=2))'
df -h /disk2
free -h
```

До продолжения сверить вывод: Compose project/образ относятся только к выбранному
экземпляру, `user` не переопределён, `/data` — его собственный каталог, TOML
подключён в `/app/plombir-git.toml` только для чтения, `docker.sock` отсутствует.
Контрольные файлы `.env`, `compose.yml`, `src/.git/config`, root-скрипты и их
родительские каталоги принадлежат root и не доступны другим на запись.
Конфиг принадлежит UID из таблицы, а каталог `data` — тому же UID/GID с режимом
`0700`. Служебному UID не нужны shell, sudo или SSH-доступ.

При несовпадении остановиться и разобрать причину. Не исправлять доступ через
`chmod 640/644`, ACL, запуск контейнера от root или замену ключей шифрования.
Симлинк `/root` сам по себе не требует исправления; приватные служебные файлы
можно хранить в проверенном root-only каталоге на `/disk2`.

Проверить источник и целевой коммит **до изменения checkout и образа**:

```bash
[[ "$TARGET_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]
[[ "$TARGET_COMMIT" =~ ^[0-9a-f]{40}$ ]]
test "$(docker inspect -f '{{.State.Running}}' "$CONTAINER")" = true
test -z "$(g status --porcelain --untracked-files=all)"
OLD_COMMIT=$(g rev-parse HEAD)
CURRENT_COMMIT=$(curl --noproxy '*' -fsS \
  "http://127.0.0.1:$HTTP_PORT/api/v1/instance" |
  /usr/bin/python3 -I -c 'import json,sys; print(json.load(sys.stdin)["source_commit"])')
test "$CURRENT_COMMIT" = "$OLD_COMMIT"
curl --noproxy '*' -fsS "http://127.0.0.1:$HTTP_PORT/readyz"

g fetch --no-tags https://github.com/Nettechnologys/plombir-git.git \
  "refs/tags/$TARGET_TAG:refs/tags/$TARGET_TAG"
test "$(g rev-parse --verify "refs/tags/$TARGET_TAG^{commit}")" = "$TARGET_COMMIT"
g merge-base --is-ancestor "$OLD_COMMIT" "$TARGET_COMMIT"
```

Не использовать `--force` для тега и не обходить отказ `merge-base`. Если
`OLD_COMMIT` уже равен `TARGET_COMMIT`, сборка и миграция не нужны: выполнить
раздел 6 для текущего образа и закончить.

## 5. Сборка, холодная копия, миграция и запуск

Для ручного пути из раздела 4 сначала сохранить исходное состояние. Каталог
резервных копий должен быть обычным root-owned каталогом `0700`, не симлинком.

```bash
install -d -o root -g root -m 0700 "$ROOT/backups/$INSTANCE"
RUN_DIR=$(mktemp -d "$ROOT/backups/$INSTANCE/upgrade-$TARGET_TAG.XXXXXXXX")
OLD_IMAGE=$(docker inspect -f '{{.Image}}' "$CONTAINER")
IMAGE=$(dc config --format json |
  /usr/bin/python3 -I -c 'import json,sys; print(json.load(sys.stdin)["services"]["plombir-git"]["image"])')
test "$IMAGE" = "plombir-$INSTANCE:local"
ROLLBACK_IMAGE="plombir-$INSTANCE:before-$(date -u +%Y%m%dT%H%M%SZ)"
printf '%s\n' "INSTANCE=$INSTANCE" "OLD_COMMIT=$OLD_COMMIT" \
  "OLD_IMAGE=$OLD_IMAGE" "ROLLBACK_IMAGE=$ROLLBACK_IMAGE" \
  "TARGET_TAG=$TARGET_TAG" "TARGET_COMMIT=$TARGET_COMMIT" > "$RUN_DIR/record.txt"
printf 'Recovery directory: %s\n' "$RUN_DIR"
docker image tag "$OLD_IMAGE" "$ROLLBACK_IMAGE"
printf '%s\n' building > "$RUN_DIR/stage"

g checkout --detach "$TARGET_COMMIT"
dc build --build-arg "PLOMBIR_GIT_SOURCE_COMMIT=$TARGET_COMMIT" \
  --build-arg "PLOMBIR_GIT_VERSION=$TARGET_TAG" \
  --build-arg "PLOMBIR_GIT_UID=$EXPECTED_UID" \
  --build-arg "PLOMBIR_GIT_GID=$EXPECTED_UID" plombir-git 2>&1 | tee "$RUN_DIR/build.log"
BUILT_IMAGE=$(docker image inspect -f '{{.Id}}' "$IMAGE")
printf 'BUILT_IMAGE=%s\n' "$BUILT_IMAGE" >> "$RUN_DIR/record.txt"
test "$(docker run --rm --network=none --pull=never --entrypoint id "$BUILT_IMAGE" -u)" = "$EXPECTED_UID"
test "$(docker run --rm --network=none --pull=never --entrypoint id "$BUILT_IMAGE" -g)" = "$EXPECTED_UID"
test "$(docker run --rm --network=none --pull=never --entrypoint plombir-git "$BUILT_IMAGE" --version)" = "plombir-git $TARGET_TAG"
```

Эти проверки запускают только одноразовые контейнеры **без production mounts**.
Не заменять их на `dc run`: bind mounts с SELinux `:Z` могут изменить метки
живого каталога данных. Убедиться, что сборка завершилась без ошибки, прежде чем
переходить к остановке.

Следующий блок начинает простой выбранного экземпляра. `backup.sh` останавливает
его и сохраняет холодную копию `data`, `.env`, TOML и Compose с ACL/xattrs/SELinux
и числовыми владельцами. Флаг `--leave-stopped` оставляет его остановленным.

```bash
check_config
printf '%s\n' backup > "$RUN_DIR/stage"
/bin/bash -p "$ROOT/bin/backup.sh" "$INSTANCE" --leave-stopped 2>&1 |
  tee "$RUN_DIR/backup.log"
```

Сохранить напечатанный путь архива и проверить соседний файл `.sha256` командой
`sha256sum -c /полный/путь/архива.tar.gz.sha256`. При ошибке копирования или
контрольной суммы миграцию не начинать. После успешной проверки:

```bash
printf '%s\n' migrating > "$RUN_DIR/stage"
dc run --rm --no-deps -T --interactive=false plombir-git \
  plombir-git migrate --config /app/plombir-git.toml < /dev/null 2>&1 |
  tee "$RUN_DIR/migrate.log"
```

Для перехода **0.4.1 → 0.4.2** ожидаются код 0 и `No pending migrations`.
Если что-то применилось вопреки этому ожиданию или команда упала — остановиться
и изучить журнал, не пытаться автоматически откатить БД. Для других выпусков
ожидание определяется их release notes. Уровень логирования может скрыть INFO:
отсутствие строки само по себе не доказывает, что миграция выполнялась или нет.

После принятого результата миграции:

```bash
printf '%s\n' starting > "$RUN_DIR/stage"
/bin/bash -p "$ROOT/bin/start.sh" "$INSTANCE" 2>&1 | tee "$RUN_DIR/start.log"
```

Проверенный `start.sh` ждёт `/readyz` с повторами. Возврат управления после
`docker ... Started` ещё не заменяет финальную проверку ниже.

## 6. Что считать успехом

Для public задать `HTTP_PORT=30443`, для internal — `10443`; `TARGET_TAG` и
`TARGET_COMMIT` должны оставаться выбранными в начале. Это только безопасные
GET-запросы; доступ к Docker для них не нужен.

```bash
/usr/bin/python3 -I - "$HTTP_PORT" "$TARGET_TAG" "$TARGET_COMMIT" <<'PY'
import json, sys, urllib.request
port, tag, commit = sys.argv[1:]
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None
client = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
for path in ('/readyz', '/api/v1/instance', '/health', '/'):
    with client.open('http://127.0.0.1:' + port + path, timeout=10) as response:
        assert response.status == 200, path
        if path == '/api/v1/instance':
            assert json.load(response)['source_commit'] == commit
        elif path == '/health':
            health = json.load(response)
            assert health['status'] == 'ok'
            assert health['version'] == tag[1:] and health['commit'] == commit
            print(json.dumps(health, indent=2))
        print(path, '200 OK')
PY
```

Если сборка и запуск выполнялись по разделу 5, дополнительно проверить образ
и только затем записать успех:

```bash
test "$(docker inspect -f '{{.Image}}' "$CONTAINER")" = "$BUILT_IMAGE"
printf '%s\n' complete > "$RUN_DIR/stage"
docker logs --since 10m "$CONTAINER" > "$RUN_DIR/startup.log" 2>&1
```

При пропуске сборки из-за уже установленного SHA последний Bash-блок не
выполнять: `RUN_DIR` и `BUILT_IMAGE` в такой сессии не заданы. Проверить текущий
образ по сохранённой записи успешного обновления, не создавать новую запись
об якобы выполненной сборке или миграции.
Просмотреть стартовый журнал на новые ошибки, неизвестную visibility организаций
и нечитаемые зашифрованные поля, включая push mirrors. При проблеме с ключом
следовать release notes; не генерировать замену ключа поверх существующих данных.

Сохранить тег, SHA, ID старого и нового образов, путь и checksum холодной копии,
результат миграции и HTTP-проверок. Затем повторить процедуру для второго
экземпляра. Только после успеха **обоих** обновить общие release-пины в
`common.sh`. Релиз из GHCR и локальная сборка из того же тега — разные артефакты:
в записи указывать, какой именно образ был запущен.

## 7. Если произошла ошибка или оборвалась сессия

Не запускать старый `update.sh`, не удалять recovery record и не повторять
миграцию вслепую. Проверить контейнер, `/health`, журналы и последнюю стадию.
У helper запись находится в `backups/public/ops-*.json`; у ручного пути — в
напечатанном `RUN_DIR`. Исходный неуспешный результат сохранять как неуспешный;
ручное восстановление записывать отдельно со временем и исполнителем.

| Где остановились | Что делать |
|---|---|
| Проверки до изменения checkout/тега образа | Исправить причину отказа; сервис обычно ещё работает. |
| Сборка, проверка UID/версии или backup; миграция точно не запускалась | Владелец может вернуть **сохранённые** старые image ID и commit, затем запустить выбранный экземпляр. При backup он мог остаться остановленным. БД не восстанавливать без отдельной причины. |
| `migrating`, либо неизвестно, запускалась ли миграция | Автоматический откат запрещён. Сохранить журналы и выяснить состояние БД. При необходимости восстанавливать согласованный комплект БД, репозиториев, конфигурации, ключей и образа по процедуре соответствующего выпуска. |
| `starting` или финальный HTTP-check | Новая версия уже может работать. Проверить `/health`, `/readyz` и образ; исправлять запуск по фактам. Повторное `Already running` не закрывает неуспешную проверку. |

Пример **только для подтверждённого отказа до миграции**, под root. Значения
`OLD_IMAGE` и `OLD_COMMIT` взять из записи именно этой попытки; пути и имя
экземпляра снова сверить с таблицей. Записи не исполнять через `source`:

```bash
docker image tag "$OLD_IMAGE" "plombir-$INSTANCE:local"
git -c gc.auto=0 -c core.hooksPath=/dev/null -C "$BASE/src" checkout --detach "$OLD_COMMIT"
/bin/bash -p /disk2/plombir/bin/start.sh "$INSTANCE"
```

Для инцидента с TOML сначала проверить владельца, режим и mount. И 0.4.1, и
0.4.2 отказывают при group/other-доступе к конфигу: возврат старого образа не
исправляет `0640`. Допустимое исправление владельцем — правильный служебный
UID/GID, `0600` (или `0400`), отсутствие расширенного ACL, один hard link,
root-owned родитель и read-only bind mount. Приложение не запускать от root и
проверку owner-only не отключать.
