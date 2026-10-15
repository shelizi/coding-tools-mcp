// One-time OAuth passwords rotate in the backend after each successful authorization.
// Secret views re-read them when the app regains attention and periodically while visible,
// so the displayed password never stays on an already-consumed value.
export const SECRET_REFRESH_INTERVAL_MS = 10_000;

export function watchRotatingSecrets(
  refresh: () => void,
  intervalMs = SECRET_REFRESH_INTERVAL_MS,
): () => void {
  if (typeof window === "undefined" || typeof document === "undefined") return () => {};
  const refreshIfVisible = () => {
    if (document.visibilityState === "visible") refresh();
  };
  window.addEventListener("focus", refreshIfVisible);
  document.addEventListener("visibilitychange", refreshIfVisible);
  const timer = window.setInterval(refreshIfVisible, intervalMs);
  return () => {
    window.removeEventListener("focus", refreshIfVisible);
    document.removeEventListener("visibilitychange", refreshIfVisible);
    window.clearInterval(timer);
  };
}
