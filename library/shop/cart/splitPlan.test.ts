import { describe, expect, it } from "vitest";

import { chargeForQuote, manaFromWhole, usdCents, type ManaWei } from "../model/money";
import type { CartLine, PurchaseOffer } from "../model/purchase";
import type { MissingFact, PaymentMethod, Quote } from "../model/settlement";
import { creatorOrder, ITEM, parcelResale, storeMint, web2, web3 } from "../payment/fixtures";
import { lineUnits, type PricedLine } from "./partition";
import { planCartSplit, type CartSplitResult } from "./splitPlan";

const s = web3();
let nextItem = ITEM;

type Terms = { cents: number | null; mana?: number | null; splittable?: boolean; available?: number | null };

/** A store mint (Polygon, one Credits group for all of them) with the given quotes; null means that method is out. */
function mint({ cents, mana = 1, splittable = true, available = null }: Terms): PurchaseOffer {
  const base = storeMint(s, "wearable", nextItem++);
  const quotes: Quote[] = [];
  const unavailable: { method: PaymentMethod; missing: MissingFact }[] = [];
  if (cents === null) unavailable.push({ method: "credits", missing: "manaUsdRate" });
  else quotes.push({ method: "credits", quoted: usdCents(cents) });
  if (mana === null) unavailable.push({ method: "mana", missing: "manaUsdRate" });
  else quotes.push({ method: "mana", amount: manaFromWhole(mana) });
  return { ...base, quotes, unavailable, available, splittable };
}

function line(id: string, offer: PurchaseOffer | null, quantity = 1, method: PaymentMethod | null = null): PricedLine {
  const asset = offer?.asset ?? storeMint(s).asset;
  return {
    line: {
      id,
      asset,
      quantity,
      method,
      listing: {},
      display: { name: id },
      provenance: {},
      seen: null,
      addedAt: 0,
    } satisfies CartLine,
    offer,
  };
}

const plan = (cart: PricedLine[], credits: number, mana?: ManaWei | null): CartSplitResult =>
  planCartSplit(cart, s, { creditsBalance: usdCents(credits), manaBalance: mana });

function planned(result: CartSplitResult) {
  if (!result.ok) throw new Error(`no plan: ${JSON.stringify(result.refusal)}`);
  return result.plan;
}

