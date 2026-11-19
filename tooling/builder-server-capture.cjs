'use strict'

/**
 * Records the pinned builder-server's answers as conformance fixtures.
 * Usage: node tooling/builder-server-capture.cjs <upstream-checkout> <output-directory>
 * with CATALYST_TEST_DATABASE_URL naming a disposable PostgreSQL cluster.
 * See services/catalyst/tests/fixtures/conformance/builder/README.md.
 */

const fs = require('node:fs')
const os = require('node:os')
const path = require('node:path')
const http = require('node:http')
const crypto = require('node:crypto')
const { execFileSync } = require('node:child_process')
const { BUILDER_REVISION: REVISION, assertPinnedSource, sourceCapture, requireDatabaseUrl } = require('./social-capture-common.cjs')

const REPOSITORY = 'github.com/decentraland/builder-server'
const CLOCK = '2026-01-01T00:00:00.000Z'
const ROOT = path.resolve(__dirname, '..')
const FIXTURES = path.join(ROOT, 'services/catalyst/tests/fixtures/conformance/builder')
const GUARD = path.join(__dirname, 'builder-server-offline.cjs')
const KEPT_RESPONSE_HEADERS = ['content-type', 'cache-control', 'location']
const FAKED_GRAPH_FIELDS = new Set(['accounts', 'collections', 'items', 'thirdParties'])
const SEED_KEYS = ['wallets', 'third_parties', 'registry', 'collections', 'items', 'chain']
const FAKED_WHERE_KEYS = new Set(['id', 'id_in', 'id_gt', 'creator', 'managers_contains', 'minters_contains', 'managers_contains_nocase', 'collection', 'isCommitteeMember'])
const VOLATILE = '<<VOLATILE>>'
const MOVED_ITEM_FILE = 'bafkreigh2akiscaildcqabsyg3dfr6chu3fgpregiymsck7e7aqa4s52zy'

/** A client-shaped wearable `item` body for `id`, in `collectionId` (null: no collection). */
function movedItem(id, collectionId) {
  return {
    item: {
      id,
      name: 'Conformance moved',
      description: '',
      eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
      collection_id: collectionId,
      type: 'wearable',
      rarity: 'common',
      price: '0',
      thumbnail: 'thumbnail.png',
      contents: { 'model.glb': MOVED_ITEM_FILE, 'thumbnail.png': MOVED_ITEM_FILE },
      metrics: { meshes: 1, bodies: 1, materials: 1, textures: 1, triangles: 1, entities: 1 },
      data: {
        category: 'hat',
        tags: [],
        replaces: [],
        hides: [],
        representations: [{
          bodyShapes: ['urn:decentraland:off-chain:base-avatars:BaseMale'],
          mainFile: 'model.glb',
          contents: ['model.glb'],
          overrideReplaces: [],
          overrideHides: []
        }]
      }
    }
  }
}
const OWNER_INITIALIZE = '0xd14fee77000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001400000000000000000000000000000000000000000000000000000000000000180000000000000000000000000c1aba94a79ca07d9d15a62ff5c9b382777e0c70e0000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000017113b44fdd661a156cc01b5031e3acf72c32eb300000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000011436f6e666f726d616e6365207361766564000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e44434c2d434e46524d4e43535644000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004668747470733a2f2f706565722e646563656e7472616c616e642e6f72672f6c616d626461732f636f6c6c656374696f6e732f7374616e646172642f6572633732312f3133372f00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'
const STRANGER_INITIALIZE = '0xd14fee77000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000001400000000000000000000000000000000000000000000000000000000000000180000000000000000000000000cd6884d4258edc833f31ac773e24affe6a7d31540000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000000000000000000000000000017113b44fdd661a156cc01b5031e3acf72c32eb300000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000011436f6e666f726d616e6365207361766564000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000e44434c2d434e46524d4e43535644000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000004668747470733a2f2f706565722e646563656e7472616c616e642e6f72672f6c616d626461732f636f6c6c656374696f6e732f7374616e646172642f6572633732312f3133372f00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'
const report = process.stderr.write.bind(process.stderr)
let activeDrop

