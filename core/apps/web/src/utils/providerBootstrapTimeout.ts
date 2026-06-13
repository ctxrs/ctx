export const PROVIDER_BOOTSTRAP_TIMEOUT_MS = 15_000;

export const getProviderBootstrapTimeoutMessage = (
  timeoutMs: number = PROVIDER_BOOTSTRAP_TIMEOUT_MS,
): string => `Provider bootstrap timed out after ${Math.ceil(timeoutMs / 1000)}s.`;

// Bound provider bootstrap waits so setup/workbench recover with an explicit error
// instead of sitting in a loading state forever when the daemon request stalls.
export const withProviderBootstrapTimeout = async <T,>(
  promise: Promise<T>,
  timeoutMs: number = PROVIDER_BOOTSTRAP_TIMEOUT_MS,
): Promise<T> => {
  let timeoutHandle: ReturnType<typeof setTimeout> | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<T>((_, reject) => {
        timeoutHandle = setTimeout(() => {
          reject(new Error(getProviderBootstrapTimeoutMessage(timeoutMs)));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timeoutHandle !== null) {
      clearTimeout(timeoutHandle);
    }
  }
};
