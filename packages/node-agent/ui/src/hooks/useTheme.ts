import { useEffect, useMemo, useState } from 'react';

export type ThemePreference = 'system' | 'light' | 'dark';

function initialTheme(): ThemePreference {
  try {
    const saved = localStorage.getItem('ctmcp-theme');
    return saved === 'light' || saved === 'dark' ? saved : 'system';
  } catch {
    return 'system';
  }
}

export function useTheme() {
  const [preference, setPreference] = useState<ThemePreference>(initialTheme);
  const media = useMemo(() => window.matchMedia('(prefers-color-scheme: dark)'), []);
  const [systemDark, setSystemDark] = useState(media.matches);

  useEffect(() => {
    const handler = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    if (typeof media.addEventListener === 'function') {
      media.addEventListener('change', handler);
      return () => media.removeEventListener('change', handler);
    }
    const legacyHandler = (event: MediaQueryListEvent) => handler(event);
    media.addListener(legacyHandler);
    return () => media.removeListener(legacyHandler);
  }, [media]);

  const resolved = preference === 'system' ? (systemDark ? 'dark' : 'light') : preference;
  useEffect(() => {
    document.documentElement.dataset.bsTheme = resolved;
    document.documentElement.style.colorScheme = resolved;
    try {
      localStorage.setItem('ctmcp-theme', preference);
    } catch {
      // Theme persistence is optional when browser storage is disabled.
    }
  }, [preference, resolved]);

  return { preference, setPreference, resolved };
}
