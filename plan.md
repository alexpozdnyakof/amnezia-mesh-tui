# awg-mesh-tui — план разработки

Рабочий план проекта. Мастер-план и контекст: `../awg/plan.md` (Эпик 2). Референс-репо: `../amnezia-mesh` (python CLI, canonical, источник фикстур).

Обновлено: 2026-10-26.

## Продукт

Опенсорсная TUI-панель управления для **любого** деплоя amnezia-mesh. Администратор указывает свой сервер по ssh — панель либо создаёт сеть с нуля (`init`), либо проводит миграцию существующего AmneziaWG-контейнера под управление amnezia-mesh (`adopt`), и дальше администрирует сеть: пиры, ключи, профили, мониторинг. Локальное приложение (macOS/linux); транспорт — shell-out к системному ssh-клиенту (ключи из ssh-config пользователя, секретов в приложении нет); read-only по умолчанию, мутации — с явным подтверждением; приложение ничего не слушает снаружи.

Не входит в скоуп: привязки к личным сервисам/инфраструктуре конкретных серверов. Частные интеграции решаются generic post-mutation хуком в конфиге.

## Главный флоу: первый запуск → создание/миграция → сервисы

0. Пользователь добавляет сервер в конфиг панели (ssh-алиас/хост, путь к CLI на сервере — с дефолтами).
1. **Preflight:** ssh-доступ, docker и compose plugin на сервере, наличие amnezia-mesh CLI (если нет — предложение bootstrap: клонировать репо, pinned tag, в выбранный каталог; нужен git на сервере).
2. Выбор режима: **«Новый сервер» (init)** или **«Миграция» (adopt)**.

**init:**
3. Форма: public host/IP (автодетект внешнего IP как подсказка), port (443), subnet (100.64.0.0/24), domain (awg), image (amneziavpn/amneziawg-go:latest), rdp-reject.
4. CLI делает всё сам: pull образа → проба I1–I5 → генерация ключей и обфускации → render → compose up → doctor. Панель показывает живой прогресс.

**adopt:**
3. **Discovery:** панель перечисляет docker-контейнеры сервера (`docker ps -a`), определяет кандидатов (образ amneziawg-go / наличие `/opt/amnezia/awg/awg0.conf` внутри) → выпадающий список выбора; режим «файл конфигурации» (`adopt --config`) тоже доступен.
4. **Предпросмотр (не разрушающий):** `adopt --inspect --container <name> --json` (см. Шаг 0) — сводка: серверный ключ/обфускация/MTU/ListenPort, список пиров с именами, warnings. Пользователь подтверждает.
5. Форма параметров: host, port, domain, dns1/dns2, rdp-reject.
6. Прогон `adopt` с живым выводом → doctor-панель; при провале CLI сам откатывает (старый контейнер стартует обратно) — панель показывает экран отката с деталями и инструкцией.

**Дальше (оба режима):** переход в основной интерфейс: сервисы (статус контейнеров стека) + дашборд пиров.

## Фичи основного интерфейса (из Эпика 2 мастер-плана)

- **Сервисы:** статус контейнеров стека (compose ps): awg + dns.
- **Live-пиры:** poll `awg show` раз в 2–5с — имя, IP, handshake age, endpoint, мгновенные rx/tx (diff между опросами), статус live/stale/dead.
- **Выпуск ключей:** peer add → сервер генерирует, TUI показывает профиль `.vpn` + `vpn://`-ссылку (+QR), предлагает сохранить локально. Секреты не храним дольше показа.
- **Rename** → MagicDNS-имя `<name>.awg` видно сразу.
- **Doctor-панель live** (8 проверок).
- **Журнал + графики** поверх `awg-mesh monitoring` (stats каждые 10 мин в `/var/log/amnezia-mesh-stats.log`, cap 30к строк):
  - line-chart rx/tx per peer (ratatui `widgets::Chart`),
  - тепловая карта «кто когда был онлайн»,
  - интерактивный лог: фильтр по пиру, follow, скролл,
  - availability-таймлайн по watchdog-событиям.
- **Карта подключений (geoip):** canvas-вид «хаб в центре, пиры по geoip их endpoint-IP» в зелёных тонах; offline mmdb (GeoLite2/IP2Location LITE, файл в `~/.cache/awg-mesh-tui`), live/stale — яркостью точек; без mmdb — fallback-таблица пир → страна/город/ASN. Топология честно hub-and-spoke (P2P-рёбра — бэклог мастер-плана).
- **Обогащённые логи:** поверх журнала M3 — pubkey→имя, классификация событий (handshake / keepalive / transfer), связь события с карточкой пира в дашборде.

