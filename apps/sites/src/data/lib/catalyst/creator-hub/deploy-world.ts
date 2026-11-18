import { getJSON, worldsBase } from "../client";
import type { GetOptions } from "../client";
import { StatusResponseSchema } from "../generated-schemas/worlds";
import { worldsApiPath } from "../typed";
import { ETH_ADDRESS_RE } from "../format/address";

function normalizeAddress(addr: string | null | undefined): string {
  return (addr ?? "").trim().toLowerCase();
}

const WORLD_SUFFIX = ".dcl.eth";

export function personalWorldName(
  address: string | null | undefined,
): string | null {
  const addr = normalizeAddress(address);
  return ETH_ADDRESS_RE.test(addr) ? `${addr}${WORLD_SUFFIX}` : null;
}

type WorldsRealm = {
  online: boolean;
  personalWorlds: boolean;
};

const realmByRequest = new WeakMap<AbortSignal, Promise<WorldsRealm>>();

export function fetchWorldsRealm(opts: GetOptions = {}): Promise<WorldsRealm> {
  const signal = opts.signal;
  if (!signal || opts.fetchImpl || opts.base) return fetchWorldsRealmLive(opts);
  const memo = realmByRequest.get(signal);
  if (memo) return memo;
  const p = fetchWorldsRealmLive(opts).then(
    (realm) => realm,
    (err: unknown) => {
      realmByRequest.delete(signal);
      throw err;
    },
  );
  realmByRequest.set(signal, p);
  return p;
}

async function fetchWorldsRealmLive(opts: GetOptions): Promise<WorldsRealm> {
  const raw = await getJSON<unknown>(worldsApiPath("get", "/status"), {
    ...opts,
    base: worldsBase(),
  });
  const parsed = StatusResponseSchema.safeParse(raw);
  return {
    online: true,
    personalWorlds: parsed.success && parsed.data.personalWorlds != null,
  };
}
