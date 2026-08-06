function abortError(signal?: AbortSignal): Error {
  const reason = signal?.reason;
  if (reason instanceof Error) return reason;
  const error = new Error('admission queue wait aborted');
  error.name = 'AbortError';
  return error;
}

export class Semaphore {
  readonly limit: number;
  #active = 0;
  #waiters: Array<(release: () => void) => void> = [];

  constructor(limit: number) {
    this.limit = Math.max(1, Math.floor(limit));
  }

  get active(): number { return this.#active; }
  get queued(): number { return this.#waiters.length; }

  async acquire(timeoutMs = 30_000, signal?: AbortSignal): Promise<() => void> {
    if (signal?.aborted) throw abortError(signal);
    if (this.#active < this.limit) return this.#grant();
    return new Promise<() => void>((resolve, reject) => {
      let settled = false;
      let timer: NodeJS.Timeout;
      const removeWaiter = () => {
        const index = this.#waiters.indexOf(waiter);
        if (index >= 0) this.#waiters.splice(index, 1);
      };
      const cleanup = () => {
        clearTimeout(timer);
        signal?.removeEventListener('abort', onAbort);
      };
      const onAbort = () => {
        if (settled) return;
        settled = true;
        removeWaiter();
        cleanup();
        reject(abortError(signal));
      };
      const waiter = (release: () => void) => {
        if (settled) { release(); return; }
        settled = true;
        cleanup();
        resolve(release);
      };
      timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        removeWaiter();
        cleanup();
        reject(new Error('admission queue exceeded timeout'));
      }, timeoutMs);
      timer.unref();
      this.#waiters.push(waiter);
      signal?.addEventListener('abort', onAbort, { once: true });
    });
  }

  #grant(): () => void {
    this.#active += 1;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.#active -= 1;
      const next = this.#waiters.shift();
      if (next) next(this.#grant());
    };
  }
}
export class KeyedMutex {
  #tails = new Map<string, Promise<void>>();

  async acquire(keys: string[]): Promise<() => void> {
    const normalized = [...new Set(keys.filter(Boolean))].sort();
    const releases: Array<() => void> = [];
    for (const key of normalized) releases.push(await this.#acquireOne(key));
    return () => { for (const release of releases.reverse()) release(); };
  }

  async #acquireOne(key: string): Promise<() => void> {
    const previous = this.#tails.get(key) ?? Promise.resolve();
    let resolveCurrent!: () => void;
    const current = new Promise<void>(resolve => { resolveCurrent = resolve; });
    const tail = previous.then(() => current);
    this.#tails.set(key, tail);
    await previous;
    let released = false;
    return () => {
      if (released) return;
      released = true;
      resolveCurrent();
      void tail.finally(() => { if (this.#tails.get(key) === tail) this.#tails.delete(key); });
    };
  }
}

export class AsyncQueue<T> {
  #values: T[] = [];
  #waiters: Array<{ resolve: (value: T) => void; reject: (error: Error) => void }> = [];
  #closed?: Error;

  push(value: T): void {
    if (this.#closed) return;
    const waiter = this.#waiters.shift();
    if (waiter) waiter.resolve(value); else this.#values.push(value);
  }

  close(error = new Error('queue closed')): void {
    if (this.#closed) return;
    this.#closed = error;
    for (const waiter of this.#waiters.splice(0)) waiter.reject(error);
  }

  async shift(timeoutMs = 0, signal?: AbortSignal): Promise<T> {
    const value = this.#values.shift();
    if (value !== undefined) return value;
    if (this.#closed) throw this.#closed;
    return new Promise<T>((resolve, reject) => {
      let settled = false;
      let timer: NodeJS.Timeout | undefined;
      const waiter: { resolve: (value: T) => void; reject: (error: Error) => void } = {
        resolve: () => undefined,
        reject: () => undefined
      };
      const removeWaiter = () => {
        const index = this.#waiters.indexOf(waiter);
        if (index >= 0) this.#waiters.splice(index, 1);
      };
      const cleanup = () => {
        if (timer) clearTimeout(timer);
        signal?.removeEventListener('abort', onAbort);
      };
      const finishResolve = (next: T) => {
        if (settled) return;
        settled = true;
        cleanup();
        resolve(next);
      };
      const finishReject = (error: Error) => {
        if (settled) return;
        settled = true;
        cleanup();
        reject(error);
      };
      const onAbort = () => {
        removeWaiter();
        const reason = signal?.reason;
        const error = reason instanceof Error ? reason : new Error('queue wait aborted');
        if (!(reason instanceof Error)) error.name = 'AbortError';
        finishReject(error);
      };
      waiter.resolve = finishResolve;
      waiter.reject = finishReject;
      this.#waiters.push(waiter);
      if (timeoutMs > 0) {
        timer = setTimeout(() => {
          removeWaiter();
          finishReject(new Error('queue wait timed out'));
        }, timeoutMs);
        timer.unref();
      }
      if (signal?.aborted) onAbort();
      else signal?.addEventListener('abort', onAbort, { once: true });
    });
  }
}

export const sleep = (ms: number): Promise<void> => new Promise(resolve => setTimeout(resolve, ms));