## Окружение разработки и проверки (наш тестовый сервер)

Личные детали конкретных серверов не входят в скоуп продукта, но это наше рабочее окружение:

- **Расположение:** этот репо — `/Users/alexpozdnyakof/Desktop/digital_resistance/amnezia-mesh-tui`; референс-CLI — `/Users/alexpozdnyakof/Desktop/digital_resistance/amnezia-mesh`; мастер-план — `/Users/alexpozdnyakof/Desktop/digital_resistance/awg/plan.md` (там прод-история, в т.ч. детали нашего сервера).
- **Тестовый сервер:** ssh-алиас `vdsina_awg` (в `~/.ssh/config`; fallback на прямой IP встроен в алиас; жив, проверен). CLI там — `/root/amnezia-mesh/bin/awg-mesh` (awg-mesh 0.2.0; в PATH нет — вызывать абсолютным путём; checkout репо — `/root/amnezia-mesh`).
- **Локальный стенд (основной для M1):** colima (VZ + Rosetta → amd64-образы) + ssh-алиас `awg-tui-test` → 127.0.0.1 (ключ `~/.ssh/awg_test_key`, блок в `~/.ssh/config`, добавлены 2026-10-23; убрать = удалить блок и ключ). Скрипты `teststand/up.sh` / `down.sh` поднимают «классический» контейнер `amnezia-legacy-test` (образ `amneziavpn/amneziawg-go`, udp 51820, конфиг `teststand/.gen/` в gitignore) с 3 пирами под edge-cases adopt: чистый с датой / без комментария (генерация `peer-xxxxxx`) / `# MacBook Pro (RDP fix)` (санитизация имени). Интерфейс внутри поднят в userspace (`amneziawg-go` + `awg setconf`): в colima нет amnezia-модуля ядра, а ядерный путь `awg-quick` отвергает амнезиа-параметры (`Unable to modify interface: Invalid argument`).
- **⚠️ adopt-e2e на vdsina — никогда:** compose-проект захардкожен (`name: amnezia-mesh`), тестовый adopt увидит прод-контейнеры как свои → `compose up` перезапишет их, при провале `compose down` снесёт прод-стек. На vdsina — только неразрушающие discovery/inspect (и то по «ок»); полный цикл миграции — на локальном стенде либо на отдельном VPS.
- **Тулчейн:** на маке nightly 1.96 (default); для репо зафиксировать stable через `rust-toolchain.toml` — решить на старте разработки.
- **Фикстуры:** форматы stats-лога и `awg show` подтверждены на живом сервере (включая квирк дублирующихся `=== ===` заголовков); в репо коммитим сэмплы с зачищенными секретами.

## Контракт с сервером (проверено в коде `../amnezia-mesh`)

