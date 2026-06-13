export class CancelledOperationError extends Error {
  constructor(message = "Operation cancelled") {
    super(message);
    this.name = "CancelledOperationError";
  }
}

export const isCancelledOperationError = (error: unknown): error is CancelledOperationError =>
  error instanceof CancelledOperationError;

export const throwIfAborted = (signal: AbortSignal): void => {
  if (signal.aborted) {
    throw new CancelledOperationError();
  }
};

export const delayWithAbort = (delayMs: number, signal: AbortSignal): Promise<void> => {
  throwIfAborted(signal);
  return new Promise((resolve, reject) => {
    const timeoutId = window.setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, delayMs);

    const onAbort = () => {
      window.clearTimeout(timeoutId);
      signal.removeEventListener("abort", onAbort);
      reject(new CancelledOperationError());
    };

    signal.addEventListener("abort", onAbort, { once: true });
  });
};

export type OwnedOperation<TKey extends string = string> = {
  key: TKey;
  token: symbol;
  signal: AbortSignal;
  isCurrent: () => boolean;
  throwIfCancelled: () => void;
};

type ActiveOperation<TKey extends string> = {
  token: symbol;
  controller: AbortController;
  key: TKey;
};

export const createOperationOwner = <TKey extends string>() => {
  const activeOperations = new Map<TKey, ActiveOperation<TKey>>();

  const cancel = (key: TKey): void => {
    const active = activeOperations.get(key);
    if (!active) return;
    activeOperations.delete(key);
    active.controller.abort();
  };

  const cancelAll = (): void => {
    for (const key of [...activeOperations.keys()]) {
      cancel(key);
    }
  };

  const isCurrent = (operation: Pick<OwnedOperation<TKey>, "key" | "token">): boolean =>
    activeOperations.get(operation.key)?.token === operation.token;

  const hasActive = (key: TKey): boolean => activeOperations.has(key);

  const start = (key: TKey): OwnedOperation<TKey> => {
    cancel(key);
    const controller = new AbortController();
    const token = Symbol(key);
    activeOperations.set(key, { key, token, controller });
    return {
      key,
      token,
      signal: controller.signal,
      isCurrent: () => isCurrent({ key, token }),
      throwIfCancelled: () => {
        if (!isCurrent({ key, token })) {
          throw new CancelledOperationError();
        }
        throwIfAborted(controller.signal);
      },
    };
  };

  const finish = (operation: Pick<OwnedOperation<TKey>, "key" | "token">): void => {
    if (!isCurrent(operation)) return;
    activeOperations.delete(operation.key);
  };

  return {
    start,
    cancel,
    cancelAll,
    isCurrent,
    hasActive,
    finish,
  };
};
