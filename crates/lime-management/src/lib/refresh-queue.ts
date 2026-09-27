/** One read at a time; overlapping requests merge into one trailing read. */
export function createRefreshQueue(task: (isCurrent: () => boolean) => Promise<void>) {
  let revision = 0;
  let disposed = false;
  let queued = false;
  let inFlight: Promise<void> | null = null;

  function request(): Promise<void> {
    if (disposed) return Promise.resolve();
    queued = true;
    if (inFlight) return inFlight;
    inFlight = Promise.resolve().then(async () => {
      try {
        while (queued && !disposed) {
          queued = false;
          const startedAt = revision;
          await task(() => !disposed && revision === startedAt);
        }
      } finally { inFlight = null; }
    });
    return inFlight;
  }
  return {
    request,
    invalidate() { revision += 1; queued = false; },
    dispose() { disposed = true; queued = false; revision += 1; },
  };
}
