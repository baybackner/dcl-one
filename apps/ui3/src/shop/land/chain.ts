// The browser host's choice of the Ethereum network the Shop's LAND and NAME flows run on. The networks themselves,
// and everything read from them, take the chain as an argument (@shop/land/contracts).

import type { ChainId } from "@shop/model/chain";
import { selectLandChain } from "@shop/land/contracts";
import { shopChainEnvironment } from "@web-core/shop-chain-env";

/**
 * The Ethereum network the Shop's LAND and NAME flows run on: the host's choice, else the Shop's own
 * (`ethereumChainId`: VITE_SHOP_ETHEREUM_CHAIN_ID, or Sepolia when the Shop settles on Amoy, mainnet otherwise).
 */
export function landChainId(): ChainId {
  return selectLandChain(shopChainEnvironment());
}