const CASES = [
  {
    name: '01-collections-public-search',
    description: 'GET /collections unsigned, searching published collections',
    method: 'GET',
    path: '/v1/collections',
    query: { q: 'conformance', is_published: 'true' }
  },
  {
    name: '02-collection-owner',
    description: 'GET /collections/{id} signed by the owner of a draft',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner'
  },
  {
    name: '03-collection-unsigned',
    description: 'GET /collections/{id} without a signature',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001'
  },
  {
    name: '04-collection-stranger',
    description: 'GET /collections/{id} signed by a wallet that holds no role on the draft',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'stranger'
  },
  {
    name: '05-collection-create',
    description: 'PUT /collections/{id} signed by the owner, creating a draft',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000002',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000002',
        name: 'Conformance new draft',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: '0x00'
    },
    volatile: ['data.created_at', 'data.updated_at']
  },
  {
    name: '06-committee-unsigned',
    description: 'GET /committee without a signature',
    method: 'GET',
    path: '/v1/committee'
  },
  {
    name: '07-collection-malformed-chain',
    description: 'GET /collections/{id} with an auth chain header that is not JSON',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    headers: { 'x-identity-auth-chain-0': '{not json' }
  },
  {
    name: '08-collection-expired-signature',
    description: 'GET /collections/{id} signed by the owner 40 minutes before the request',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    sign: { offset: -40 * 60 * 1000 }
  },
  {
    name: '09-collection-future-signature',
    description: 'GET /collections/{id} signed by the owner 40 minutes after the request',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    sign: { offset: 40 * 60 * 1000 }
  },
  {
    name: '10-collection-signature-for-another-path',
    description: 'GET /collections/{id} signed by the owner over another collection path',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    sign: { path: '/v1/collections/c011ec71-0000-4000-8000-000000000009' }
  },
  {
    name: '11-collection-metadata-mismatch',
    description: 'GET /collections/{id} signed by the owner over other metadata than the request sends',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    sign: { sentMetadata: '{"origin":"https://builder.example.org"}' }
  },
  {
    name: '12-collection-scene-signer',
    description: 'GET /collections/{id} signed by the owner with a scene signer in the metadata',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    metadata: '{"signer":"decentraland-kernel-scene"}'
  },
  {
    name: '13-collection-non-canonical-signer',
    description: 'GET /collections/{id} signed by the owner with a signer key that is not canonical',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    metadata: '{"Signer":"dcl:explorer"}'
  },
  {
    name: '14-collection-untimestamped-read',
    description: 'GET /collections/{id} signed by the owner over method and path alone, without timestamp or metadata headers',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    sign: { untimestamped: true }
  },
  {
    name: '15-collection-untimestamped-write',
    description: 'PUT /collections/{id} signed by the owner over method and path alone, without timestamp or metadata headers',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000003',
    signer: 'owner',
    sign: { untimestamped: true },
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000003',
        name: 'Conformance untimestamped draft',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: '0x00'
    },
    volatile: ['data.created_at', 'data.updated_at']
  },
  {
    name: '16-collection-invalid-body',
    description: 'PUT /collections/{id} signed by the owner with a collection whose name is not a string',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000004',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000004',
        name: 5,
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: '0x00'
    }
  },
  {
    name: '17-collection-signature-for-another-method',
    description: 'GET /collections/{id} signed by the owner as a PUT',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    sign: { method: 'PUT' }
  },
  {
    name: '18-collection-untimestamped-with-headers',
    description: 'GET /collections/{id} signed by the owner over method and path alone, sent with timestamp and metadata headers',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    sign: { untimestamped: true, withHeaders: true }
  },
  {
    name: '19-collection-unsigned-invalid-body',
    description: 'PUT /collections/{id} without a signature and with a collection whose name is not a string',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000005',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000005',
        name: 5,
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: '0x00'
    }
  },
  {
    name: '20-collection-published-unapproved-owner',
    description: 'GET /collections/{id} signed by the owner of a collection on chain and not approved',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000006',
    signer: 'owner'
  },
  {
    name: '21-collection-published-approved-owner',
    description: 'GET /collections/{id} signed by the owner of a collection on chain and approved',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000007',
    signer: 'owner'
  },
  {
    name: '22-collection-committee',
    description: 'GET /collections/{id} signed by a committee member who holds no other role on the draft',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'committee'
  },
  {
    name: '23-collection-published-stranger',
    description: 'GET /collections/{id} signed by a wallet that holds no role on a collection on chain',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000006',
    signer: 'stranger'
  },
  {
    name: '24-collection-published-manager',
    description: 'GET /collections/{id} signed by an on-chain manager of the collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000006',
    signer: 'manager'
  },
  {
    name: '25-collection-save-stranger',
    description: 'PUT /collections/{id} signed by a wallet that holds no role on the draft',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'stranger',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000001',
        name: 'Conformance draft',
        eth_address: '0xcd6884d4258edc833f31ac773e24affe6a7d3154',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: '0x00'
    }
  },
  {
    name: '26-collection-save-committee',
    description: 'PUT /collections/{id} signed by a committee member who holds no other role on the draft',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'committee',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000001',
        name: 'Conformance draft',
        eth_address: '0x01eba3f0e992f53debaf51ed3738dc2f23e09aee',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: '0x00'
    }
  },
  {
    name: '27-collection-save-published-manager',
    description: 'PUT /collections/{id} signed by an on-chain manager of a collection on chain',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000006',
    signer: 'manager',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000006',
        name: 'Chain unapproved',
        eth_address: '0x1257bf35fafebbfc08ea47903e1f7c36ecda48c0',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: '0x00'
    }
  },
  {
    name: '28-collection-items-unsigned',
    description: 'GET /collections/{id}/items without a signature',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001/items'
  },
  {
    name: '29-collection-items-stranger',
    description: 'GET /collections/{id}/items signed by a wallet that holds no role on the draft',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001/items',
    signer: 'stranger'
  },
  {
    name: '30-collection-items-committee',
    description: 'GET /collections/{id}/items signed by a committee member who holds no other role on the draft',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001/items',
    signer: 'committee'
  },
  {
    name: '31-collection-items-published-stranger',
    description: 'GET /collections/{id}/items signed by a wallet that holds no role on a collection on chain',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000006/items',
    signer: 'stranger'
  },
  {
    name: '32-collection-items-published-manager',
    description: 'GET /collections/{id}/items signed by an on-chain manager of the collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000006/items',
    signer: 'manager'
  },
  {
    name: '33-item-orphan-into-third-party',
    description: 'PUT /items/{id} signed by the owner moving its item with no collection into a third-party collection it manages',
    method: 'PUT',
    path: '/v1/items/c011ec71-0000-4000-8000-0000000000a1',
    signer: 'owner',
    body: movedItem('c011ec71-0000-4000-8000-0000000000a1', 'c011ec71-0000-4000-8000-000000000008')
  },
  {
    name: '34-item-out-of-third-party',
    description: 'PUT /items/{id} signed by the owner taking its item out of a third-party collection it manages',
    method: 'PUT',
    path: '/v1/items/c011ec71-0000-4000-8000-0000000000a2',
    signer: 'owner',
    body: movedItem('c011ec71-0000-4000-8000-0000000000a2', null)
  },
  {
    name: '35-item-between-third-party',
    description: 'PUT /items/{id} signed by the owner moving its item from one third-party collection it manages to another',
    method: 'PUT',
    path: '/v1/items/c011ec71-0000-4000-8000-0000000000a3',
    signer: 'owner',
    body: movedItem('c011ec71-0000-4000-8000-0000000000a3', 'c011ec71-0000-4000-8000-000000000008')
  },
  {
    name: '36-item-published-stranger',
    description: 'GET /items/{id} signed by a wallet that holds no role on a minted item of a collection on chain',
    method: 'GET',
    path: '/v1/items/c011ec71-0000-4000-8000-0000000000b1',
    signer: 'stranger'
  },
  {
    name: '37-item-published-owner',
    description: 'GET /items/{id} signed by the owner of a minted item of a collection on chain',
    method: 'GET',
    path: '/v1/items/c011ec71-0000-4000-8000-0000000000b1',
    signer: 'owner'
  },
  {
    name: '38-item-published-committee',
    description: 'GET /items/{id} signed by a committee member who holds no other role on a minted item of a collection on chain',
    method: 'GET',
    path: '/v1/items/c011ec71-0000-4000-8000-0000000000b1',
    signer: 'committee'
  },
  {
    name: '39-collections-committee-search',
    description: 'GET /collections signed by a committee member, searching listing collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing'
    },
    signer: 'committee'
  },
  {
    name: '40-collections-committee-page',
    description: 'GET /collections signed by a committee member, paginated, first page of two',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      page: '1',
      limit: '2'
    },
    signer: 'committee'
  },
  {
    name: '41-collections-committee-name-desc',
    description: 'GET /collections signed by a committee member, sorted by name descending',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      sort: 'NAME_DESC'
    },
    signer: 'committee'
  },
  {
    name: '42-collections-committee-most-relevant',
    description: 'GET /collections signed by a committee member, sorted by relevance',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      sort: 'MOST_RELEVANT'
    },
    signer: 'committee'
  },
  {
    name: '43-collections-committee-last-activity',
    description: 'GET /collections signed by a committee member, sorted by last activity, newest first',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      sort: 'LAST_ACTIVITY_DESC'
    },
    signer: 'committee'
  },
  {
    name: '44-collections-committee-requested-at',
    description: 'GET /collections signed by a committee member, sorted by request date, oldest first',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      sort: 'REQUESTED_AT_ASC'
    },
    signer: 'committee'
  },
  {
    name: '45-collections-committee-to-review',
    description: 'GET /collections signed by a committee member, filtered to collections to review',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      status: 'to_review'
    },
    signer: 'committee'
  },
  {
    name: '46-collections-committee-statuses',
    description: 'GET /collections signed by a committee member, filtered to disabled or approved collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      status: [
        'disabled',
        'approved'
      ]
    },
    signer: 'committee'
  },
  {
    name: '47-collections-committee-pending',
    description: 'GET /collections signed by a committee member, filtered to pending collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      status: 'pending'
    },
    signer: 'committee'
  },
  {
    name: '48-collections-committee-updates',
    description: 'GET /collections signed by a committee member, filtered to reviews of updates',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      review_type: 'updates'
    },
    signer: 'committee'
  },
  {
    name: '49-collections-committee-unassigned',
    description: 'GET /collections signed by a committee member, filtered to unassigned standard collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      assignee: 'unassigned',
      type: 'standard'
    },
    signer: 'committee'
  },
  {
    name: '50-collections-committee-published',
    description: 'GET /collections signed by a committee member, filtered to published collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      is_published: 'true'
    },
    signer: 'committee'
  },
  {
    name: '51-collections-committee-unpublished',
    description: 'GET /collections signed by a committee member, filtered to unpublished collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      is_published: 'false'
    },
    signer: 'committee'
  },
  {
    name: '52-collections-committee-tag',
    description: 'GET /collections signed by a committee member, filtered by an item tag in another case',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      tag: 'CONFORMANCE'
    },
    signer: 'committee'
  },
  {
    name: '53-collections-committee-empty-page',
    description: 'GET /collections signed by a committee member, paginated with no match',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'nothing-matches',
      page: '1',
      limit: '5'
    },
    signer: 'committee'
  },
  {
    name: '54-collections-committee-invalid-status',
    description: 'GET /collections signed by a committee member, with an unknown status',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      status: 'nope'
    },
    signer: 'committee'
  },
  {
    name: '55-collections-committee-two-review-types',
    description: 'GET /collections signed by a committee member, with two review types',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      review_type: [
        'new',
        'updates'
      ]
    },
    signer: 'committee'
  },
  {
    name: '56-collections-committee-invalid-assignee',
    description: 'GET /collections signed by a committee member, with an assignee that is not an address',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      assignee: 'not-an-address'
    },
    signer: 'committee'
  },
  {
    name: '57-collections-public-search-listing',
    description: 'GET /collections unsigned, searching published listing collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      is_published: 'true'
    }
  },
  {
    name: '58-collections-public-search-approved',
    description: 'GET /collections unsigned, as the marketplace searches approved collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      is_published: 'true',
      status: 'approved',
      limit: '5'
    }
  },
  {
    name: '59-collections-public-search-committee',
    description: 'GET /collections signed by a committee member, searching published collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      is_published: 'true'
    },
    signer: 'committee'
  },
  {
    name: '60-collections-stranger',
    description: 'GET /collections signed by a wallet that is not on the committee, without a search',
    method: 'GET',
    path: '/v1/collections',
    signer: 'stranger'
  },
  {
    name: '61-collections-unsigned',
    description: 'GET /collections without a signature or a search',
    method: 'GET',
    path: '/v1/collections'
  },
  {
    name: '62-address-collections-creator',
    description: 'GET /{address}/collections signed by that address, every collection it may list',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    signer: 'creator'
  },
  {
    name: '63-address-collections-page',
    description: 'GET /{address}/collections signed by that address, paginated, first page of two',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      page: '1',
      limit: '2'
    },
    signer: 'creator'
  },
  {
    name: '64-address-collections-draft',
    description: 'GET /{address}/collections signed by that address, filtered to drafts',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      status: 'draft'
    },
    signer: 'creator'
  },
  {
    name: '65-address-collections-published',
    description: 'GET /{address}/collections signed by that address, filtered to published collections',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      status: 'published'
    },
    signer: 'creator'
  },
  {
    name: '66-address-collections-disabled',
    description: 'GET /{address}/collections signed by that address, filtered to disabled collections',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      status: 'disabled'
    },
    signer: 'creator'
  },
  {
    name: '67-address-collections-unpublished',
    description: 'GET /{address}/collections signed by that address, filtered to unpublished collections',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      is_published: 'false'
    },
    signer: 'creator'
  },
  {
    name: '68-address-collections-third-party',
    description: 'GET /{address}/collections signed by that address, filtered to third-party collections',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      type: 'third_party'
    },
    signer: 'creator'
  },
  {
    name: '69-address-collections-contract',
    description: 'GET /{address}/collections signed by that address, filtered by a contract address',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      contract_address: '0x5eed000000000000000000000000000000000103'
    },
    signer: 'creator'
  },
  {
    name: '70-address-collections-search',
    description: 'GET /{address}/collections signed by that address, searching by name, sorted by name',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      q: 'pending',
      sort: 'NAME_ASC'
    },
    signer: 'creator'
  },
  {
    name: '71-address-collections-manager',
    description: 'GET /{address}/collections signed by that address, an on-chain manager',
    method: 'GET',
    path: '/v1/0x1257bf35fafebbfc08ea47903e1f7c36ecda48c0/collections',
    signer: 'manager'
  },
  {
    name: '72-address-collections-minter',
    description: 'GET /{address}/collections signed by that address, an on-chain minter',
    method: 'GET',
    path: '/v1/0xc1eba15006e9d7f629bcad7a7dd41285f012b949/collections',
    signer: 'minter'
  },
  {
    name: '73-address-collections-new-creator',
    description: 'GET /{address}/collections signed by that address, the chain creator of a collection another wallet stored',
    method: 'GET',
    path: '/v1/0xcd6884d4258edc833f31ac773e24affe6a7d3154/collections',
    signer: 'stranger'
  },
  {
    name: '74-address-collections-other',
    description: 'GET /{address}/collections signed by another wallet',
    method: 'GET',
    path: '/v1/0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e/collections',
    signer: 'creator'
  },
  {
    name: '75-address-collections-invalid-status',
    description: 'GET /{address}/collections signed by that address, with an unknown status',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      status: 'bogus'
    },
    signer: 'creator'
  },
  {
    name: '76-address-collections-invalid-contract',
    description: 'GET /{address}/collections signed by that address, with a contract address that is not an address',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      contract_address: 'nope'
    },
    signer: 'creator'
  },
  {
    name: '77-address-collections-unsigned',
    description: 'GET /{address}/collections without a signature',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections'
  },
  {
    name: '78-collection-minter',
    description: 'GET /collections/{id} signed by an on-chain minter of an approved collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000103',
    signer: 'minter'
  },
  {
    name: '79-collection-published-committee',
    description: 'GET /collections/{id} signed by a committee member who holds no other role on a collection on chain',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000102',
    signer: 'committee'
  },
  {
    name: '80-collection-third-party-manager',
    description: 'GET /collections/{id} signed by a manager of the provider of the third-party collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000105',
    signer: 'creator'
  },
  {
    name: '81-collection-third-party-stranger',
    description: 'GET /collections/{id} signed by a wallet that manages no provider of the third-party collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000105',
    signer: 'stranger'
  },
  {
    name: '82-collection-missing',
    description: 'GET /collections/{id} signed by the owner for an id no collection has',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000ff',
    signer: 'owner'
  },
  {
    name: '83-collection-malformed-id',
    description: 'GET /collections/{id} signed by the owner with an id that is not a UUID',
    method: 'GET',
    path: '/v1/collections/not-a-uuid',
    signer: 'owner'
  },
  {
    name: '84-collection-approved-stranger',
    description: 'GET /collections/{id} signed by a wallet that holds no role on an approved collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000103',
    signer: 'stranger'
  },
  {
    name: '85-collection-save-create',
    description: 'PUT /collections/{id} signed by the owner, creating a draft with initialize data that names it as the creator',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000f1',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000f1',
        name: 'Conformance created',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    },
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '86-collection-save-rename',
    description: 'PUT /collections/{id} signed by the owner, renaming its draft',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000001',
        name: 'Conformance renamed',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: '0x72d18e0bfa67ba8dce8e9463c8953dce9bd7e4bd88f9149e54b63f974fabefea',
        contract_address: '0x00cc1666ebe7da593c2a77d5bb9686e923f44e26',
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    },
    volatile: [
      'data.updated_at'
    ]
  },
  {
    name: '87-collection-save-name-in-use',
    description: 'PUT /collections/{id} signed by the owner, creating a draft whose name another collection has in another case',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000f2',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000f2',
        name: 'LISTING DRAFT',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '88-collection-save-ids-differ',
    description: 'PUT /collections/{id} signed by the owner, with a body id that is not the path id',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000f3',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000f4',
        name: 'Conformance ids',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '89-collection-save-without-data',
    description: 'PUT /collections/{id} signed by the owner, creating a draft without data',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000f5',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000f5',
        name: 'Conformance without data',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      }
    }
  },
  {
    name: '90-collection-save-claims-publication',
    description: 'PUT /collections/{id} signed by the owner, creating a draft that claims to be published',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000f6',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000f6',
        name: 'Conformance claims',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: true,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '91-collection-save-published',
    description: 'PUT /collections/{id} signed by the owner of a collection on chain',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000006',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000006',
        name: 'Chain renamed',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '92-collection-save-extra-field',
    description: 'PUT /collections/{id} signed by the owner, creating a draft whose body has a field the schema does not name',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000f7',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000f7',
        name: 'Conformance extra',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        forum_post: 'x'
      },
      data: OWNER_INITIALIZE
    },
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '93-collection-save-foreign-creator',
    description: 'PUT /collections/{id} signed by the owner, creating a draft with initialize data that names another wallet as the creator',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000f8',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000f8',
        name: 'Conformance foreign',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: STRANGER_INITIALIZE
    },
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '94-collection-save-third-party-create',
    description: 'PUT /collections/{id} signed by a manager of the provider, creating a third-party collection',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000f9',
    signer: 'creator',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000f9',
        name: 'Listing created',
        eth_address: '0x05bb381a642d932037707c4a626f93fb009696b2',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:listing:listing-created'
      },
      data: ''
    },
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '95-collection-save-third-party-urn-in-use',
    description: 'PUT /collections/{id} signed by a manager of the provider, creating a third-party collection with a URN another collection has',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-0000000000fa',
    signer: 'creator',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-0000000000fa',
        name: 'Listing duplicate',
        eth_address: '0x05bb381a642d932037707c4a626f93fb009696b2',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:listing:listing-linked'
      },
      data: ''
    }
  },
  {
    name: '96-collection-save-third-party-rename-urn',
    description: 'PUT /collections/{id} signed by a manager of the provider, changing the URN of an unpublished third-party collection',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000105',
    signer: 'creator',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000105',
        name: 'Listing linked',
        eth_address: '0x05bb381a642d932037707c4a626f93fb009696b2',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:listing:listing-renamed',
        linked_contract_address: '0x5eed00000000000000000000000000000000c0de',
        linked_contract_network: 'mainnet'
      },
      data: ''
    },
    volatile: [
      'data.updated_at'
    ]
  },
  {
    name: '97-collection-save-third-party-as-standard',
    description: 'PUT /collections/{id} signed by the owner, saving a third-party collection without its URN',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000008',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000008',
        name: 'Conformance linked A',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '98-collection-save-standard-as-third-party',
    description: 'PUT /collections/{id} signed by the owner, saving its standard draft with a third-party URN',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000001',
        name: 'Conformance renamed',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:conformance:converted'
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '99-collection-save-third-party-provider-change',
    description: 'PUT /collections/{id} signed by a manager of the provider, moving its collection to another provider',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000105',
    signer: 'creator',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000105',
        name: 'Listing linked',
        eth_address: '0x05bb381a642d932037707c4a626f93fb009696b2',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:conformance:listing-renamed'
      },
      data: ''
    }
  }
