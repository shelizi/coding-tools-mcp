import { desktopClick, desktopDisplays, desktopDrag, desktopKey, desktopScreenshot, desktopScroll, desktopType } from '../desktopTools.js';
import type { ToolHandlerMap } from '../toolDispatch/contract.js';

export const desktopToolHandlers = {
  desktop_displays: () => desktopDisplays(),
  desktop_screenshot: ({ args }) => desktopScreenshot(args),
  desktop_click: ({ args }) => desktopClick(args),
  desktop_drag: ({ args }) => desktopDrag(args),
  desktop_scroll: ({ args }) => desktopScroll(args),
  desktop_type: ({ args }) => desktopType(args),
  desktop_key: ({ args }) => desktopKey(args)
} satisfies ToolHandlerMap;
