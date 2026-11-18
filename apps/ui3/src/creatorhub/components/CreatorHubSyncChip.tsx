import { useEffect, useState, useSyncExternalStore } from "react";

import { SyncIndicatorView } from "./SyncIndicator";
import { getSharedSyncEngine } from "@web-core/creator-hub-sync";

type Props = { className?: string; onReview?: () => void };

export default function CreatorHubSyncChip({ className, onReview }: Props) {
  const [mounted, setMounted] = useState(false);
  useEffect(() => setMounted(true), []);

  const engine = getSharedSyncEngine();
  const summary = useSyncExternalStore(engine.subscribe, engine.getSummary, engine.getSummary);

  if (!mounted || summary.scenes.length === 0) return null;

  return <SyncIndicatorView summary={summary} className={className} onReview={onReview} />;
}