,
  {
    name: '100-address-collections-published-only',
    description: 'GET /{address}/collections signed by that address, filtered to published collections',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      is_published: 'true'
    },
    signer: 'creator'
  },
  {
    name: '101-address-collections-published-search',
    description: 'GET /{address}/collections signed by that address, filtered to published collections and searching by name',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      is_published: 'true',
      q: 'approved'
    },
    signer: 'creator'
  },
  {
    name: '102-address-collections-published-no-match',
    description: 'GET /{address}/collections signed by that address, filtered to published collections with a search nothing matches',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      is_published: 'true',
      q: 'zzz'
    },
    signer: 'creator'
  },
  {
    name: '103-address-collections-published-page',
    description: 'GET /{address}/collections signed by that address, filtered to published collections, paginated',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      is_published: 'true',
      page: '1',
      limit: '1'
    },
    signer: 'creator'
  },
  {
    name: '104-address-collections-unpublished-search',
    description: 'GET /{address}/collections signed by that address, filtered to unpublished collections and searching by name',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      is_published: 'false',
      q: 'draft'
    },
    signer: 'creator'
  },
  {
    name: '105-address-collections-standard',
    description: 'GET /{address}/collections signed by that address, filtered to standard collections',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      type: 'standard'
    },
    signer: 'creator'
  },
  {
    name: '106-address-collections-page-two',
    description: 'GET /{address}/collections signed by that address, filtered to standard collections, the second page of two',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      type: 'standard',
      page: '2',
      limit: '2'
    },
    signer: 'creator'
  },
  {
    name: '107-address-collections-oldest-first',
    description: 'GET /{address}/collections signed by that address, filtered to standard collections, oldest first',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      type: 'standard',
      sort: 'CREATED_AT_ASC'
    },
    signer: 'creator'
  },
  {
    name: '108-address-collections-none',
    description: 'GET /{address}/collections signed by that address, which holds no collection',
    method: 'GET',
    path: '/v1/0x01eba3f0e992f53debaf51ed3738dc2f23e09aee/collections',
    signer: 'committee'
  },
  {
    name: '109-address-collections-committee-other',
    description: 'GET /{address}/collections signed by a committee member for another address',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    signer: 'committee'
  },
  {
    name: '110-collections-committee-curation-page',
    description: 'GET /collections signed by a committee member, as the curation page asks for its first page',
    method: 'GET',
    path: '/v1/collections',
    query: {
      page: '1',
      limit: '12',
      is_published: 'true',
      sort: 'MOST_RELEVANT'
    },
    signer: 'committee'
  },
  {
    name: '111-collections-committee-curation-page-two',
    description: 'GET /collections signed by a committee member, as the curation page asks for a later page',
    method: 'GET',
    path: '/v1/collections',
    query: {
      page: '2',
      limit: '2',
      is_published: 'true',
      sort: 'MOST_RELEVANT'
    },
    signer: 'committee'
  },
  {
    name: '112-collections-committee-third-party',
    description: 'GET /collections signed by a committee member, filtered to third-party collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'conformance linked',
      type: 'third_party'
    },
    signer: 'committee'
  },
  {
    name: '113-collections-committee-review-new',
    description: 'GET /collections signed by a committee member, filtered to reviews of new collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      type: 'standard',
      review_type: 'new'
    },
    signer: 'committee'
  },
  {
    name: '114-collections-committee-sort-name-asc',
    description: 'GET /collections signed by a committee member, standard collections sorted by NAME_ASC',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      type: 'standard',
      sort: 'NAME_ASC'
    },
    signer: 'committee'
  },
  {
    name: '115-collections-committee-sort-created-at-asc',
    description: 'GET /collections signed by a committee member, standard collections sorted by CREATED_AT_ASC',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      type: 'standard',
      sort: 'CREATED_AT_ASC'
    },
    signer: 'committee'
  },
  {
    name: '116-collections-committee-sort-created-at-desc',
    description: 'GET /collections signed by a committee member, standard collections sorted by CREATED_AT_DESC',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      type: 'standard',
      sort: 'CREATED_AT_DESC'
    },
    signer: 'committee'
  },
  {
    name: '117-collections-committee-sort-updated-at-asc',
    description: 'GET /collections signed by a committee member, standard collections sorted by UPDATED_AT_ASC',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      type: 'standard',
      sort: 'UPDATED_AT_ASC'
    },
    signer: 'committee'
  },
  {
    name: '118-collections-committee-sort-updated-at-desc',
    description: 'GET /collections signed by a committee member, standard collections sorted by UPDATED_AT_DESC',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      type: 'standard',
      sort: 'UPDATED_AT_DESC'
    },
    signer: 'committee'
  },
  {
    name: '119-collections-committee-sort-last-activity-asc',
    description: 'GET /collections signed by a committee member, standard collections sorted by LAST_ACTIVITY_ASC',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      type: 'standard',
      sort: 'LAST_ACTIVITY_ASC'
    },
    signer: 'committee'
  },
  {
    name: '120-collections-committee-sort-requested-at-desc',
    description: 'GET /collections signed by a committee member, standard collections sorted by REQUESTED_AT_DESC',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      type: 'standard',
      sort: 'REQUESTED_AT_DESC'
    },
    signer: 'committee'
  },
  {
    name: '121-collections-committee-assignee',
    description: 'GET /collections signed by a committee member, filtered to a curator nobody assigned',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      assignee: '0x01eba3f0e992f53debaf51ed3738dc2f23e09aee'
    },
    signer: 'committee'
  },
  {
    name: '122-collections-committee-rejected',
    description: 'GET /collections signed by a committee member, filtered to rejected, under review and auto-reviewing collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      status: [
        'rejected',
        'under_review',
        'auto_reviewing'
      ]
    },
    signer: 'committee'
  },
  {
    name: '123-collections-public-search-page',
    description: 'GET /collections unsigned, searching published collections, paginated',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      is_published: 'true',
      page: '1',
      limit: '1'
    }
  },
  {
    name: '124-collections-public-search-tag',
    description: 'GET /collections unsigned, searching published collections with an item tag',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'listing',
      is_published: 'true',
      tag: 'hat'
    }
  },
  {
    name: '125-collections-public-search-type',
    description: 'GET /collections unsigned, searching published standard collections',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'chain',
      is_published: 'true',
      type: 'standard'
    }
  },
  {
    name: '126-collection-new-chain-creator',
    description: 'GET /collections/{id} signed by the chain creator of a collection another wallet stored',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000106',
    signer: 'stranger'
  },
  {
    name: '127-collection-stored-owner-after-transfer',
    description: 'GET /collections/{id} signed by the stored owner of a collection whose chain creator is another wallet',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000106',
    signer: 'creator'
  },
  {
    name: '128-collection-third-party-committee',
    description: 'GET /collections/{id} signed by a committee member who holds no other role on a third-party draft',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000105',
    signer: 'committee',
    volatile: [
      'data.updated_at'
    ]
  },
  {
    name: '129-collection-admin',
    description: 'GET /collections/{id} signed by an ADMIN_ADDRESSES wallet on a collection on chain',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000006',
    signer: 'admin'
  },
  {
    name: '130-collection-save-draft-managers',
    description: 'PUT /collections/{id} signed by the owner, saving its draft with a manager in the body',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000001',
        name: 'Conformance renamed',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [
          '0x1257bf35fafebbfc08ea47903e1f7c36ecda48c0'
        ]
      },
      data: OWNER_INITIALIZE
    },
    volatile: [
      'data.updated_at'
    ]
  },
  {
    name: '131-collection-body-manager',
    description: 'GET /collections/{id} signed by a wallet the draft names as manager in its body only',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    signer: 'manager'
  },
  {
    name: '132-collection-save-unsigned',
    description: 'PUT /collections/{id} without a signature, with a valid body',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000201',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000201',
        name: 'Conformance unsigned',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '133-collection-save-third-party-stranger',
    description: 'PUT /collections/{id} signed by a wallet that manages no provider, saving an existing third-party collection',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000105',
    signer: 'stranger',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000105',
        name: 'Listing linked',
        eth_address: '0xcd6884d4258edc833f31ac773e24affe6a7d3154',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:listing:listing-renamed'
      },
      data: ''
    }
  },
  {
    name: '134-collection-save-name-of-third-party',
    description: 'PUT /collections/{id} signed by the owner, creating a draft named as a third-party collection',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000202',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000202',
        name: 'Listing linked',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    },
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '135-collection-save-name-case-and-spaces',
    description: 'PUT /collections/{id} signed by the owner, creating a draft whose name differs from one in use only by case and spaces',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000203',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000203',
        name: ' CONFORMANCE RENAMED ',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '136-collection-save-approval-claim',
    description: 'PUT /collections/{id} signed by the owner, creating a draft that claims approval only',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000204',
    signer: 'owner',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000204',
        name: 'Conformance approval',
        eth_address: '0xc1aba94a79ca07d9d15a62ff5c9b382777e0c70e',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: true,
        minters: [],
        managers: []
      },
      data: OWNER_INITIALIZE
    }
  },
  {
    name: '137-collection-save-linked-contract-taken',
    description: 'PUT /collections/{id} signed by a manager of the provider, creating a collection linked to a contract another collection of the provider links',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000205',
    signer: 'creator',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000205',
        name: 'Listing duplicate link',
        eth_address: '0x05bb381a642d932037707c4a626f93fb009696b2',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:listing:listing-duplicate',
        linked_contract_address: '0x5eed00000000000000000000000000000000c0de',
        linked_contract_network: 'mainnet'
      },
      data: ''
    }
  },
  {
    name: '138-collection-save-linked-contract-cleared',
    description: 'PUT /collections/{id} signed by a manager of the provider, clearing the linked contract of its collection',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000105',
    signer: 'creator',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000105',
        name: 'Listing linked',
        eth_address: '0x05bb381a642d932037707c4a626f93fb009696b2',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:listing:listing-renamed',
        linked_contract_address: null,
        linked_contract_network: null
      },
      data: ''
    }
  },
  {
    name: '139-collection-save-registered-provider-stranger',
    description: 'PUT /collections/{id} signed by a wallet that manages no provider, creating a collection under a provider the registry lists',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000206',
    signer: 'stranger',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000206',
        name: 'Registered summer',
        eth_address: '0xcd6884d4258edc833f31ac773e24affe6a7d3154',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:registered:summer'
      },
      data: ''
    }
  },
  {
    name: '140-collection-save-registered-provider-manager',
    description: 'PUT /collections/{id} signed by a manager the registry lists, creating a collection under its provider',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000207',
    signer: 'manager',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000207',
        name: 'Registered winter',
        eth_address: '0x1257bf35fafebbfc08ea47903e1f7c36ecda48c0',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:registered:winter'
      },
      data: ''
    },
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '141-collection-registered-manager',
    description: 'GET /collections/{id} signed by another manager the registry lists for the provider of a third-party collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000207',
    signer: 'minter',
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '142-address-collections-registered-manager',
    description: 'GET /{address}/collections signed by that address, a manager the registry lists for a provider',
    method: 'GET',
    path: '/v1/0xc1eba15006e9d7f629bcad7a7dd41285f012b949/collections',
    signer: 'minter',
    volatile: [
      'data.created_at',
      'data.updated_at',
      'data.last_activity_at'
    ]
  },
  {
    name: '143-collections-committee-registered-provider',
    description: 'GET /collections signed by a committee member, searching collections of a provider the registry lists',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'registered'
    },
    signer: 'committee',
    volatile: [
      'data.created_at',
      'data.updated_at',
      'data.last_activity_at'
    ]
  },
  {
    name: '144-collection-save-programmatic-provider',
    description: 'PUT /collections/{id} signed by a manager the registry lists, creating a collection under its programmatic provider',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000208',
    signer: 'manager',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000208',
        name: 'Programmatic spring',
        eth_address: '0x1257bf35fafebbfc08ea47903e1f7c36ecda48c0',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:programmatic:spring'
      },
      data: ''
    },
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '145-collection-programmatic-provider',
    description: 'GET /collections/{id} signed by the manager of the programmatic provider of a third-party collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000208',
    signer: 'manager',
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '146-address-collections-programmatic-provider',
    description: 'GET /{address}/collections signed by that address, a manager the registry lists for a programmatic provider',
    method: 'GET',
    path: '/v1/0x1257bf35fafebbfc08ea47903e1f7c36ecda48c0/collections',
    query: {
      type: 'third_party'
    },
    signer: 'manager',
    volatile: [
      'data.created_at',
      'data.updated_at',
      'data.last_activity_at'
    ]
  },
  {
    name: '147-collections-committee-programmatic-provider',
    description: 'GET /collections signed by a committee member, searching collections of a programmatic provider',
    method: 'GET',
    path: '/v1/collections',
    query: {
      q: 'spring'
    },
    signer: 'committee',
    volatile: [
      'data.created_at',
      'data.updated_at',
      'data.last_activity_at'
    ]
  },
  {
    name: '148-collection-save-registered-other-manager',
    description: 'PUT /collections/{id} signed by a manager the registry lists, saving a collection another manager of the provider created',
    method: 'PUT',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000207',
    signer: 'minter',
    body: {
      collection: {
        id: 'c011ec71-0000-4000-8000-000000000207',
        name: 'Registered winter',
        eth_address: '0xc1eba15006e9d7f629bcad7a7dd41285f012b949',
        salt: null,
        contract_address: null,
        is_published: false,
        is_approved: false,
        minters: [],
        managers: [],
        urn: 'urn:decentraland:matic:collections-thirdparty:registered:winter'
      },
      data: ''
    },
    volatile: [
      'data.created_at',
      'data.updated_at'
    ]
  },
  {
    name: '149-address-collections-query-address-other',
    description: 'GET /{address}/collections signed by that address, with an address query parameter naming another wallet',
    method: 'GET',
    path: '/v1/0x05bb381a642d932037707c4a626f93fb009696b2/collections',
    query: {
      address: '0xcd6884d4258edc833f31ac773e24affe6a7d3154',
      type: 'standard'
    },
    signer: 'creator'
  },
  {
    name: '150-address-collections-query-address-own',
    description: 'GET /{address}/collections signed by another wallet, with an address query parameter naming the signer',
    method: 'GET',
    path: '/v1/0xcd6884d4258edc833f31ac773e24affe6a7d3154/collections',
    query: {
      address: '0x05bb381a642d932037707c4a626f93fb009696b2',
      type: 'standard'
    },
    signer: 'creator'
  },
  {
    name: '151-collection-query-id',
    description: 'GET /collections/{id} signed by the owner, with an id query parameter naming another collection',
    method: 'GET',
    path: '/v1/collections/c011ec71-0000-4000-8000-000000000001',
    query: {
      id: 'c011ec71-0000-4000-8000-000000000006'
    },
    signer: 'owner',
    volatile: [
      'data.updated_at'
    ]
  }
]

