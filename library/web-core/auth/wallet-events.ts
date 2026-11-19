import type { Eip1193Provider } from "./eip1193";

export type WalletEvents = {
  onAccountsChanged?(account: string | null): void;
  onChainChanged?(): void;
};

/** Follows the wallet's own events; returns the unsubscribe. */
export function subscribeWallet(provider: Eip1193Provider | null, events: WalletEvents): () => void {
  if (!provider?.on || !provider.removeListener) return () => {};
  const accounts = (...args: unknown[]) => {
    const list = (args[0] as string[] | undefined) ?? [];
    events.onAccountsChanged?.(list[0]?.toLowerCase() ?? null);
  };
  const chain = () => events.onChainChanged?.();
  provider.on("accountsChanged", accounts);
  provider.on("chainChanged", chain);
  return () => {
    provider.removeListener?.("accountsChanged", accounts);
    provider.removeListener?.("chainChanged", chain);
  };
}