- CLI: `bin/awg-mesh` (stdlib-only python), `--json` в любом месте командной строки, ошибки → `{"ok": false, "error"}`. Прогресс (`say()`) в `--json`-режиме глушится — для предпросмотра нужен `--inspect` (Шаг 0).
- **init:** `awg-mesh init --host <pub-ip> [--port 443] [--subnet 100.64.0.0/24] [--domain awg] [--image amneziavpn/amneziawg-go:latest] [--rdp-reject]`. Механика (`cli.py:cmd_init`, `keygen.py`): pull образа → проба I1–I5 (throwaway-контейнер с `awg-quick up` на тестовом конфиге; нет `awg-quick` = жёсткий отказ; результат в `AWG_MESH_I_SUPPORTED`) → ключи через `docker run --rm --network none --entrypoint awg <image> genkey|genpsk|pubkey` (хосту нужен только docker) → обфускация генерируется на хосте (random.SystemRandom, диапазоны как в оригинале AmneziaWG) → `config/server.env` (0600) + render → compose up → doctor. Пробе I1–I5 нужен `/dev/net/tun` на сервере.
- **adopt:** `awg-mesh adopt (--container <name> | --config <путь> --image <image>) --host <pub-ip> [--port 443] [--domain awg] [--dns1 1.1.1.1] [--dns2] [--rdp-reject]` (`cli.py:cmd_adopt`, `adopt.py`). Механика: чтение `/opt/amnezia/awg/awg0.conf` из контейнера (образ — из `docker inspect`) → парсинг → запись state → render → **стоп старого контейнера (не удаляется)** → compose up → doctor; при провале — автоматический откат (старый контейнер стартует обратно) + в выводе инструкция отката.
- **Ограничения adopt-парсера (мастер обязан показывать их как объяснимые ошибки, а не падение панели):** неизвестные `[Interface]`-ключи → отказ (не дропаем молча); `Address` обязан быть первым хостом своей сети; у каждого пира обязательны PublicKey, PresharedKey (mesh-профили требуют PSK), AllowedIPs; имена пиров — из комментария `# name (added ...)` над секцией, санируются к `[a-z0-9-]{1,32}`, без имени генерируется `peer-xxxxxx` (всё с warnings); keepalive первого пира → `SERVER_KEEPALIVE`; `Endpoint` пиров игнорируется (клиентская сторона); у adopted-серверов `AWG_MESH_I_SUPPORTED=0`.
- **render** (`render.py`): детерминирован из `server.env` + `peers.db` → `config/awg0.conf` (0600, комментарий пира `# name (added ISO)` круговходит через adopt), `config/dnsmasq.conf` (host-record на пир → MagicDNS), `config/start.sh` (ip_forward, iptables FORWARD/MASQUERADE, MSS-clamp MTU−40 в обе стороны, опциональный RDP-reject UDP/3389).
- JSON-вывод:
  - `peer list` → `{ok, peers: [{name, ip, created}]}` — **pub пока нет, см. Шаг 0** (`awg_mesh/cli.py:346`).
  - `peer add` → `{ok, name, ip, profiles: [пути на сервере]}` — профили пишутся на сервер в `config/peers/<name>-{exit,mesh}.{vpn,conf}`.
  - `peer rename` → `{ok, renamed, name}`; `peer revoke` → `{ok, revoked}` (спросит confirm; `AWG_MESH_NONINTERACTIVE=1` авто-yes — TUI подтверждает сам и дёргает с этим env, `awg_mesh/cli.py:48`).
  - `doctor` → `{ok, checks: [{name, ok, detail}]}` — 8 проверок: docker-daemon, initialized, awg-container, dns-container, listen-port, ip-forward, magicdns, port-published. Retry 15×1с на listen-port и magicdns (`awg_mesh/doctor.py:17`) → doctor на медленном poll.
- `awg show` (внутри контейнера awg) показывает пиров по pubkey → нужен маппинг pubkey→name. Источник: `config/peers.db` (TSV: name, ip, pub, psk, created — `awg_mesh/peers.py:14`).
- Каждая мутация → `apply_conf` → restart контейнеров стека (`awg_mesh/cli.py:89`) → poll падает на ~5–10с, это ожидаемо.
- Stats-лог: блоки `=== <ISO8601 от date -Is> ===`, строки `peer: / endpoint: / latest handshake: / transfer:` (`scripts/awg-stats.sh`); пустые блоки и дубли заголовков встречаются — парсер терпит.
- Watchdog-лог `/var/log/amnezia-mesh-watchdog.log`: строки `<ISO> <container> down, restarting` / `wedged, restarting` (`scripts/awg-watchdog.sh:6`) — источник availability-таймлайна.
- Кодек vpn://: `"vpn://" + base64url(uint32be(len) + zlib(json, 9))` (`awg_mesh/profile.py:63`).

## Архитектура

- **Стек:** ratatui + crossterm + tokio (process, time, sync), serde/serde_json, clap, base64, flate2, chrono, qrcodegen (профили).
- **Транспорт:** shell-out к системному `ssh <alias>` через `tokio::process`. Ключи/агент из ssh-config; fallback на прямой IP — свойство ssh-конфигурации пользователя, приложение к нему не прикасается. Удалённые команды выполняются через login-shell обёртку (login-shell пользователя): в non-interactive ssh PATH часто без `docker` (найдено на стенде: macOS + colima). Preflight определяет путь к `docker` на сервере и кеширует его в конфиге сервера.
- **Каркас:** трейт `Transport`, реализации `Ssh` и `Mock` (тесты, dev без сервера). Все парсеры — чистые функции + фикстуры (как golden-тесты python-репо).
- **Конфиг** `~/.config/awg-mesh-tui/config.toml`: список серверов (ssh-алиас, путь к CLI на сервере, имя контейнера), активный сервер, интервалы poll, пороги статусов, post-mutation хук.

