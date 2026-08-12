# RustConn 0.20 — Dependency Upgrade Plan

Інструкція для підготовки релізу 0.20 з підняттям основних залежностей.

## Передумови

- 0.19.x серія стабільна, всі known issues виправлені або задокументовані
- GNOME 50 Flatpak runtime (`org.gnome.Platform//50`) доступний на Flathub
- ironrdp 0.18+ або нова major-версія опублікована на crates.io

---

## Фаза 1: GTK/Adwaita стек

### 1.1 Libadwaita baseline: v1_5 → v1_6

Підняти мінімальний feature з `v1_5` на `v1_6` в workspace Cargo.toml:

```toml
libadwaita = { version = "0.9", features = ["v1_6"] }
```

**Що це дає:**
- `AdwSpinner` — заміна `gtk4::Spinner` (прибрати `#[cfg(feature = "adw-1-6")]` guard з sidebar)
- CSS variables — стабільні кастомні кольори
- Accent colors API — системний акцент у dark/light

**Міграція:**
1. Видалити `#[cfg(feature = "adw-1-6")]` / `#[cfg(not(feature = "adw-1-6"))]` fallback для Spinner в sidebar та інших місцях
2. Замінити всі `gtk4::Spinner` на `adw::Spinner`
3. Перевірити: `cargo check --all-targets`

**Flatpak:** потребує `org.gnome.Platform//48` або вище (вже задоволено runtime 50).
**Snap:** `gnome-46-2404` extension має libadwaita 1.5 — snap build НЕ отримає adw-1-6 baseline. Залишити `adw-1-6` feature для snap, або дочекатись core26 extension.

### 1.2 Нові feature flags: adw-1-7, adw-1-8, adw-1-9

Залишити як opt-in features (не baseline):

```toml
# rustconn/Cargo.toml [features]
adw-1-7 = ["adw-1-6", "libadwaita/v1_7"]
adw-1-8 = ["adw-1-7", "libadwaita/v1_8"]
adw-1-9 = ["adw-1-8", "libadwaita/v1_9"]
```

| Feature | Використання | Пріоритет |
|---------|-------------|-----------|
| `adw-1-7` | `AdwToggleGroup` для sidebar протокол-фільтрів (замість linked buttons) | Середній |
| `adw-1-8` | `AdwShortcutsDialog` замість deprecated `GtkShortcutsWindow` | Низький |
| `adw-1-9` | `AdwSidebar` — повна заміна кастомного sidebar (великий рефакторинг) | Опціональний для 0.20 |

### 1.3 VTE: feature v0_76 → v0_78

```toml
# rustconn/Cargo.toml
vte4 = { workspace = true, features = ["v0_78"] }
```

**Що це дає (VTE 0.78, GNOME 48):**
- Покращений OSC 52 clipboard (copy/paste через escape)
- Shell integration API enhancements
- Кращий fractional scaling support

**Опціонально — v0_80 feature flag:**
```toml
# rustconn/Cargo.toml [features]
vte-0-80 = []  # gate нових API під цей flag
```

**Flatpak:** runtime 50 має VTE 0.82+ — будь-яка версія працює.
**Snap:** залежить від VTE в Ubuntu archive для core24.

---

## Фаза 2: IronRDP

### 2.1 Моніторинг ironrdp 0.18

