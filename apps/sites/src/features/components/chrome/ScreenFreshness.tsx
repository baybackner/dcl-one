import { useEffect } from "react";
import { useRevalidator } from "react-router";
import FreshnessNotice from "@dcl/one-ui/apps/sites/shared/FreshnessNotice";
import type { PublicFreshness } from "@web-core/freshness/section";

export default function ScreenFreshness({ freshness }: { freshness?: PublicFreshness }) {
  const { revalidate, state } = useRevalidator();
  useEffect(() => {
    if (!freshness?.refreshing || state !== "idle") return;
    const timer = setTimeout(() => void revalidate(), 1_000);
    return () => clearTimeout(timer);
  }, [freshness, state, revalidate]);
  return <FreshnessNotice failed={freshness?.refreshFailed} onRetry={() => void revalidate()} />;
}
