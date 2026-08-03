import { browser } from "$app/environment";
import { derived, get, writable } from "svelte/store";
import { MESSAGES, type Locale, type MessageKey } from "./catalog";

const STORAGE_KEY = "coding-tools.locale";
export const DEFAULT_LOCALE: Locale = "en";
export const SUPPORTED_LOCALES: readonly Locale[] = ["en", "zh-TW", "zh-CN", "ja"];

export const LOCALE_OPTIONS: ReadonlyArray<{ value: Locale; label: string }> = [
  { value: "en", label: "English" },
  { value: "zh-TW", label: "繁體中文" },
  { value: "zh-CN", label: "简体中文" },
  { value: "ja", label: "日本語" },
];

export function isLocale(value: unknown): value is Locale {
  return typeof value === "string" && SUPPORTED_LOCALES.includes(value as Locale);
}

function initialLocale(): Locale {
  if (!browser) return DEFAULT_LOCALE;
  const saved = localStorage.getItem(STORAGE_KEY);
  return isLocale(saved) ? saved : DEFAULT_LOCALE;
}

function interpolate(message: string, values: Record<string, string | number> = {}): string {
  return message.replace(/\{(\w+)\}/g, (match, name: string) =>
    Object.prototype.hasOwnProperty.call(values, name) ? String(values[name]) : match,
  );
}

function translateFor(
  activeLocale: Locale,
  key: MessageKey,
  values?: Record<string, string | number>,
): string {
  const localeIndex = SUPPORTED_LOCALES.indexOf(activeLocale);
  return interpolate(MESSAGES[key][localeIndex] ?? MESSAGES[key][0], values);
}

export const locale = writable<Locale>(initialLocale());

if (browser) {
  locale.subscribe((activeLocale) => {
    localStorage.setItem(STORAGE_KEY, activeLocale);
    document.documentElement.lang = activeLocale;
  });
}

export const t = derived(
  locale,
  (activeLocale) =>
    (key: MessageKey, values?: Record<string, string | number>): string =>
      translateFor(activeLocale, key, values),
);

export function setLocale(nextLocale: Locale): void {
  locale.set(nextLocale);
}

export function translate(
  key: MessageKey,
  values?: Record<string, string | number>,
): string {
  return translateFor(get(locale), key, values);
}

export type { Locale, MessageKey };