describe("planCartSplit: a cart's Credits and MANA parts", () => {
  it("charges each unit up to a whole credit: two units quoted 11 cents cost 40, which 30 does not cover", () => {
    const cart = [line("pair", mint({ cents: 11 }), 2), line("one", mint({ cents: 20, splittable: false }))];
    expect(planned(plan(cart, 30))).toEqual({ credits: ["one"], split: null, mana: ["pair"], creditsCents: 20 });
    expect(planned(plan(cart, 40)), "40 covers the pair, and nothing is left for the next line").toEqual({
      credits: ["pair"],
      split: null,
      mana: ["one"],
      creditsCents: 40,
    });
  });

  it("a line only MANA can pay goes to MANA, however many Credits are left", () => {
    const cart = [line("manaOnly", mint({ cents: null })), line("parcel", parcelResale(s)), line("credits", mint({ cents: 100 }))];
    expect(planned(plan(cart, 10_000))).toEqual({ credits: ["credits"], split: null, mana: ["manaOnly", "parcel"], creditsCents: 100 });
  });

  it("a line neither method can pay leaves the cart without a split", () => {
    const ok = line("ok", mint({ cents: 100 }));
    expect(plan([ok, line("gone", null)], 50)).toEqual({ ok: false, refusal: { reason: "noMethod", lineId: "gone" } });
    expect(plan([ok, line("priceless", mint({ cents: null, mana: null }))], 50)).toEqual({
      ok: false,
      refusal: { reason: "noMethod", lineId: "priceless" },
    });
    expect(plan([ok, line("soldOut", mint({ cents: 100, available: 1 }), 2)], 50), "not enough stock for its units").toEqual({
      ok: false,
      refusal: { reason: "noMethod", lineId: "soldOut" },
    });
  });

  it("covers the lines only Credits can pay first, or offers no split", () => {
    const cart = [line("either", mint({ cents: 100 })), line("creditsOnly", mint({ cents: 120, mana: null }))];
    expect(planned(plan(cart, 150)), "the Credits-only line first, though it comes second").toEqual({
      credits: ["creditsOnly"],
      split: { lineId: "either", legCents: 30, priceCents: 100 },
      mana: [],
      creditsCents: 150,
    });
    expect(plan(cart, 110)).toEqual({ ok: false, refusal: { reason: "creditsShort", required: 120, balance: 110 } });
  });

  it("a line of several units that does not fit goes to MANA whole, never split by units, and the walk goes on", () => {
    const cart = [line("first", mint({ cents: 100 })), line("three", mint({ cents: 115 }), 3), line("after", mint({ cents: 90 }))];
    expect(planned(plan(cart, 200))).toEqual({ credits: ["first", "after"], split: null, mana: ["three"], creditsCents: 190 });
  });

  it("a line that does not fit and that the node does not split goes to MANA, and the walk goes on", () => {
    const cart = [line("big", mint({ cents: 500, splittable: false })), line("a", mint({ cents: 115 })), line("b", mint({ cents: 201 }))];
    expect(planned(plan(cart, 150))).toEqual({
      credits: ["a"],
      split: { lineId: "b", legCents: 30, priceCents: 201 },
      mana: ["big"],
      creditsCents: 150,
    });
    const usdPegged = [line("pegged", creatorOrder(s)), line("store", storeMint(s))];
    expect(planned(plan(usdPegged, 50)), "a USD-pegged listing is never split").toEqual({
      credits: [],
      split: { lineId: "store", legCents: 50, priceCents: 115 },
      mana: ["pegged"],
      creditsCents: 50,
    });
  });

  it("the first line that does not fit and can be split takes the whole credits left, and MANA pays every line after it", () => {
    const cart = [
      line("a", mint({ cents: 115 })),
      line("b", mint({ cents: 201 })),
      line("c", mint({ cents: 115 })),
      line("d", mint({ cents: 20, mana: null })),
      line("e", mint({ cents: 5 })),
    ];
    // d (Credits only) is covered first: 257 - 20 leaves 237. a costs 120 (117 left); b's 210 does not fit, so it takes 110.
    expect(planned(plan(cart, 257))).toEqual({
      credits: ["a", "d"],
      split: { lineId: "b", legCents: 110, priceCents: 201 },
      mana: ["c", "e"],
      creditsCents: 250,
    });
  });

  it("splits only for a Credits part above nothing and below the line's own quote", () => {
    const one = (cents: number) => [line("one", mint({ cents }))];
    expect(plan(one(115), 9), "less than a whole credit left").toEqual({ ok: false, refusal: { reason: "noCreditsPart" } });
    expect(plan(one(115), 120), "enough for the whole line").toEqual({ ok: false, refusal: { reason: "noManaPart" } });
    expect(planned(plan(one(115), 117)), "115 rounds up to 120, so 117 does not cover it: 110 of it").toEqual({
      credits: [],
      split: { lineId: "one", legCents: 110, priceCents: 115 },
      mana: [],
      creditsCents: 110,
    });
    expect(planned(plan(one(111), 119)).split, "against the quote, not its rounded-up charge").toEqual({
      lineId: "one",
      legCents: 110,
      priceCents: 111,
    });
  });

  it("Credits alone or MANA alone is no split: the cart pays as it does today", () => {
    const cart = [line("a", mint({ cents: 100 })), line("b", mint({ cents: 200 }))];
    expect(plan(cart, 300)).toEqual({ ok: false, refusal: { reason: "noManaPart" } });
    expect(plan(cart, 0)).toEqual({ ok: false, refusal: { reason: "noCreditsPart" } });
    expect(plan([line("m", mint({ cents: null })), line("p", parcelResale(s))], 10_000)).toEqual({
      ok: false,
      refusal: { reason: "noCreditsPart" },
    });
    expect(planCartSplit(cart, web2(), { creditsBalance: usdCents(300) }), "no MANA without a web3 wallet").toEqual({
      ok: false,
      refusal: { reason: "noManaPart" },
    });
  });

  it("keeps every Credits group within 30 units, naming the lines where they sit in the cart", () => {
    const cart = [line("m", mint({ cents: null })), line("a", mint({ cents: 10 }), 16), line("b", mint({ cents: 10 }), 16)];
    const result = plan(cart, 10_000);
    expect(result.ok).toBe(false);
    if (result.ok || result.refusal.reason !== "refused") throw new Error("expected refused lines");
    expect(result.refusal.refusals.map((r) => [r.lineId, r.index, r.code])).toEqual([
      ["a", 1, "groupTooLarge"],
      ["b", 2, "groupTooLarge"],
    ]);
    expect(planned(plan([...cart.slice(0, 2), line("b", mint({ cents: 10 }), 14)], 10_000)).credits, "30 units fit").toEqual(["a", "b"]);
  });

  it("places every line once with all its units, and takes no more Credits than the balance", () => {
    const quotes = [7, 10, 11, 20, 95, 115, 201, 499];
    for (let seed = 0; seed < 400; seed++) {
      const cart = Array.from({ length: 1 + (seed % 6) }, (_, i) => {
        const pick = (seed * 7 + i * 13) % 97;
        const cents = pick % 11 === 0 ? null : quotes[pick % quotes.length]!;
        const mana = pick % 7 === 0 && cents !== null ? null : 1 + (pick % 3);
        return line(`l${i}`, mint({ cents, mana, splittable: pick % 5 !== 0 }), 1 + (pick % 4 === 0 ? pick % 3 : 0));
      });
      const balance = (seed * 37) % 900;
      const result = plan(cart, balance);
      if (!result.ok) continue;
      const { credits, split, mana, creditsCents } = result.plan;
      const placed = [...credits, ...(split ? [split.lineId] : []), ...mana];
      expect(placed.slice().sort()).toEqual(cart.map((p) => p.line.id).sort());
      expect(credits.length > 0 || split !== null).toBe(true);
      expect(mana.length > 0 || split !== null).toBe(true);
      const byId = new Map(cart.map((p) => [p.line.id, p]));
      if (split) {
        const entry = byId.get(split.lineId)!;
        expect(lineUnits(entry.line)).toBe(1);
        expect(split.legCents % 10).toBe(0);
        expect(split.legCents > 0 && split.legCents < split.priceCents).toBe(true);
      }
      const charged = credits.reduce((sum, id) => {
        const entry = byId.get(id)!;
        const quote = entry.offer!.quotes.find((q) => q.method === "credits")!;
        return sum + chargeForQuote(quote.method === "credits" ? quote.quoted : usdCents(0))! * lineUnits(entry.line);
      }, 0);
      expect(creditsCents).toBe(charged + (split?.legCents ?? 0));
      expect(creditsCents).toBeLessThanOrEqual(balance);
    }
  });

  it("decides each line's method itself, whatever method the line was added with", () => {
    const cart = [line("fits", mint({ cents: 100 }), 1, "mana"), line("manaOnly", mint({ cents: null }), 1, "credits")];
    expect(planned(plan(cart, 100))).toEqual({ credits: ["fits"], split: null, mana: ["manaOnly"], creditsCents: 100 });
  });

  it("checks the MANA lines against the wallet, leaving the split line's MANA to the host", () => {
    const cart = [line("k", mint({ cents: 100 })), line("m", mint({ cents: 300, mana: 2 }), 2), line("n", mint({ cents: null, mana: 1 }))];
    expect(planned(plan(cart, 100, manaFromWhole(5))).mana).toEqual(["m", "n"]);
    expect(plan(cart, 100, manaFromWhole(4))).toEqual({
      ok: false,
      refusal: { reason: "manaShort", required: manaFromWhole(5), balance: manaFromWhole(4) },
    });
    expect(planned(plan([line("one", mint({ cents: 115 }))], 50, manaFromWhole(0))).split?.legCents).toBe(50);
  });

  it("an empty cart or a line listed twice has no split", () => {
    expect(plan([], 100)).toEqual({ ok: false, refusal: { reason: "empty" } });
    const twice = mint({ cents: 100 });
    expect(plan([line("x", twice), line("x", twice)], 50)).toEqual({ ok: false, refusal: { reason: "duplicateLine", lineId: "x" } });
  });
});