Перед 0.20 перевірити на [crates.io](https://crates.io/crates/ironrdp) та [GitHub](https://github.com/Devolutions/IronRDP/releases):

```bash
cargo search ironrdp --limit 5
```

**Що шукати в 0.18:**
- [ ] `ClientDriveNotifyChangeDirectoryResponse` — розблокує directory change notifications
- [ ] AVC444 decode support в `ironrdp-egfx` — розблокує повний GFX pipeline
- [ ] RAIL / RemoteApp channel handshake — розблокує RemoteApp support
- [ ] Audio Input (MS-RDPEAI) channel — розблокує мікрофон

**Якщо 0.18 вийшов:**

1. Оновити всі ironrdp-* залежності в `rustconn-core/Cargo.toml`
2. Прибрати/оновити коментарі `# ON UPGRADE (0.17 → next)`
3. Якщо з'явився `ClientDriveNotifyChangeDirectoryResponse`:
   - Активувати `poll_directory_changes()` в `rdpdr.rs`
   - Прибрати `#[allow(dead_code)]` з `build_file_notify_info`
4. Якщо з'явився AVC444:
   - Додати `CapabilitySet::V10_7` в `gfx_handler::capabilities()`
   - Видалити тест `advertised_capabilities_exclude_avc444`
5. `cargo test -p rustconn-core --features rdp-embedded,gfx-h264`

### 2.2 Пов'язані crates (ironrdp ecosystem)

Ці crates мають RC-versions locked в Cargo.lock через ironrdp:

| Crate | Locked RC | Stable available |
|-------|-----------|-----------------|
| picky | 7.0.0-rc.25 | 7.0.0-rc.26 |
| curve25519-dalek | 5.0.0-rc.1 | 5.0.0 |
| ed25519-dalek | 3.0.0-rc.1 | 3.0.0 |
| ecdsa | 0.17.0-rc.22 | 0.17.0 |
| p256, p384, p521 | 0.14.0-rc.14 | 0.14.0 |
| x25519-dalek | 3.0.0-rc.1 | 3.0.0 |

Ці оновляться автоматично коли ironrdp підніме свої deps. **Не форсити вручну.**

---

## Фаза 3: Інші significant bumps

### 3.1 reqwest 0.13 → 0.14 (якщо вийде)

Поточна: 0.13.4. Стежити за:
- Видалення legacy reqwest 0.12 з дерева залежностей (зараз дублюється через ironrdp-tokio)
- Breaking changes в API

### 3.2 oo7 0.6 → 0.7 (Secret Service client)

Якщо вийде 0.7:
- Перевірити API сумісність `Keyring::new()` / `lookup()` / `create_item()`
- Перевірити чи з'явились нові features (portal backend, unlock prompt API)

### 3.3 notify 8.2 → 9.x (filesystem watcher)

Якщо вийде notify 9:
- Можливі breaking changes в `Event` enum
- Перевірити `rustconn-core/src/rdp_client/rdpdr.rs` dir_watcher
- Перевірити `rustconn-core/src/sync/watcher.rs`

### 3.4 thiserror 2.x → 3.x (якщо вийде)

Малоймовірно, але якщо буде — масовий рефакторинг усіх error types.

---

## Фаза 4: Flatpak / Runtime

### 4.1 GNOME Runtime

Файл: `packaging/flatpak/io.github.totoshko88.RustConn.yml`

Перевірити:
```bash
flatpak remote-info --show-runtime flathub org.gnome.Platform//51
```

Якщо runtime 51 доступний — оновити:
```yaml
runtime-version: '51'
```

Також оновити `packaging/flathub/io.github.totoshko88.RustConn.yml`.

### 4.2 FreeRDP bundled module

Перевірити latest на https://pub.freerdp.com/releases/ :
```bash
curl -s https://api.github.com/repos/FreeRDP/FreeRDP/releases/latest | jq -r '.tag_name'
```

Оновити URL + sha256 в обох flatpak manifests.

### 4.3 Cargo sources regeneration

Після будь-якого оновлення Cargo.lock:
```bash
cd packaging/flatpak
python3 flatpak-cargo-generator.py ../../Cargo.lock -o cargo-sources.json
cp cargo-sources.json ../flathub/cargo-sources.json
```

---

## Фаза 5: Великі рефакторинги (опціонально для 0.20)

### 5.1 AdwSidebar замість кастомного ConnectionSidebar

**Передумова:** libadwaita 1.9 (`adw-1-9` feature).

`AdwSidebar` надає:
- Sections (замінює manual group rendering)
- Tooltips (вбудовані)
- Context menus (вбудовані)
- Drop target (DnD вбудований)

**Scope:**
1. Створити `rustconn/src/sidebar_adw/` з новою реалізацією
2. Зберегти `rustconn/src/sidebar/` як fallback за `#[cfg(not(feature = "adw-1-9"))]`
3. Trait `SidebarInterface` для спільного API
4. Поступова міграція — sidebar widget за feature flag

**Ризик:** Високий (sidebar — 1914 LOC, 53 members). Можна відкласти на 0.21.

### 5.2 TerminalNotebook split

God class з 109 members. Кандидати на виділення:
- `TabManager` — створення/закриття/переміщення табів
- `SessionLifecycle` — connect/disconnect/reconnect
- `TabContextMenu` — контекстне меню

**Scope:** 3-4 нових файли, trait extraction. Не потребує зовнішніх залежностей.

---

## Чеклист перед 0.20

```
[ ] ironrdp latest перевірено, оновлено якщо є нова версія
[ ] libadwaita baseline піднято до v1_6
[ ] VTE feature піднято до v0_78
[ ] cargo update виконано
[ ] cargo clippy --all-targets — 0 warnings
[ ] cargo test --workspace — pass
[ ] cargo test -p rustconn-core --features rdp-embedded,gfx-h264 — pass
[ ] ./scripts/check-cli-versions.sh — exit 0
[ ] Flatpak runtime перевірено
[ ] FreeRDP version перевірено
[ ] cargo-sources.json regenerated
[ ] CHANGELOG.md заповнений
[ ] Packaging changelogs propagated
[ ] Snap compatibility перевірена (feature flags OK для gnome-46-2404)
[ ] cargo build --release — success
[ ] release.sh --dry-run — pass
```

---

## Порядок виконання

1. Створити бранч `0.20.0`
2. Bump version → `0.20.0`
3. **Спершу:** cargo update + перевірка
4. **Потім:** libadwaita v1_6 baseline + VTE v0_78 + Spinner міграція
5. **Потім:** ironrdp оновлення (якщо є)
6. **Потім:** Flatpak runtime + FreeRDP + cargo-sources
7. **Потім:** Optional рефакторинги (AdwSidebar, TerminalNotebook split)
8. **Фіналізація:** CHANGELOG, packaging, quality checks
9. `./scripts/release.sh`

---

## Rollback план

Якщо major bump ламає build/tests:

1. `git stash` поточних змін
2. Повернути версію залежності в Cargo.toml
3. `cargo update` щоб regenerate lockfile
4. Перевірити що попередній стан компілюється
5. Відкрити issue з описом breakage для upstream

Ніколи не форсити incompatible version через `cargo update -p crate@version` без повної перевірки тестами.
