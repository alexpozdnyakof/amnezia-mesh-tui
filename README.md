# awg-mesh-tui

TUI-панель управления для любого деплоя [amnezia-mesh](https://github.com/alexpozdnyakof/amnezia-mesh-go)
— self-hosted WireGuard-меша с обфускацией AmneziaWG и MagicDNS. Rust + ratatui.

Панель подключается к серверу по SSH системным клиентом (shell-out, транспорт в
`src/transport.rs`), ничего не слушает снаружи и не требует своего бэкенда: на
сервере должен лежать только CLI `awg-mesh` (Go-бинарь или Python-версия).
Read-only по умолчанию; мутации сети выполняются только с явным подтверждением
в UI. Секреты (приватные ключи, PSK) не хранятся дольше момента показа.

## Установка

macOS — одним cask вместе с CLI:

```sh
brew tap alexpozdnyakof/tap
brew install --cask alexpozdnyakof/tap/awg-mesh   # awg-mesh + awg-mesh-tui
```

Или собрать из исходников (нужен stable Rust):

```sh
git clone https://github.com/alexpozdnyakof/amnezia-mesh-tui
cd amnezia-mesh-tui
cargo build --release        # target/release/awg-mesh-tui
```

## Быстрый старт

```sh
awg-mesh-tui install --ssh root@YOUR_VPS   # копирует linux CLI на сервер
awg-mesh-tui                               # мастер → дашборд
```

`install` копирует bundled linux-бинарь `awg-mesh` (payloads `linux/amd64` и
`linux/arm64` лежат рядом с панелью в cask, либо `--payload_dir`) в
`~/.local/bin/awg-mesh` на сервере и записывает сервер в конфиг панели. Сеть
при этом не создаётся — `init` / `adopt` делает мастер внутри панели.

## `install`

```sh
awg-mesh-tui install [--ssh user@host] [--identity key] [--json] [--no_save]
```

- `--ssh` — alias или `user@host`; без него спросит в форме (с `--json`
  обязателен: JSON-режим не prompting'ит и печатает один объект
  `{ok, ssh, arch, remote_path, version, server_name, config}`).
- SSH — системный клиент: alias'ы и ключи берутся из `~/.ssh/config` / агента,
  `--identity` передаётся как `ssh -i`.
- `--no_save` — не писать сервер в конфиг панели.
- Без аргументов — интерактивная форма с одним полем (SSH-цель). Пароли не
  спрашиваются.

## Мастер

Главный флоу — миграция/создание сети одним VPS:

1. **Preflight** — проба сервера: docker, версия CLI, заметки. `d` — список
   контейнеров, `i` — мастер `init`, `b` — доустановить CLI, `g` — сразу в
   дашборд (если сеть уже есть).
2. **Discovery** — контейнеры сервера; ◆ помечены кандидаты в миграцию
   (AmneziaWG-контейнеры). Enter — inspect: подсеть, MTU, listen-порт,
   obfuscation-параметры, пиры, предупреждения.
3. **Мастер `init` / `adopt`** — форма (хост, порт, DNS-домен, параметры
   обфускации; `h` — автоопределение внешнего IP), финальный confirm, экран
   выполнения с логом. `init` поднимает новый стек, `adopt` переносит
   существующий AmneziaWG-контейнер под управление `awg-mesh` (профили
   пересобираются, публичные ключи сохраняются).
4. После успеха — Enter открывает **дашборд**.

## Дашборд

- **Live-пиры** — poll `awg show` раз в 2–5 с: имя, IP, возраст handshake,
  endpoint, мгновенные rx/tx; статусы live/stale/dead.
- **Выпуск ключей** — `a` add: сервер генерирует профиль, панель рисует QR
  прямо в терминале (брайль, чёрный по белому); `s` — сохранить `.vpn`
  локально; `k` — перечитать сохранённый на сервере ключ без ротации.
- **Rename / revoke** — `r` / `x` с confirm-диалогом; `c` — revoke самого
  stale-кандидата (подсказка по дням без handshake).
- **Карта** (`m`) — geoip-раскладка endpoint'ов вокруг сервера.
- **Журнал** (`l`) — `awg-mesh monitoring`-статистика: rx/tx-графики,
  тепловая карта присутствия, фильтр по пиру.
- **Doctor** — 8 live-проверок здоровья стека.

## Одноразовые режимы (без TUI)

```sh
awg-mesh-tui --probe_once            # проба сервера, выход
awg-mesh-tui --discover_once         # список контейнеров, выход
awg-mesh-tui --inspect_once NAME     # adopt --inspect контейнера, выход
awg-mesh-tui --peers_once            # таблица live-пиров (awg show), выход
```

Плюс `--server NAME` (выбор записи из конфига) и `--config PATH`.

## Конфиг

`~/.config/awg-mesh-tui/config.toml`:

```toml
active_server = "prod"

[servers.prod]
ssh_alias = "my-vps"              # как в ~/.ssh/config или user@host
cli_path = "~/.local/bin/awg-mesh"
container = "amnezia-mesh-awg"
# login_shell, mmdb_path, server_lat/lon, read_only — опционально

# post_mutation_hook = "..."      # generic-хук после мутаций сети
```

## Разработка

```sh
cargo test            # кодек vpn:// + golden-roundtrip против python-фикстур
cargo clippy
cargo run -- --mock   # TUI против canned-транспорта, без сервера
```

План и решения — в [plan.md](plan.md). Референс-CLI и фикстуры —
[amnezia-mesh](https://github.com/alexpozdnyakof/amnezia-mesh) (Python,
canonical) и его Go-порт
[amnezia-mesh-go](https://github.com/alexpozdnyakof/amnezia-mesh-go).
