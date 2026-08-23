import { setBackend } from "./index";
import { createNodeBackend } from "./node";

export function installHostBackend(): void {
  setBackend(createNodeBackend());
}
