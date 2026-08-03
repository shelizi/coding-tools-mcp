import { showToast } from "$lib/stores/toast";
import { translate } from "$lib/i18n";
import type { RuntimeStatus } from "$lib/types";

export function isPortConflictError(error: unknown): boolean {
  const text = error instanceof Error ? error.message : String(error);
  return (
    text.includes("已被占用") ||
    text.includes("未能成功启动") ||
    text.includes("上一次服务占用")
  );
}

export function serviceErrorMessage(status: RuntimeStatus): string {
  return status.localMessage || status.publicMessage || translate("The service failed to start");
}

export async function runServiceToggle(
  running: boolean,
  start: () => Promise<RuntimeStatus>,
  stop: () => Promise<RuntimeStatus>,
  serviceLabel = translate("Service"),
): Promise<RuntimeStatus | null> {
  try {
    return running ? await stop() : await start();
  } catch (error) {
    const text = error instanceof Error ? error.message : String(error);
    showToast(text, {
      title: translate(running ? "{service} failed to stop" : "{service} failed to start", {
        service: serviceLabel,
      }),
      kind: "error",
      duration: 8000,
    });
    return null;
  }
}

export function notifyStartFailure(
  serviceLabel: string,
  status: RuntimeStatus,
): void {
  showToast(serviceErrorMessage(status), {
    title: translate("{service} failed to start", { service: serviceLabel }),
    kind: "error",
    duration: 8000,
  });
}