/** Why `cases` can't run in the order of their numbers: a name without a number, or a number not above the one before. */
function caseOrderErrors(cases) {
  const errors = []
  let previous
  for (const { name } of cases) {
    const number = /^(\d+)-/.exec(name)?.[1]
    if (number === undefined) {
      errors.push(`${name}: a case name starts with its number`)
      continue
    }
    if (previous && Number(number) <= Number(previous.number)) errors.push(`${name} runs after ${previous.name}`)
    previous = { name, number }
  }
  return errors
}

/** The source captures a gap of `ledger` (parity.json) lists that no case of `cases` records. */
function ledgerCaptureErrors(ledger, cases) {
  const recorded = new Set(cases.map(({ name }) => `source/${name}.json`))
  return ['deliberate', 'open'].flatMap(kind =>
    Object.entries(ledger[`${kind}_gaps`] ?? {}).flatMap(([name, gap]) =>
      gap.captures
        .filter(capture => capture.startsWith('source/') && !recorded.has(capture))
        .map(capture => `${kind} gap ${name}: lists ${capture}, which is no capture`)
    )
  )
}

function fail(message) {
  throw Error(message)
}

function decoded(text) {
  try {
    return decodeURIComponent(text)
  } catch {
    return text
  }
}

function passwordOf(databaseUrl) {
  try {
    return new URL(databaseUrl).password
  } catch {
    return /^[^:]+:\/\/[^:@/]*:([^@]*)@/.exec(databaseUrl)?.[1] ?? ''
  }
}

