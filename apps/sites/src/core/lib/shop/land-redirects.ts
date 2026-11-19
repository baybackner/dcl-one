import { selectLandChain } from "@shop/land/contracts";
import { landIdentity as resolveLandIdentity, landRedirectTarget as resolveLandRedirect, type LandRedirect } from "@shop/land/redirects";
import { shopChainEnvironment } from "@web-core/shop-chain-env";

export function landIdentity(query: URLSearchParams) {
  return resolveLandIdentity(query, selectLandChain(shopChainEnvironment()));
}

export function landRedirectTarget(kind: LandRedirect, search: string): string {
  return resolveLandRedirect(kind, search, selectLandChain(shopChainEnvironment()));
}
