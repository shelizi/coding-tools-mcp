import { invoke } from "@tauri-apps/api/core";
import { confirm, message, open } from "@tauri-apps/plugin-dialog";
import { setBackend } from "./index";
import { createTauriBackend } from "./tauri";
import type { InvokeFn } from "./types";

export function installDesktopBackend(): void {
  setBackend(
    createTauriBackend({
      invoke: ((cmd, args) => invoke(cmd, args)) as InvokeFn,
      dialog: {
        open,
        confirm,
        async message(text, options) {
          await message(text, options);
        },
      },
    }),
  );
}