/** The message with the database URL and every spelling of its password replaced; never throws. */
function redact(message, databaseUrl) {
  const url = typeof databaseUrl === 'string' ? databaseUrl : ''
  const password = url ? passwordOf(url) : ''
  const secrets = [url, password, decoded(password)].filter(Boolean)
  return secrets.reduce((text, secret) => text.split(secret).join('<redacted>'), String(message))
}

function databaseUrl(base, name) {
  const url = new URL(base)
  url.pathname = '/' + name
  return url.toString()
}

/** Creates a scratch database, runs `body` with its URL, and drops it afterwards, on failure and on SIGINT or SIGTERM. */
async function withScratchDatabase(admin, adminUrl, body) {
  const name = `builder_capture_${process.pid}_${crypto.randomBytes(6).toString('hex')}`
  let dropping
  const drop = () => (dropping ??= admin.query(`DROP DATABASE IF EXISTS "${name}" WITH (FORCE)`).finally(() => admin.end()))
  const interrupted = signal => drop().finally(() => process.exit(128 + os.constants.signals[signal]))
  activeDrop = drop
  process.on('SIGINT', interrupted)
  process.on('SIGTERM', interrupted)
  try {
    await admin.query(`CREATE DATABASE "${name}"`)
    return await body(databaseUrl(adminUrl, name))
  } finally {
    process.off('SIGINT', interrupted)
    process.off('SIGTERM', interrupted)
    await drop()
    activeDrop = undefined
  }
}