## Решения по данным

- Быстрый poll — один ssh-вызов на тик (`awg show`), ~3с. Маппинг pubkey→name кешируется: перечитывать после мутаций + раз в несколько минут.
- Скорости rx/tx: diff кумулятивного transfer / dt; рестарт контейнера сбрасывает счётчики → отрицательные diff clamp в 0.
- Пороги статусов (конфиг, дефолты): live ≤ 3 мин, stale ≤ 24 ч, dead дальше. Кандидат на revoke: 14 дней без handshake.
- Журнал: чтение через `ssh tail -n N` / `grep по пиру`, 30к строк целиком не тянуть. Парсер wg-времён: «1 minute, 23 seconds ago», «off», отсутствие строки; единицы KiB/MiB/GiB → байты. Тепловая карта: бакеты 10 мин для суточного зума (144 кол.), часовые для недельного (168 кол.).
- QR: спайк — реальный vpn:// (~0.9–1.4 КБ после zlib+base64) сканируется с телефона из терминала; если плотность неудобная — основной путь «сохранить .vpn / скопировать ссылку», QR как удобство + экспорт в PNG.

## Milestones

### Шаг 0 — патчи python CLI

Два маленьких патча в `../amnezia-mesh` (backward-compatible, коммитим там):

1. `peer list --json` отдаёт `pub` (одна строка) — без него маппинг pubkey→name только через `cat config/peers.db` по ssh (тащит PSK в дашборд); заодно достанется Go-порту.
2. `adopt --inspect (--container <name> | --config <путь> --image <image>) --json` — не мутирующий разбор конфигурации: сводка (серверный ключ/число обфускационных параметров/MTU/ListenPort, список пиров с именами и IP, warnings, список жёстких ошибок если конфиг непригоден). Human-режим сейчас печатает это через `say()`, но в `--json` он глушится — мастеру нужен экран предпросмотра до подтверждения.

### M1 — подключение и мастер создания/миграции

1. Конфигурация сервера в панели (ssh-алиас, путь к CLI, дефолты) + проверка связи.
2. Bootstrap CLI на сервере: проверка `bin/awg-mesh --version`; если нет — предложение клонировать репо (pinned tag) в выбранный каталог.
3. Preflight окружения: docker, compose plugin, git (для bootstrap), для init — `/dev/net/tun`; понятные сообщения при отсутствии.
4. Два режима:
   - **init:** форма параметров (host с автодетектом внешнего IP, port, subnet, domain, image, rdp-reject) → прогон с живым выводом → doctor.
   - **adopt:** discovery (`docker ps -a`, кандидаты по образу/`/opt/amnezia/awg/awg0.conf`) → выпадающий список → экран предпросмотра (`adopt --inspect`) → форма параметров → прогон с живым выводом → doctor; экран отката при провале.
5. После успеха — переход в дашборд (M2).

Приёмка: на сервере с зависшим amneziawg-контейнером пользователь проводит миграцию до зелёного doctor, не открывая терминал; на чистом сервере — создаёт сеть через init.

### M2 — дашборд сервисов и пиров (read-only)

1. Парсер `awg show` (нет handshake, endpoint `(none)`, возраст «N minutes/hours/days ago») + модель Peer.
2. Поллер: peers ~3с, doctor 45–60с; кеш маппинга pubkey→name.
3. UI: сервисы (compose ps), таблица пиров (имя, IP, статус, handshake age, endpoint, rx/tx), панель doctor, баннер ssh-ошибок (таймауты, переподключение, никаких паник).

Приёмка: read-only админка любого (в т.ч. нашего) сервера; мутаций нет в принципе.

### M3 — журнал/графики

1. Парсер stats-лога + фикстуры.
2. Line-chart rx/tx на выбранный пир (даунсэмпл под ширину терминала).
3. Тепловая карта presence.
4. Интерактивный лог: фильтр по пиру, follow, скролл.
5. Availability-таймлайн из watchdog-лога.

### M4 — мутации

1. Confirm-диалоги; вызовы CLI `--json` + `AWG_MESH_NONINTERACTIVE=1`.
2. add → забрать `.vpn` по ssh, показ vpn:// + сохранение локально; секреты zeroize после показа.
3. rename/revoke; в revoke — подсказка «стейл N дней» (данные M3).
4. Post-mutation хук из конфига; подавление poll-ошибок ~10с после мутации.

