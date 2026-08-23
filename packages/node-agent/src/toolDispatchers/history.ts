import { bootstrapHistory, checkpointHistory, validateHistory } from '../history.js';
import type { ToolHandlerMap } from '../toolDispatch/contract.js';

export const historyToolHandlers = {
  history_session_bootstrap: ({ ctx, key, historyArgs }) => bootstrapHistory(ctx, key, historyArgs),
  history_session_checkpoint: ({ ctx, key, historyArgs }) => checkpointHistory(ctx, key, historyArgs),
  history_session_validate: ({ ctx, key, args }) => validateHistory(ctx, key, args)
} satisfies ToolHandlerMap;
