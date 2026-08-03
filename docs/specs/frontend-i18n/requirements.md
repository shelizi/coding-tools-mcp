# Frontend i18n Requirements

## Goal

The desktop UI supports runtime language switching with English as the default.

## Supported locales

- `en` — English
- `zh-TW` — Traditional Chinese
- `zh-CN` — Simplified Chinese
- `ja` — Japanese

## Functional requirements

1. A language selector is available from the global application shell.
2. Changing the locale updates visible UI copy without reloading the app.
3. The selected locale persists across app launches in local storage.
4. A fresh install, an invalid saved locale, or a missing saved locale uses English.
5. The root document `lang` attribute follows the active locale.
6. Shared components, settings pages, workspace pages, dialogs, toast messages, and accessibility labels use the active locale.
7. User content, workspace names, paths, URLs, logs, protocol names, and backend error details are not translated.

## Acceptance criteria

- Each supported locale can be selected from the global shell.
- All three non-English catalogs contain the same message IDs.
- English is displayed when no locale preference exists.
- Locale changes are persisted and restored.
- `npm test`, `npm run check`, and `npm run build` pass.

