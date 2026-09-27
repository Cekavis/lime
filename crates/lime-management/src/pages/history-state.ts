import type { HistoryPage, InputData } from "@/api/types";

export const INPUT_TEST_COMPLETED_EVENT = "lime:input-test-completed";

export function signalInputTestCompleted() {
  window.dispatchEvent(new Event(INPUT_TEST_COMPLETED_EVENT));
}

export function sortHistory(page: HistoryPage): HistoryPage {
  const items = page.items.map((item, index) => ({ item, index }));
  items.sort((left, right) => {
    if (left.item.timestampMs !== null && right.item.timestampMs !== null && left.item.timestampMs !== right.item.timestampMs) {
      return right.item.timestampMs - left.item.timestampMs;
    }
    // Older services may omit timestamps. Preserve their order instead of
    // treating the compatibility request id as a chronological identifier.
    return left.index - right.index;
  });
  return { ...page, items: items.map(({ item }) => item) };
}

export function keyedHistoryEntries(items: InputData[]) {
  const occurrences = new Map<string, number>();
  return items.map((entry) => {
    const identity = entry.timestampMs !== null
      ? `timestamp:${entry.timestampMs}`
      : entry.requestId !== undefined
        ? `request:${entry.requestId}`
        : `content:${JSON.stringify([entry.precedingText, entry.preedit, entry.model, entry.rimeCandidates, entry.finalCandidates])}`;
    const occurrence = occurrences.get(identity) ?? 0;
    occurrences.set(identity, occurrence + 1);
    return { entry, key: `${identity}:${occurrence}` };
  });
}

interface HistoryObserver {
  onChange: () => void;
  onError: (error: unknown) => void;
}

export function createHistoryWatcher(waitForRevision: (revision: number) => Promise<number>, retryMs = 1000) {
  const observers = new Set<HistoryObserver>();
  let revision = 0;
  let task: Promise<void> | null = null;
  let cancelRetry: (() => void) | null = null;

  async function watch() {
    while (observers.size > 0) {
      try {
        const nextRevision = await waitForRevision(revision);
        if (!Number.isFinite(nextRevision)) throw new Error("历史更新通知无效");
        if (nextRevision !== revision) {
          revision = nextRevision;
          for (const observer of observers) observer.onChange();
        }
      } catch (error) {
        for (const observer of observers) observer.onError(error);
        if (observers.size === 0) return;
        await new Promise<void>((resolve) => {
          const timer = setTimeout(() => { cancelRetry = null; resolve(); }, retryMs);
          cancelRetry = () => { clearTimeout(timer); cancelRetry = null; resolve(); };
        });
      }
    }
  }

  function start() {
    if (task) return;
    task = watch().finally(() => {
      task = null;
      if (observers.size > 0) start();
    });
  }

  return (observer: HistoryObserver) => {
    observers.add(observer);
    start();
    return () => {
      observers.delete(observer);
      if (observers.size === 0) cancelRetry?.();
      // Tauri commands cannot be aborted. Keep the in-flight task until it
      // settles so a remount reuses it instead of opening a second native wait.
    };
  };
}
