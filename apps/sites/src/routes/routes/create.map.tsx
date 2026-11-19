import { Link } from "react-router";

import ChFlowMapPage from "@dcl/one-ui/apps/sites/flow-map/ChFlowMapPage";
import { CREATOR_HUB_HOME } from "@web-core/creator-hub-host";

import { creatorHubMeta } from "@/core/lib/seo/creator-hub-meta";

export const meta = () => creatorHubMeta("Flow map");

export default function CreatorHubMapRoute() {
  return <ChFlowMapPage LinkComponent={Link} homeHref={CREATOR_HUB_HOME} />;
}
