import { useEffect } from "react";
import { useLocation, useNavigate } from "react-router";
import { dehydrate, HydrationBoundary, QueryClient } from "@tanstack/react-query";

import SitesChrome from "@ui/web/frames/SitesChrome";

import { creatorHubHost } from "@ui/creatorhub/explorer-host";

import { isDesktopShell } from "@/data/lib/auth/native-shell";
import { latestPostsQuery } from "@/data/lib/create-landing/blog-posts";
import { latestPosts } from "@/data/lib/create-landing/blog-posts.server";
import { liveScenesQuery } from "@/data/lib/create-landing/live-scenes";
import { sidLoader } from "@/core/lib/telemetry/page-loader";
import CreateLanding from "@/features/create-landing/CreateLanding";

import type { Route } from "./+types/create._index";

export const meta = () => [
  { title: "Create in Decentraland | Decentraland" },
  {
    name: "description",
    content: "Download the Creator Hub to start building immersive scenes and interactive experiences in Decentraland.",
  },
];

/** `/create` is the Creator Hub's marketing landing on the web; the desktop shell and the Explorer's
    framed hub start at the hub home. */
export async function loader({ request, context }: Route.LoaderArgs) {
  const { wrap } = sidLoader(request, context);
  const queryClient = new QueryClient();
  await Promise.all([
    queryClient.prefetchQuery(liveScenesQuery()),
    queryClient.prefetchQuery({ ...latestPostsQuery(), queryFn: () => latestPosts() }),
  ]);
  const dehydratedState = dehydrate(queryClient);
  queryClient.clear();
  return wrap({ dehydratedState });
}

export async function clientLoader(): Promise<null> {
  return null;
}

export default function CreateRoute({ loaderData }: Route.ComponentProps) {
  const navigate = useNavigate();
  const { search, hash } = useLocation();

  useEffect(() => {
    if (isDesktopShell() || creatorHubHost(window)) navigate(`/create/home${search}${hash}`, { replace: true });
  }, [navigate, search, hash]);

  return (
    <HydrationBoundary state={loaderData?.dehydratedState}>
      <SitesChrome active="create" overlayNav>
        <CreateLanding />
      </SitesChrome>
    </HydrationBoundary>
  );
}
