import {
  auto,
  click,
  computeStats,
  load,
  node,
  outcome,
  route,
  sep,
  state,
  step,
} from "../../flowmap/flowmapdata";
import type { FlowSection } from "../../flowmap/flowmapdata";

export const SECTIONS: FlowSection[] = [
  {
    id: "entry",
    num: "01",
    title: "Entry & Home",
    blurb:
      "The hub shell at /create: rail navigation, the Start-building fast path, the home cards, and sign-in. Every rail door stays in-product.",
    tracks: [
      {
        chain: "home",
        items: [route("/create/home", { sub: "HOME" })],
        branches: [
          {
            chain: "home",
            chips: true,
            note: "the rail \u{2014} Land \u{2192} /shop and Names \u{2192} /marketplace/names stay in-product; Curate appears only for committee wallets",
            items: [
              node("chip", "Home", { href: "/create/home" }),
              node("chip", "Scenes", { href: "/create/scenes" }),
              node("chip", "Collections", { href: "/create/wearables" }),
              node("chip", "Curate", { href: "/create/curate", sub: "committee" }),
              node("chip", "Worlds", { href: "/create/worlds" }),
              node("chip", "Land", { href: "/shop" }),
              node("chip", "Names", { href: "/marketplace/names" }),
              node("chip", "Metrics", { href: "/create/metrics" }),
              node("chip", "Data sources", { href: "/create/data-sources" }),
              node("chip", "Learn", { href: "/create/learn" }),
              node("chip", "\u{2699} Settings", { href: "/create/settings" }),
              node("chip", "Sign in", { title: "opens the sign-in modal \u{2014} see below" }),
            ],
          },
          {
            chain: "start-building",
            items: [click("Start building"), route("/download/creator-hub")],
            note: "the header button and the Scenes card's empty state \u{2014} scenes are built in the desktop app",
          },
          {
            chain: "home-cards",
            chips: true,
            note: "the home cards \u{2014} the Manage card mirrors the rail doors",
            items: [
              node("chip", "Scenes", { href: "/create/scenes", sub: "See All" }),
              node("chip", "Manage", { sub: "Collections \u{B7} Worlds \u{B7} Metrics \u{B7} Curate \u{B7} Land \u{B7} Names" }),
              node("chip", "Learn", { href: "/create/learn" }),
            ],
          },
          {
            chain: "see-all",
            items: [click("See All"), route("/create/scenes"), sep(), route("/create/learn")],
          },
          {
            chain: "sign-in",
            items: [
              click("Sign in"),
              node("modal", "SIGNIN_MODAL"),
              click("OTP / social"),
              outcome("signed-in"),
            ],
          },
        ],
      },
    ],
  },

  {
    id: "scenes",
    num: "02",
    title: "Published Scenes",
    blurb:
      "The scenes your wallet deployed to LAND or a World. Each one's deployed files download as a zip; scenes are built in the desktop app.",
    tracks: [
      {
        chain: "published",
        items: [
          route("/create/scenes", { sub: "scenes deployed by your wallet" }),
          click("Download scene files"),
          load("fetch the deployed files"),
          outcome("zip"),
        ],
      },
    ],
  },

  {
    id: "wearables",
    num: "04",
    title: "Wearables & Collections",
    machines: [
      "wearable-create-collection",
      "wearable-item-editor",
      "wearable-publish-collection",
    ],
    blurb:
      "Collections home, the new-collection wizard, per-item editing, and the publish flow (its Pay step is a disclosed stub).",
    tracks: [
      {
        chain: "collections",
        items: [route("/create/wearables", { sub: "COLLECTIONS home" })],
        branches: [
          {
            chain: "new-collection",
            items: [
              click("New collection"),
              route("/create/wearables/collections/new", {
                sub: "machine: wearable-create-collection",
              }),
              step(),
              state("NAMING", { sub: "\u{23CE} submits \u{B7} \u{201C}third-party?\u{201D} \u{2192} ?type=linked" }),
              step(),
              state("ITEMS", { sub: "dropzone: .zip \u{B7} .glb \u{B7} .gltf \u{B7} .png \u{2014} remove" }),
              step(),
              state("REVIEW"),
              step(),
              state("SUBMITTING", { busy: true }),
              step(),
              state("DONE"),
              step(),
              outcome("detail"),
            ],
          },
          {
            chain: "collection-detail",
            items: [
              click("collection"),
              route("/create/wearables/collections/:id", {
                href: "/create/wearables",
                sub: "tabs: items \u{21C4} activity (?tab)",
                title: "parameterized \u{2014} link opens the collections list",
              }),
            ],
            branches: [
              {
                chain: "item-editor",
                items: [
                  click("item row"),
                  route("/create/wearables/item-editor", { sub: "ITEM-EDITOR" }),
                  step(),
                  state("SELECT"),
                  step(),
                  state("MODEL"),
                  step(),
                  state("CATEGORY"),
                  step(),
                  state("RARITY"),
                  step(),
                  state("PRICE"),
                  load(undefined, { label: "Save" }),
                  outcome("done"),
                ],
              },
              {
                chain: "publish-collection",
                items: [
                  click("Publish"),
                  route("/create/wearables/publish", {
                    sub: "machine: wearable-publish-collection",
                  }),
                  step(),
                  state("SUMMARY"),
                  step(),
                  state("COST"),
                  step(),
                  state("TERMS"),
                  load("disclosed stub", { label: "Pay" }),
                  state("SUBMITTED"),
                ],
              },
            ],
          },
          {
            chain: "single-item",
            items: [
              click("item"),
              route("/create/wearables/items/:id", {
                href: "/create/wearables",
                title: "parameterized \u{2014} link opens the collections list",
              }),
              click("Edit"),
              route("/create/wearables/item-editor?step=model", {
                href: "/create/wearables/item-editor?step=model",
              }),
            ],
          },
        ],
      },
    ],
  },

  {
    id: "worlds",
    num: "05",
    title: "Worlds",
    machines: ["manage-worlds", "worlds-storage", "world-settings", "world-permissions"],
    blurb:
      "Manage worlds, watch storage quota, edit settings (Unpublish is real), and grant permissions (commit writes the real ACL via signed POST /world/<name>/permissions/access).",
    tracks: [
      {
        chain: "worlds-manage",
        items: [route("/create/worlds")],
        branches: [
          {
            chain: "worlds-manage",
            items: [
              click("card"),
              outcome("settings / layout", { href: "/create/world-settings" }),
            ],
          },
          {
            chain: "storage",
            items: [
              click("Your Storage"),
              route("/create/storage", { sub: "storage panel" }),
            ],
          },
        ],
      },
      {
        chain: "storage",
        items: [
          route("/create/storage"),
          click("SELECT world"),
          state("QUOTA PANEL", { sub: "DAO-proposal link" }),
          sep(),
          node("external", "BUY MANA / LAND / NAME"),
        ],
      },
      {
        chain: "world-settings",
        items: [
          route("/create/world-settings", { sub: "machine: world-settings \u{B7} tabs" }),
          load("invoke", { label: "Save" }),
          outcome("saved"),
        ],
        branches: [
          { chain: "world-settings", items: [click("Discard"), node("end", "")] },
          {
            chain: "world-settings",
            items: [load("real", { label: "Unpublish" }), node("end", "")],
          },
        ],
      },
      {
        chain: "world-permissions",
        items: [
          route("/create/admissions", { sub: "machine: world-permissions \u{B7} tabs" }),
          click("invite / add collaborator / password \u{2265}8+2num"),
          state("COMMIT", { busy: true, sub: "signed ACL write" }),
        ],
      },
    ],
  },

  {
    id: "data-sources",
    num: "05b",
    title: "Data sources",
    blurb: "The ledger of endpoints used by the Creator Hub, their availability, and what reads them.",
    tracks: [
      {
        chain: "data-sources",
        items: [
          route("/create/data-sources", { sub: "the source ledger" }),
          load("probe live + sampled rows (4s timeout)"),
          outcome("\u{25CF} live \u{B7} \u{25D0} sampled \u{B7} \u{25D4} snapshot \u{B7} \u{2298} unavailable \u{B7} \u{25A8} not built \u{B7} \u{2715} excluded", {
            sub: "unbuilt and excluded rows are constants and are never probed",
          }),
        ],
      },
    ],
  },

  {
    id: "metrics",
    num: "07",
    title: "Metrics",
    machines: ["metrics"],
    blurb:
      "Creator metrics are scoped to your wallet \u{2014} cards plus a per-scene visits table from the places API.",
    tracks: [
      {
        chain: "metrics",
        items: [route("/create/metrics")],
        branches: [
          { chain: "metrics", items: [auto("signed-out"), state("GATE")] },
          {
            chain: "metrics",
            note: "per-card \u{201C}Not available\u{201D} on failure \u{B7} EmptyState if none",
            items: [
              auto("creator"),
              outcome("cards", { sub: "real: collections \u{B7} on-sale \u{B7} sales \u{B7} visits" }),
              step(),
              outcome("per-scene visits table", { sub: "real rows \u{B7} per-row \u{201C}Not available\u{201D}" }),
            ],
          },
        ],
      },
    ],
  },

  {
    id: "curate",
    num: "08",
    title: "Curation",
    machines: ["curate-committee"],
    blurb:
      "Committee-only review queue: pick a row, decide, back to the queue. The rail item shows only for wallets on the committee.",
    tracks: [
      {
        chain: "curate",
        items: [
          route("/create/curate", { sub: "committee only \u{2014} rail item is membership-gated" }),
          step(),
          state("QUEUE", { sub: "filters ?status ?type ?assignee=me \u{B7} search \u{B7} sort" }),
          click("row"),
          state("REVIEW"),
          click("approve / reject + comment"),
          state("DECIDED"),
          step(),
          outcome("queue"),
        ],
      },
    ],
  },

  {
    id: "learn-meta",
    num: "09",
    title: "Learn, Settings & Meta",
    blurb:
      "Learn links out to videos and docs (plus this map), Settings holds preferences, the download landing ships the desktop app \u{2014} and the map maps itself.",
    tracks: [
      {
        chain: "learn",
        items: [route("/create/learn")],
        branches: [
          {
            chain: "learn",
            chips: true,
            note: "Videos \u{B7} Creator Docs \u{B7} More \u{2014} external tabs, plus one in-product link",
            items: [
              node("external", "Videos"),
              node("external", "Creator Docs"),
              node("external", "Studios"),
              node("chip", "Creator Hub flow map", { href: "/create/map" }),
            ],
          },
        ],
      },
      {
        chain: "settings",
        items: [
          route("/create/settings", { sub: "preferences" }),
          click("Close"),
          outcome("back", { sub: "history back \u{B7} /create fallback" }),
        ],
      },
      {
        chain: "download",
        items: [
          route("/download/creator-hub", { sub: "desktop app landing" }),
          click("Download"),
          outcome("installer", { sub: "per-OS build" }),
        ],
      },
      {
        chain: "map",
        items: [
          route("/create/map", { sub: "this page \u{2014} linked from Learn" }),
          sep(),
          route("/explorer-map", { sub: "sibling \u{2014} the Explorer client, mapped the same way" }),
        ],
      },
    ],
  },
];


