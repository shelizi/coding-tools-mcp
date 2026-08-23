import type { IncomingMessage, ServerResponse } from 'node:http';
import { ProcessRequestLifecycle } from '../../processes.js';
import type { ToolContext } from '../../types.js';

export class McpToolCallLifecycle {
  readonly process: ProcessRequestLifecycle;
  readonly #req: IncomingMessage;
  readonly #res: ServerResponse;
  readonly #abortRequest: () => void;
  readonly #closeResponse: () => void;
  #disposed = false;

  constructor(context: ToolContext, req: IncomingMessage, res: ServerResponse) {
    this.process = new ProcessRequestLifecycle(context);
    this.#req = req;
    this.#res = res;
    this.#abortRequest = () => this.process.abort();
    this.#closeResponse = () => {
      if (!this.#res.writableEnded) this.process.abort();
    };
    this.#req.once('aborted', this.#abortRequest);
    this.#res.once('close', this.#closeResponse);
  }

  complete(): void {
    this.process.complete();
  }

  abort(): void {
    this.process.abort();
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.#req.off('aborted', this.#abortRequest);
    this.#res.off('close', this.#closeResponse);
  }
}
