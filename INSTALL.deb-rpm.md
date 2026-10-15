# Установка на deb/rpm-дистрибутивах без сборки из сырцов вручную

Шаблон: см. предыдущие компоненты. Автор собирает пакеты у себя локально, как сейчас `makepkg -si`.

Опорные платформы — Debian stable и Fedora: `.deb` собираем на Debian (встаёт и на Ubuntu/Mint),
`.rpm` — на Fedora (встаёт и на RHEL/openSUSE). Наоборот не гарантируется.

Тулчейн берётся из `rust-toolchain.toml` (сейчас 1.93) — руками ставить версию не надо.

## Исходники рядом

Обычная сборка требует соседние чек-ауты (path-зависимости Cargo, см. `PKGBUILD`):

```
work/
  wmde-comp/                   <- этот репозиторий (ветка wmde)
  libcosmic/                   <- git clone -b wmde https://github.com/Lin-WMDE/libcosmic.git
  wmde-settings-daemon/        <- git clone -b wmde https://github.com/Lin-WMDE/wmde-settings-daemon.git
  wmde-randr/                  <- git clone -b wmde https://github.com/Lin-WMDE/wmde-randr.git
  wmde-dbus-settings-bindings/ <- git clone -b wmde https://github.com/Lin-WMDE/wmde-dbus-settings-bindings.git
```

Внутри `libcosmic` лежат git-сабмодули (`iced` ветки `wmde`, `cosmic-icons`).
Официальный URL сабмодуля `iced` — SSH (`git@github.com:`), без ключей не клонируется.
Обход без ключей:

```sh
git config --global url."https://github.com/".insteadOf "git@github.com:"
git -C libcosmic submodule update --init
```

## Собрать

```sh
# зависимости сборки: rustc (из rust-toolchain.toml) cargo just clang lld pkgconf wayland-protocols + nfpm
# плюс -dev пакеты под весь рантайм smithay (drm/gbm/egl/input/seat/udev, см. PKGBUILD)
cargo install --locked nfpm  # или бинарник с github.com/goreleaser/nfpm/releases

just deb   # -> target/wmde-comp_*.deb
just rpm   # -> target/wmde-comp-*.rpm
```

Версия — `1.10.0.<commits>.g<sha>`, как `pkgver()` в `PKGBUILD`.
По умолчанию сборка под чистый `x86-64`. Опционально под `x86-64-v3` (как в Arch-репозитории):

```sh
WMDE_CPU=x86-64-v3 just deb
```

Релизный профиль (`lto=fat`, `codegen-units=1`) оставлен как в апстриме:
линк долгий и однопоточный, зато бинарь как у автора. `panic=abort` здесь
намеренно нет (композитор и есть сессия — abort положит весь десктоп).

## Офлайн-сборка (без сети к git-зависимостям)

`vendor.tar` в git не лежит (см. `.gitignore`), генерим на машине с сетью один раз
(соседние чек-ауты для резолва нужны):

```sh
just vendor          # -> vendor.tar (реестр + git-крейты)
just deb-vendored    # сборка --frozen --offline -> target/*.deb
just rpm-vendored    # аналогично -> target/*.rpm
```

На машину без сети привозим `vendor.tar` файлом. Соседние чек-ауты тоже нужны:
path-зависимости вендором не покрываются, Cargo читает их с диска.

## Поставить

```sh
# Debian/Ubuntu/Mint
sudo apt install ./target/wmde-comp_*.deb

# Fedora/RHEL/openSUSE
sudo dnf install ./target/wmde-comp-*.rpm
```

Рантайм-зависимости подтянутся сами (см. `packaging/nfpm.yaml`).
`xwayland` и `wmde-session` — мягкие (suggests): без первого нет X11-приложений,
без второй нет полной обвязки сессии. Wayland-сессию, target'ы и хелпер
ставит пакет `wmde-session`, здесь только бинарь, юнит и дефолтные схемы.
