type ShopChainWindow = Window & {
  __SHOP_CHAIN__?: { settlementChainId?: number; ethereumChainId?: number };
  __SHOP_ETHEREUM_CHAIN_ID__?: number;
};

export function parseShopChainId(value: string | undefined): number | undefined {
  const id = Number(value);
  return Number.isSafeInteger(id) && id > 0 ? id : undefined;
}

export function shopChainEnvironment() {
  const host = typeof window !== "undefined" ? (window as ShopChainWindow) : undefined;
  return {
    settlementChainId: host?.__SHOP_CHAIN__?.settlementChainId ?? parseShopChainId(import.meta.env?.VITE_SHOP_CHAIN_ID),
    ethereumChainId: host?.__SHOP_CHAIN__?.ethereumChainId ?? parseShopChainId(import.meta.env?.VITE_SHOP_ETHEREUM_CHAIN_ID),
    landChainId: host?.__SHOP_ETHEREUM_CHAIN_ID__,
  };
}
