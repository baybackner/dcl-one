import { useEffect, useState } from "react";
import { useRevalidator } from "react-router";

import ChMyScenes, { type MySceneVM } from "@ui/creatorhub/pages/ChMyScenes";

import { useAuth } from "@/data/lib/auth/index";
import { openSignIn } from "@/features/components/auth/signin-store";
import { useChromeAuth } from "@ui/web/frames/chrome-auth";
import { discoverDeployedScenes } from "@/data/lib/catalyst/creator-hub/discover-deployed.server";
import type { DiscoveredScene } from "@/data/lib/catalyst/creator-hub/discover-deployed";
import { type PageDefaults } from "@/core/lib/telemetry/page-loader";
import { pageLoaderWith } from "@/core/lib/telemetry/page-loader";
import { readWallet } from "@/data/lib/auth/wallet-cookie";
import { track } from "@/core/lib/telemetry/track";

import { creatorHubMeta } from "@/core/lib/seo/creator-hub-meta";

import type { Route } from "./+types/create.scenes";
import type { StoryId } from "@/core/lib/telemetry/story-id";

export const meta = () => creatorHubMeta("Scenes");

const STORY: StoryId = "create/hub-to-scenes";

const FALLBACK: PageDefaults = {
  variant: "scenes_first",
};

export async function loader({ request, context }: Route.LoaderArgs) {
  const publishedAddress = readWallet(request) || "";
  const { sid, wrap, data: published } = await pageLoaderWith(request, context, FALLBACK, async () =>
    publishedAddress
      ? await discoverDeployedScenes(publishedAddress, { signal: request.signal }).catch((): DiscoveredScene[] => [])
      : [],
  );
  return wrap({ sid, publishedAddress, published });
}

function saveFile(name: string, bytes: Uint8Array): void {
  const url = URL.createObjectURL(new Blob([bytes.slice().buffer as ArrayBuffer], { type: "application/zip" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export default function CreateScenes({ loaderData }: Route.ComponentProps) {
  const { sid, publishedAddress: loaderAddress, published: scenes } = loaderData;
  const revalidator = useRevalidator();
  const { isConnected, address } = useAuth();
  const { name } = useChromeAuth();

  const needsRescope = isConnected && !!address && !loaderAddress;
  useEffect(() => {
    if (needsRescope && revalidator.state === "idle") void revalidator.revalidate();
  }, [needsRescope, revalidator]);

  const [downloadingId, setDownloadingId] = useState<string | null>(null);

  async function onDownload(s: MySceneVM) {
    if (downloadingId) return;
    setDownloadingId(s.entityId);
    try {
      const { deployedSceneZip } = await import("@/data/lib/catalyst/creator-hub/deployed-scene-zip");
      const zip = await deployedSceneZip(s.entityId, s.title);
      if (zip) saveFile(zip.name, zip.bytes);
      track("ch_scenes_clicked", { to: "download-entity-zip", ok: zip !== null, entity: s.entityId }, { sid, story: STORY });
    } finally {
      setDownloadingId(null);
    }
  }

  return (
    <ChMyScenes
      signedIn={isConnected || !!loaderAddress}
      account={loaderAddress || address || ""}
      name={name}
      loading={needsRescope || revalidator.state === "loading"}
      scenes={scenes}
      onSignIn={() => openSignIn()}
      onDownload={(s) => void onDownload(s)}
      downloadingId={downloadingId}
    />
  );
}