### M5 — профили и stale

1. Кодек vpn:// на Rust; golden-roundtrip против фикстур, сгенерированных python CLI.
2. Decode-view профиля.
3. Подсветка stale-пиров (N дней без handshake), поток «кандидат на revoke одной клавишей».
4. QR: спайк сканируемости → виджет → PNG-экспорт.

### M6 — карта подключений (geoip)

1. Провайдер offline-БД: GeoLite2 (maxminddb) или IP2Location LITE; mmdb кладёт пользователь в `~/.cache/awg-mesh-tui` — никаких онлайн-запросов по IP пиров (приватность).
2. Резолв endpoint-IP пиров (поллер M2) → страна/город/ASN/lat-lon.
3. Canvas-вид (ratatui `widgets::Canvas`): хаб по координатам сервера в центре, пиры точками по lat-lon, линии пир↔хаб; live/stale — яркостью/насыщенностью. Зелёная тема — базовое направление оформления дашборда.
4. Fallback без mmdb: таблица пир → страна/город/ASN текстом.
5. Топология честно hub-and-spoke; P2P-рёбра — бэклог мастер-плана.

Выполняется после M3 (нужен поллер endpoint'ов M2), параллельно/до M4 по готовности данных.

### Бэклог патчей python CLI (`../amnezia-mesh`)

- `peer rotate` — перевыпуск ключей пира («редактирование» глубже rename).
- Кастомные DNS-записи (host-record'ы сверх MagicDNS `<name>.awg`) — под фичу «назначать домены» пирам.

## Тесты / CI

Юнит-тесты всех парсеров на фикстурах (коммитим в `tests/fixtures/`, секреты зачищены), transport-mock, GitHub Actions: `cargo test` + clippy + fmt.

## Риски

- ssh-флэки → таймауты + баннер в UI, никаких паник.
- Generic-ядра без amnezia-модуля: `awg-quick` падает на `awg setconf` (ванильный wg не понимает амнезиа-параметры) → после init/adopt doctor не станет зелёным. Локально закрыто userspace-подъёмом в стенде; для прод-стека варианты — runtime-сборка модуля образом (нужны headers хоста, как на vdsina) или userspace-fallback в `start.sh` (см. Открытое).
- Bootstrap требует git на сервере пользователя; если нет — честное сообщение с инструкцией.
- Разнообразие серверов пользователей (docker без sudo в PATH, иные ОС, нестандартные конфиги) → все команды через обёртки с понятными ошибками; жёсткие ошибки adopt-парсера (неизвестные ключи, нестандартная схема IP) показываем текстом с рекомендацией (issue/ручная правка), а не падаем.
- Adopt/init — длинные операции с рестартами; вывод построчно, UI не блокируется.
- Doctor медленный (retries) → отдельный медленный цикл.
- Мутации бьют по поллингу (рестарт контейнеров) → ожидаемо, подавляем.
- Cap 30к строк старой истории обрежет тепловую карту → приемлемо.

## Открытое (не блокирует старт)

- Userspace-fallback в `start.sh` (патч в `../amnezia-mesh`) для серверов без amnezia-модуля ядра — продуктово полезно для generic VPS; до решения такие сервера не пройдут doctor после init/adopt (локальный стенд ходит в обход через ручной подъём, прод-стек — нет).
- Дефолты порогов live/stale/revoke-N — калибруем на живых данных M2/M3.
- Раздача: Mac cask `awg-mesh` (вместе с Go CLI). `awg-mesh-tui install` копирует linux-payload на сервер по ssh; `init`/`adopt` делает мастер панели. `cargo install` не используем.
- Multi-server UI (переключатель между серверами) — после M2.
- Зависимость от Эпика 1 (Go CLI): слабая, TUI говорит JSON-over-ssh; M4 работает на python CLI как canonical сейчас.

## Порядок работ

1. Шаг 0 (два патча python CLI) → M1 (мастер создания/миграции — главный флоу онбординга).
2. M2 (дашборд сервисов и пиров: список, add/rename/revoke, выпуск ключей, MagicDNS `<name>.awg`) → M3 (журнал/графики) → M6 (карта/geoip) — самая заметная ценность для существующих пользователей.
3. M4 (мутации) → M5 (профили + QR).
