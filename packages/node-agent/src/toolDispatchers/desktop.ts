import { desktopDisplays, desktopScreenshot } from '../desktopTools.js';
import { desktopInput } from '../desktopInput.js';
import type { ToolHandlerMap } from '../toolDispatch/contract.js';

export const desktopToolHandlers = {
  desktop_displays: () => desktopDisplays(),
  desktop_screenshot: ({ args }) => desktopScreenshot(args),
  desktop_input: ({ args }) => desktopInput(args)
} satisfies ToolHandlerMap;
