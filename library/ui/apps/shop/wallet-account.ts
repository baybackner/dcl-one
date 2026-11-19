import { useEffect, useState } from "react";

import type { Eip1193Provider } from "../../../web-core/auth/eip1193";
import { subscribeWallet } from "../../../web-core/auth/wallet-events";

export function useWalletAccount(provider: Eip1193Provider | null): string | null {
  const [account, setAccount] = useState<string | null>(null);
  useEffect(() => {
    if (!provider) {
      setAccount(null);
      return;
    }
    let live = true;
    provider.request({ method: "eth_accounts" }).then(
      (list) => live && setAccount((list as string[] | undefined)?.[0]?.toLowerCase() ?? null),
      () => {},
    );
    const off = subscribeWallet(provider, { onAccountsChanged: (next) => setAccount(next) });
    return () => {
      live = false;
      off();
    };
  }, [provider]);
  return account;
}