/** Publishes `fixtures` ([name, text] pairs) into `output` only when `problems` is empty, staging them beside it first. */
function finish(output, fixtures, problems) {
  if (problems.length) fail(`upstream reached a service the capture does not fake: ${[...new Set(problems)].join(', ')}`)
  const target = path.resolve(output)
  const staging = fs.mkdtempSync(path.join(path.dirname(target), '.builder-capture-'))
  try {
    for (const [name, text] of fixtures) fs.writeFileSync(path.join(staging, name), text)
    fs.mkdirSync(target, { recursive: true })
    for (const [name] of fixtures) fs.renameSync(path.join(staging, name), path.join(target, name))
  } finally {
    fs.rmSync(staging, { recursive: true, force: true })
  }
}

function assertLockedInstall(upstream) {
  const read = file => JSON.parse(fs.readFileSync(path.join(upstream, file), 'utf8')).packages
  let locked, installed
  try {
    locked = read('package-lock.json')
    installed = read('node_modules/.package-lock.json')
  } catch {
    fail('Install the upstream dependencies with npm ci before capturing')
  }
  const differing = Object.entries(locked)
    .filter(([name, entry]) => name && !entry.optional)
    .filter(([name, entry]) => installed[name]?.version !== entry.version || installed[name]?.integrity !== entry.integrity)
    .map(([name]) => name)
  const extra = Object.keys(installed).filter(name => !locked[name])
  if (differing.length || extra.length) {
    fail(`node_modules differs from package-lock.json (${[...differing, ...extra].slice(0, 5).join(', ')}); install with npm ci`)
  }
}

function freezeClock() {
  const fixed = Date.parse(CLOCK)
  const RealDate = Date
  class FixedDate extends RealDate {
    constructor(...args) {
      if (args.length === 0) super(fixed)
      else super(...args)
    }
    static now() { return fixed }
  }
  globalThis.Date = FixedDate
}

function readSeed() {
  const seed = JSON.parse(fs.readFileSync(path.join(FIXTURES, 'seed.json'), 'utf8'))
  const unknown = Object.keys(seed).filter(key => !SEED_KEYS.includes(key))
  if (unknown.length) fail(`seed.json fields the capture does not apply: ${unknown.join(', ')}`)
  return seed
}

function wallets(req, seed) {
  const { ethers } = req('ethers')
  const key = label => ethers.utils.keccak256(ethers.utils.toUtf8Bytes(`dcl-one builder conformance ${label}`))
  const out = {}
  for (const [role, recorded] of Object.entries(seed.wallets)) {
    const root = new ethers.Wallet(key(role))
    const ephemeral = new ethers.Wallet(key(`${role} ephemeral`))
    const address = root.address.toLowerCase()
    if (address !== recorded) fail(`seed.json wallet ${role} must be ${address}`)
    out[role] = { root, ephemeral, address }
  }
  return out
}

/**
 * The headers a client signs for `item`: the timestamped payload over its method and path with
 * `item.metadata` (`{}` by default), with the one defect `item.sign` names: `offset` moves the
 * timestamp, `path` or `method` signs another one, `sentMetadata` sends other metadata than was
 * signed, and `untimestamped` signs `method:path` below `/v1` alone, as pre-ADR-44 clients do,
 * sending the timestamp and metadata headers too with `withHeaders`.
 */
async function signedHeaders(req, wallet, item) {
  const { Authenticator } = req('@dcl/crypto')
  const identity = await Authenticator.initializeAuthChain(
    wallet.address,
    { address: wallet.ephemeral.address, privateKey: wallet.ephemeral.privateKey, publicKey: wallet.ephemeral.publicKey },
    60,
    message => wallet.root.signMessage(message)
  )
  const sign = item.sign || {}
  const chainHeaders = payload => Object.fromEntries(
    Authenticator.signPayload(identity, payload).map((link, index) => [`x-identity-auth-chain-${index}`, JSON.stringify(link)])
  )
  const method = sign.method || item.method
  if (sign.untimestamped) {
    const headers = chainHeaders(`${method}:${item.path.replace(/^\/v1/, '')}`.toLowerCase())
    return sign.withHeaders ? { ...headers, 'x-identity-timestamp': String(Date.now()), 'x-identity-metadata': '{}' } : headers
  }
  const timestamp = String(Date.now() + (sign.offset || 0))
  const metadata = item.metadata || '{}'
  const payload = [method, sign.path || item.path, timestamp].join(':').toLowerCase() + ':' + metadata
  return {
    ...chainHeaders(payload),
    'x-identity-timestamp': timestamp,
    'x-identity-metadata': sign.sentMetadata || metadata
  }
}

/** The collections subgraph's rows for `seed.chain`, in the shape upstream's fragments select. */
function chainRows(seed, signers) {
  const addresses = roles => roles.map(role => signers[role].address)
  const accounts = addresses(seed.chain.committee).map(address => ({ id: address, address, isCommitteeMember: true }))
  const collections = seed.chain.collections.map(c => ({
    __typename: 'Collection',
    id: c.contract_address,
    creator: signers[c.creator].address,
    owner: signers[c.owner].address,
    name: c.name,
    isApproved: c.is_approved,
    isCompleted: true,
    minters: addresses(c.minters),
    managers: addresses(c.managers),
    reviewedAt: String(c.reviewed_at),
    updatedAt: String(c.updated_at),
    createdAt: String(c.created_at)
  }))
  const items = seed.chain.items.map(i => {
    const collection = collections.find(c => c.id === i.contract_address)
    return {
      __typename: 'Item',
      id: `${i.contract_address}-${i.blockchain_id}`,
      blockchainId: i.blockchain_id,
      urn: `urn:decentraland:matic:collections-v2:${i.contract_address}:${i.blockchain_id}`,
      totalSupply: i.total_supply,
      price: i.price,
      beneficiary: signers[i.beneficiary].address,
      managers: [],
      minters: [],
      contentHash: i.content_hash,
      collection: {
        __typename: 'Collection',
        id: collection.id,
        creator: collection.creator,
        owner: collection.owner,
        name: collection.name,
        isApproved: collection.isApproved,
        minters: collection.minters,
        managers: collection.managers
      },
      metadata: {
        __typename: 'Metadata',
        wearable: {
          __typename: 'Wearable',
          name: i.name,
          description: '',
          category: i.category,
          rarity: i.rarity,
          bodyShapes: ['BaseMale']
        }
      }
    }
  })
  for (const collection of collections) collection.items = items.filter(item => item.collection.id === collection.id)
  const registry = seed.registry.map(provider => ({
    __typename: 'ThirdParty',
    id: provider.id,
    root: null,
    managers: addresses(provider.managers),
    maxItems: '100',
    isApproved: true,
    isProgrammatic: !!provider.programmatic,
    metadata: { __typename: 'Metadata', type: 'third_party_v2', thirdParty: { __typename: 'ThirdPartyMetadata', name: provider.name, description: '', contracts: [] } }
  }))
  return { accounts, collections, items, registry }
}