export const STATS = computeStats(SECTIONS);


export const ASCII_SOURCE = `# Creator Hub \u{2014} click/state sitemap

Every edge is one user CLICK, or a LOAD\u{23F3} when the transition invokes work >100ms
(uploads, signed writes, file downloads). States in CAPS are
machine states; \`/paths\` are routes. Regenerated against the live route tree.

\`\`\`
/create (HOME)
\u{251C}\u{2500}\u{2500} rail: Home \u{B7} Scenes \u{B7} Collections \u{2503} Curate(committee-gated) \u{B7}
\u{2502}         Worlds \u{B7} Land(/shop) \u{B7} Names(/marketplace/names) \u{B7} Metrics \u{2503} Learn
\u{2502}         (+ \u{2699} Settings \u{B7} Sign in \u{2014} every door stays in-product)
\u{251C}\u{2500}\u{2500} cards: Scenes(See All \u{2192} /create/scenes) \u{B7} Learn
\u{2502}          (+ a Manage card mirroring the rail doors)
\u{2502}
\u{251C}\u{2500}[Start building]\u{2500}\u{25B6} /download/creator-hub (scenes are built in the desktop app)
\u{251C}\u{2500}[See All]\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{25B6} /create/scenes \u{B7} /create/learn
\u{251C}\u{2500}[Sign in]\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{25B6} SIGNIN_MODAL \u{2500}\u{2500}[OTP/social]\u{2500}\u{2500}\u{25B6} signed-in
\u{2502}
\u{251C}\u{2500}\u{2500} /create/scenes \u{2500}\u{2500} [Download scene files]\u{23F3} fetch the deployed files \u{2500}\u{2500}\u{25B6} zip
\u{2502}
\u{251C}\u{2500}\u{2500} /create/wearables (COLLECTIONS)
\u{2502}    \u{251C}\u{2500}[New collection]\u{25B6} /collections/new   (machine: wearable-create-collection)
\u{2502}    \u{2502}   NAMING(\u{23CE} submits; "third-party?"\u{2192}?type=linked) \u{2500}\u{25B6} ITEMS(upload dropzone,
\u{2502}    \u{2502}   .zip/.glb/.gltf/.png, remove) \u{2500}\u{25B6} REVIEW \u{2500}\u{25B6} SUBMITTING\u{23F3} \u{2500}\u{25B6} DONE \u{2500}\u{25B6} detail
\u{2502}    \u{251C}\u{2500}[collection]\u{25B6} /collections/:id  tabs items\u{21C4}activity (?tab)
\u{2502}    \u{2502}   \u{251C}\u{2500}[item row]\u{25B6} ITEM-EDITOR: SELECT\u{25B6}MODEL\u{25B6}CATEGORY\u{25B6}RARITY\u{25B6}PRICE\u{25B6}SAVE\u{23F3}\u{25B6} done
\u{2502}    \u{2502}   \u{2514}\u{2500}[Publish]\u{25B6} PUBLISH: SUMMARY\u{25B6}COST\u{25B6}TERMS\u{25B6}PAY\u{23F3}(disclosed stub)\u{25B6} SUBMITTED
\u{2502}    \u{2514}\u{2500}[item]\u{25B6} /items/:id \u{2500}[Edit]\u{25B6} item-editor?step=model
\u{2502}
\u{251C}\u{2500}\u{2500} WORLDS
\u{2502}    \u{251C}\u{2500} /create/worlds \u{2500}[card]\u{25B6} settings/layout \u{B7} \u{2500}[Your Storage]\u{25B6} storage panel
\u{2502}    \u{251C}\u{2500} storage: SELECT world \u{25B6} QUOTA panel (DAO-proposal link) \u{B7} BUY MANA/LAND/NAME\u{2197}
\u{2502}    \u{251C}\u{2500} world-settings: tabs \u{2500}[Save]\u{23F3} invoke \u{25B6} saved \u{B7} [Discard] \u{B7} [Unpublish]\u{23F3} real
\u{2502}    \u{2514}\u{2500} world-permissions: tabs \u{2500}[invite]/[add collaborator]/[password \u{2265}8+2num]\u{2500}\u{25B6}
\u{2502}                          COMMIT\u{23F3} (signed ACL write)
\u{2502}
\u{251C}\u{2500}\u{2500} /create/metrics   signed-out\u{25B6}GATE \u{B7} creator\u{25B6}cards (real: collections/
\u{2502}    on-sale/sales/visits) + per-scene visits table (real rows from the places
\u{2502}    API; per-row "Not available" on gaps)
\u{2502}
\u{251C}\u{2500}\u{2500} /create/curate (committee; rail item shows per-membership): QUEUE(filters
\u{2502}    ?status ?type ?assignee=me, search, sort) \u{2500}[row]\u{25B6} REVIEW \u{2500}[approve/reject
\u{2502}    +comment]\u{25B6} DECIDED \u{2500}\u{25B6} queue
\u{2502}
\u{251C}\u{2500}\u{2500} /create/learn \u{2500}\u{2500} Videos\u{2197} \u{B7} Creator Docs\u{2197} \u{B7} Studios\u{2197} \u{B7} Creator Hub flow map
\u{2502}    (\u{25B6} /create/map \u{2014} this document)
\u{251C}\u{2500}\u{2500} /create/settings \u{2500}\u{2500} preferences \u{2500}[Close]\u{25B6} back (history \u{B7} /create)
\u{251C}\u{2500}\u{2500} /download/creator-hub \u{2500}\u{2500} [Download]\u{25B6} per-OS installer
\u{2514}\u{2500}\u{2500} /create/map \u{2014} this map \u{B7} sibling: /explorer-map (the Explorer client)
\`\`\`

Legend: \`[x]\` = click edge \u{B7} \`\u{23F3}\` = >100ms invoked load (spinner/status shown) \u{B7}
\`\u{21C4}\` = reversible pair \u{B7} \`\u{2197}\` = external tab. Redirect shims carry no clicks and
are compressed: /create \u{2192} /create \u{B7} /create/about \u{2192} /create \u{B7}
/builder/* \u{2192} in-product equivalents (/shop, /marketplace/names, \u{2026}).
Orphans/gaps (metrics-funnel,
integration-create-entry) are intentionally absent \u{2014} not implemented.
`;
