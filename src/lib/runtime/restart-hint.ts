import { message } from "@tauri-apps/plugin-dialog";
import { translate } from "$lib/i18n";

export async function promptServiceRestart(
  serviceRunning: boolean,
  serviceLabel: string,
): Promise<void> {
  if (!serviceRunning) return;
  await message(translate("Configuration saved. Stop and restart {service} for the changes to take effect.", {
    service: serviceLabel,
  }), {
    title: translate("Service restart required"),
    kind: "info",
  });
}
