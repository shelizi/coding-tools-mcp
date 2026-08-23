import { base } from "$app/paths";

/** Prefix a root-relative app path with SvelteKit `paths.base`. */
export function withAppBase(path: string, appBase: string): string {
  const suffix = path.startsWith("/") ? path : `/${path}`;
  if (!appBase) return suffix;
  if (suffix === "/") return `${appBase}/`;
  return `${appBase}${suffix}`;
}

/** Strip SvelteKit `paths.base` so route checks stay origin-independent. */
export function withoutAppBase(pathname: string, appBase: string): string {
  if (!appBase) return pathname;
  if (pathname === appBase || pathname === `${appBase}/`) return "/";
  if (pathname.startsWith(`${appBase}/`)) return pathname.slice(appBase.length);
  return pathname;
}

export function appUrl(path: string): string {
  return withAppBase(path, base);
}

export function routePath(pathname: string): string {
  return withoutAppBase(pathname, base);
}
