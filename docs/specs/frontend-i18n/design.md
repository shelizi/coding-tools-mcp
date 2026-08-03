# Frontend i18n Design

## Architecture

Add a small dependency-free i18n module under `src/lib/i18n/`:

- A Svelte locale store drives reactive translations.
- English source messages are stable message IDs and the English fallback.
- JSON catalogs provide `zh-TW`, `zh-CN`, and `ja` translations.
- Placeholder interpolation uses `{name}` tokens.
- A non-reactive `translate()` helper supports utility modules and event handlers.

## Persistence

The active locale is stored under `coding-tools.locale`. Locale values are validated
against the supported locale list before use. The document language is updated when
the store changes.

## UI

`LanguageSelect.svelte` is mounted in the application shell next to the theme toggle.
It uses native language names so the selector remains understandable in any locale.

## Compatibility

No backend schema or Tauri command changes are required. Dynamic values such as user
paths and runtime output are interpolated or rendered unchanged.