/** The value of a GraphQL argument node, with its variables substituted. */
function argumentValue(node, variables) {
  if (node.kind === 'Variable') return variables[node.name.value]
  if (node.kind === 'ListValue') return node.values.map(v => argumentValue(v, variables))
  if (node.kind === 'ObjectValue') return Object.fromEntries(node.fields.map(f => [f.name.value, argumentValue(f.value, variables)]))
  if (node.kind === 'NullValue') return null
  return node.value
}

/** The rows of `field` its `where` argument selects; a filter the fake can't apply is reported. */
function selected(field, rows, variables, unfaked) {
  const arg = name => {
    const node = (field.arguments || []).find(a => a.name.value === name)
    return node ? argumentValue(node.value, variables) : undefined
  }
  const where = arg('where') || {}
  const lower = value => String(value).toLowerCase()
  const containsAll = (list, values, fold) => values.every(v => (list || []).map(fold).includes(fold(v)))
  const matched = rows.filter(row => Object.entries(where).every(([key, value]) => {
    if (value === undefined || value === null) return true
    if (!FAKED_WHERE_KEYS.has(key)) {
      unfaked.push(`graph ${field.name.value} where ${key}`)
      return false
    }
    if (key === 'id') return row.id === lower(value)
    if (key === 'id_in') return value.map(lower).includes(row.id)
    if (key === 'id_gt') return row.id > lower(value)
    if (key === 'isCommitteeMember') return row.isCommitteeMember === value
    if (key === 'creator') return row.creator === lower(value)
    if (key === 'managers_contains') return containsAll(row.managers, value, String)
    if (key === 'managers_contains_nocase') return containsAll(row.managers, value, lower)
    if (key === 'minters_contains') return containsAll(row.minters, value, String)
    return row.collection?.id === lower(value)
  }))
  const orderBy = arg('orderBy') || 'id'
  const ordered = [...matched].sort((a, b) => (a[orderBy] < b[orderBy] ? -1 : a[orderBy] > b[orderBy] ? 1 : 0))
  if (arg('orderDirection') === 'desc') ordered.reverse()
  const skip = Number(arg('skip') || 0)
  return ordered.slice(skip, skip + Number(arg('first') || 100))
}

function startFakes(req, unfaked, chain) {
  const { parse } = req('graphql')
  const server = http.createServer((request, response) => {
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const send = (status, body) => {
        response.writeHead(status, { 'content-type': 'application/json' })
        response.end(JSON.stringify(body))
      }
      if (request.method === 'POST' && request.url === '/peer/content/entities/active') return send(200, [])
      if (request.method === 'POST' && request.url === '/rpc') {
        const { id, method, params = [] } = JSON.parse(Buffer.concat(chunks).toString('utf8'))
        const deployed = address => chain.collections.some(c => c.id === String(address).toLowerCase())
        const results = { eth_chainId: '0x89', net_version: '137', eth_getCode: deployed(params[0]) ? '0x6080' : '0x' }
        if (!(method in results)) unfaked.push(`rpc ${method}`)
        return send(200, { jsonrpc: '2.0', id, result: results[method] ?? null })
      }
      if (request.method !== 'POST' || !request.url.startsWith('/graph/')) {
        unfaked.push(`${request.method} ${request.url}`)
        return send(404, { error: 'not faked' })
      }
      const { query, variables = {} } = JSON.parse(Buffer.concat(chunks).toString('utf8'))
      const operation = parse(query).definitions.find(d => d.kind === 'OperationDefinition')
      const subgraph = request.url === '/graph/collections' ? chain : request.url === '/graph/third-parties' ? { thirdParties: chain.registry } : {}
      const data = {}
      for (const field of operation.selectionSet.selections) {
        if (!FAKED_GRAPH_FIELDS.has(field.name.value)) unfaked.push(`graph ${field.name.value}`)
        const rows = subgraph[field.name.value]
        data[(field.alias || field.name).value] = rows ? selected(field, rows, variables, unfaked) : []
      }
      send(200, { data })
    })
  })
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => resolve(server)))
}

function upstreamEnvironment(fakeBase, url, admins) {
  return {
    CONNECTION_STRING: url,
    ANALYTICS_CONNECTION_STRING: url,
    DEFAULT_ETH_ADDRESS: '0x0000000000000000000000000000000000000000',
    API_VERSION: 'v1',
    CHAIN_NAME: 'Ethereum Mainnet',
    COLLECTIONS_GRAPH_URL: `${fakeBase}/graph/collections`,
    THIRD_PARTY_GRAPH_URL: `${fakeBase}/graph/third-parties`,
    PEER_URL: `${fakeBase}/peer`,
    RPC_URL: `${fakeBase}/rpc`,
    FORUM_URL: `${fakeBase}/forum`,
    OPEN_SEA_URL: `${fakeBase}/opensea`,
    OPEN_SEA_API_KEY: 'example-opensea-key',
    AWS_SNS_ARN: 'arn:aws:sns:us-east-1:000000000000:conformance',
    WEARABLE_VALIDATOR_CALLBACK_SECRET: 'example-callback-secret',
    WAREHOUSE_URL: `${fakeBase}/warehouse`,
    WAREHOUSE_TOKEN: 'example-warehouse-token',
    AWS_STORAGE_URL: `${fakeBase}/s3`,
    AWS_SNS_ENDPOINT: `${fakeBase}/sns`,
    AWS_BUCKET_NAME: 'conformance',
    AWS_ACCESS_KEY: 'example-access-key',
    AWS_ACCESS_SECRET: 'example-access-secret',
    AWS_REGION: 'us-east-1',
    BUILDER_SERVER_URL: 'https://builder-api.example.org',
    CREATORS_URL: 'https://creators.example.org',
    FF_AUTO_CURATION: 'false',
    FF_AUTO_CURATION_REVIEW_FLOW: 'false',
    FF_COLLECTION_API_ALT: 'false',
    SLACK_BOT_TOKEN: '',
    ADMIN_ADDRESSES: admins,
    NODE_ENV: 'production'
  }
}

/** Runs upstream's migrations in a child process that loads the offline guard first. */
function migrate(upstream, environment) {
  execFileSync(process.execPath, [
    '--require', GUARD,
    path.join(upstream, 'migrations/node-pg-migrate'),
    '--database-url-var', 'CONNECTION_STRING',
    '--migration-file-language', 'ts',
    '--migrations-dir', path.join(upstream, 'migrations'),
    '--ignore-pattern', '\\..*|.*migrate(.ts)?',
    'up'
  ], { cwd: upstream, env: { ...process.env, ...environment }, stdio: ['ignore', 'ignore', 'pipe'] })
}

async function seedDatabase(client, seed, signers) {
  for (const provider of seed.third_parties) {
    await client.query(
      'INSERT INTO virtual_third_parties (id, managers, raw_metadata) VALUES ($1, $2, $3)',
      [provider.id, provider.managers.map(role => signers[role].address), `tp:1:${provider.name}:`]
    )
  }
  for (const collection of seed.collections) {
    await client.query(
      'INSERT INTO collections (id, name, eth_address, salt, contract_address, third_party_id, urn_suffix, linked_contract_address, linked_contract_network, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)',
      [collection.id, collection.name, collection.third_party_id ? '' : signers[collection.owner].address, collection.salt, collection.contract_address, collection.third_party_id ?? null, collection.urn_suffix ?? null, collection.linked_contract_address ?? null, collection.linked_contract_network ?? null, collection.created_at, collection.updated_at]
    )
  }
  for (const item of seed.items) {
    await client.query(
      'INSERT INTO items (id, name, eth_address, collection_id, blockchain_item_id, type, data, metrics, thumbnail, contents, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)',
      [item.id, item.name, signers[item.owner].address, item.collection_id, item.blockchain_item_id, 'wearable', JSON.stringify(item.data), JSON.stringify(item.metrics), item.thumbnail, '{}', item.created_at, item.updated_at]
    )
  }
}

function scrub(value, parts) {
  if (Array.isArray(value)) return value.reduce((count, child) => count + scrub(child, parts), 0)
  if (!value || typeof value !== 'object' || !(parts[0] in value)) return 0
  if (parts.length > 1) return scrub(value[parts[0]], parts.slice(1))
  value[parts[0]] = VOLATILE
  return 1
}

/** Fails when upstream's own answer or the request already holds a redaction or volatile marker. */
function refuseMarkers(name, value, at = '') {
  if (typeof value === 'string') {
    if (value.includes('<<REDACTED')) fail(`${name}: upstream recorded <<REDACTED at ${at}`)
    if (value.includes(VOLATILE)) fail(`${name}: upstream recorded ${VOLATILE} at ${at}`)
    return
  }
  if (!value || typeof value !== 'object') return
  for (const [key, child] of Object.entries(value)) {
    refuseMarkers(name, child, Array.isArray(value) ? `${at}[${key}]` : at ? `${at}.${key}` : key)
  }
}

function valuesAt(value, parts) {
  if (Array.isArray(value)) return value.flatMap(child => valuesAt(child, parts))
  if (!parts.length) return [value]
  if (!value || typeof value !== 'object' || !(parts[0] in value)) return []
  return valuesAt(value[parts[0]], parts.slice(1))
}

/** The groups of the case's volatile paths whose recorded values were equal before masking. */
function volatileEqualities(item, body) {
  const groups = new Map()
  for (const volatile of item.volatile || []) {
    const values = valuesAt(body, volatile.split('.'))
    if (!values.length) continue
    const key = JSON.stringify(values)
    groups.set(key, [...(groups.get(key) || []), volatile])
  }
  return [...groups.values()].filter(group => group.length > 1)
}

/** Masks the case's volatile paths in `body`; each must name a field in `allowed` and match a recorded value. */
function scrubVolatile(item, body, allowed) {
  for (const volatile of item.volatile || []) {
    if (!allowed.includes(volatile.split('.').at(-1))) fail(`${item.name}: volatile path ${volatile} is not one of the volatile_fields`)
    if (!scrub(body, volatile.split('.'))) fail(`${item.name}: volatile path ${volatile} matches nothing`)
  }
  return body
}

/**
 * The `sign` value a capture keeps for `item` (the case's defect plus the metadata it signed when
 * timestamped), or undefined without one; refuses a defect the replay would not reproduce.
 */
function recordedSign(item) {
  if (!item.sign) return undefined
  const sign = item.sign.untimestamped ? { ...item.sign, ...(item.metadata ? { metadata: item.metadata } : {}) } : { ...item.sign, metadata: item.metadata || '{}' }
  const problems = []
  const defect = sign.untimestamped || sign.offset || sign.path || sign.method || sign.sentMetadata
  if (!defect) problems.push(sign.offset === 0 ? 'offset 0 is no defect' : 'names no defect')
  if (sign.withHeaders && !sign.untimestamped) problems.push('withHeaders needs untimestamped')
  if (sign.untimestamped && ['path', 'offset', 'metadata', 'sentMetadata'].some(key => key in sign)) {
    problems.push('untimestamped takes no path, offset, metadata or sentMetadata')
  }
  if (problems.length) fail(problems.map(problem => `${item.name}: sign ${problem}`).join('; '))
  return sign
}

function fixtureJson(fixture) {
  return JSON.stringify(fixture, null, 2) + '\n'
}

async function record(req, base, signers, volatileFields, item) {
  const query = item.query || {}
  const search = new URLSearchParams(Object.entries(query).flatMap(([key, value]) => [value].flat().map(v => [key, v]))).toString()
  const headers = item.signer ? await signedHeaders(req, signers[item.signer], item) : { ...item.headers }
  if (item.body !== undefined) headers['content-type'] = 'application/json'
  const response = await fetch(base + item.path + (search ? '?' + search : ''), {
    method: item.method,
    headers,
    body: item.body === undefined ? undefined : JSON.stringify(item.body),
    signal: AbortSignal.timeout(5000)
  })
  const bytes = Buffer.from(await response.arrayBuffer())
  const kept = {}
  for (const name of KEPT_RESPONSE_HEADERS) {
    const value = response.headers.get(name)
    if (value !== null) kept[name] = value
  }
  const recorded = { status: response.status, headers: kept }
  if ((kept['content-type'] || '').startsWith('application/json')) recorded.body_json = JSON.parse(bytes.toString('utf8'))
  else recorded.body_bytes_b64 = bytes.toString('base64')
  refuseMarkers(item.name, { request: { headers, query, body: item.body }, response: recorded })
  const equalities = volatileEqualities(item, recorded.body_json)
  scrubVolatile(item, recorded.body_json, volatileFields)
  return {
    description: `${item.description}. Recorded from ${REPOSITORY} at ${REVISION.slice(0, 7)} by tooling/builder-server-capture.cjs.`,
    request: {
      method: item.method,
      path: item.path,
      query,
      headers: Object.fromEntries(Object.entries(headers).sort(([a], [b]) => a.localeCompare(b))),
      body: item.body === undefined ? null : item.body
    },
    response: recorded,
    captured_from: `${REPOSITORY}@${REVISION}`,
    captured_at: CLOCK,
    volatile_paths: item.volatile || [],
    ...(equalities.length ? { volatile_equal: equalities } : {}),
    ...(item.headers ? { auth: 'recorded' } : {}),
    ...(item.sign ? { sign: recordedSign(item) } : {})
  }
}

async function closeServer(server) {
  if (server) await new Promise((resolve, reject) => server.close(error => (error ? reject(error) : resolve())))
}

async function runUpstream(capture, url, seed, signers, unfaked) {
  const { upstream, req, load } = capture
  const fakes = await startFakes(req, unfaked, chainRows(seed, signers))
  let listener, db
  try {
    const environment = upstreamEnvironment(`http://127.0.0.1:${fakes.address().port}`, url, signers.admin.address)
    migrate(upstream, environment)
    const { Client } = req('pg')
    const client = new Client({ connectionString: url })
    await client.connect()
    try { await seedDatabase(client, seed, signers) } finally { await client.end() }
    Object.assign(process.env, environment)
    freezeClock()
    process.stdout.write = () => true
    process.stderr.write = () => true
    const { app } = load('server.ts')
    db = load('database').db
    await db.connect(url)
    listener = await new Promise(resolve => {
      const server = app.getApp().listen(0, '127.0.0.1', () => resolve(server))
    })
    const base = `http://127.0.0.1:${listener.address().port}`
    const fixtures = []
    for (const item of CASES) fixtures.push([`${item.name}.json`, fixtureJson(await capture.record(base, item))])
    return fixtures
  } finally {
    try {
      await closeServer(listener)
      if (db) await db.close()
    } finally {
      await closeServer(fakes)
    }
  }
}

async function main() {
  const [root, output] = process.argv.slice(2)
  const ledger = JSON.parse(fs.readFileSync(path.join(FIXTURES, 'parity.json'), 'utf8'))
  const planErrors = [...caseOrderErrors(CASES), ...ledgerCaptureErrors(ledger, CASES)]
  if (planErrors.length) fail(planErrors.join('; '))
  const { refused } = require(GUARD)
  const { upstream } = assertPinnedSource(root, 'builder')
  if (!output) fail('Usage: node tooling/builder-server-capture.cjs <upstream-checkout> <output-directory>')
  const adminUrl = requireDatabaseUrl(process.env.CATALYST_TEST_DATABASE_URL)
  try { databaseUrl(adminUrl, 'postgres') } catch { fail('CATALYST_TEST_DATABASE_URL is not a valid URL') }
  assertLockedInstall(upstream)
  const capture = sourceCapture(upstream, { kind: 'builder' })
  const seed = readSeed()
  const signers = wallets(capture.req, seed)
  const { volatile_fields: volatileFields } = ledger
  capture.record = (base, item) => record(capture.req, base, signers, volatileFields, item)
  const unfaked = []
  const { Client } = capture.req('pg')
  const admin = new Client({ connectionString: databaseUrl(adminUrl, 'postgres') })
  await admin.connect()
  const fixtures = await withScratchDatabase(admin, adminUrl, url => runUpstream(capture, url, seed, signers, unfaked))
  finish(output, fixtures, [...refused, ...unfaked])
}

function failed(error) {
  report(`builder capture failed: ${redact(error?.message ?? error, process.env.CATALYST_TEST_DATABASE_URL)}\n`)
  Promise.resolve()
    .then(() => activeDrop?.())
    .catch(() => {})
    .finally(() => process.exit(1))
}

/** Reports an uncaught error or rejection redacted, drops the scratch database and exits with 1. */
function installFailureHandlers() {
  process.on('uncaughtException', failed)
  process.on('unhandledRejection', failed)
}

if (require.main === module) {
  installFailureHandlers()
  main().then(() => process.exit(0), failed)
}

module.exports = { CASES, caseOrderErrors, ledgerCaptureErrors,recordedSign, withScratchDatabase, finish, redact, scrubVolatile, refuseMarkers, volatileEqualities, migrate, installFailureHandlers, selected }
