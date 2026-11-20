import type { PBGltfContainer } from '@dcl/ecs';
import { ColliderLayer } from '@dcl/ecs';

import { toNumber, toString } from '../utils';
import type { TreeNode } from '../../ProjectAssetExplorer/ProjectView';
import { isAssetNode } from '../../ProjectAssetExplorer/utils';
import type { AssetNodeItem } from '../../ProjectAssetExplorer/types';
import type { AssetCatalogResponse } from '../../../tooling-entrypoint';
import type { GltfContainerInput } from './types';

export const fromGltf = (value: PBGltfContainer): GltfContainerInput => {
  return {
    ...value,
    visibleMeshesCollisionMask: toString(value.visibleMeshesCollisionMask, ColliderLayer.CL_NONE),
    invisibleMeshesCollisionMask: toString(
      value.invisibleMeshesCollisionMask,
      ColliderLayer.CL_PHYSICS,
    ),
  };
};

export const toGltf = (value: GltfContainerInput): PBGltfContainer => {
  return {
    ...value,
    visibleMeshesCollisionMask: toNumber(value.visibleMeshesCollisionMask, ColliderLayer.CL_NONE),
    invisibleMeshesCollisionMask: toNumber(
      value.invisibleMeshesCollisionMask,
      ColliderLayer.CL_PHYSICS,
    ),
  };
};

export function isValidInput({ assets }: AssetCatalogResponse, src: string): boolean {
  if (!src) return true;
  return !!assets.find($ => src === $.path);
}

export const isAsset = (value: string): boolean =>
  value.endsWith('.gltf') || value.endsWith('.glb');
export const isModel = (node: TreeNode): node is AssetNodeItem =>
  isAssetNode(node) && isAsset(node.name);

export const COLLISION_LAYERS = [
  {
    value: ColliderLayer.CL_NONE,
    label: 'None',
  },
  {
    value: ColliderLayer.CL_POINTER,
    label: 'Pointer',
  },
  {
    value: ColliderLayer.CL_PHYSICS,
    label: 'Physics',
  },
  {
    value: ColliderLayer.CL_PHYSICS | ColliderLayer.CL_POINTER,
    label: 'Physics and Pointer',
  },
  {
    value: ColliderLayer.CL_CUSTOM1,
    label: 'Custom 1',
  },
  {
    value: ColliderLayer.CL_CUSTOM2,
    label: 'Custom 2',
  },
  {
    value: ColliderLayer.CL_CUSTOM3,
    label: 'Custom 3',
  },
  {
    value: ColliderLayer.CL_CUSTOM4,
    label: 'Custom 4',
  },
  {
    value: ColliderLayer.CL_CUSTOM5,
    label: 'Custom 5',
  },
  {
    value: ColliderLayer.CL_CUSTOM6,
    label: 'Custom 6',
  },
  {
    value: ColliderLayer.CL_CUSTOM7,
    label: 'Custom 7',
  },
  {
    value: ColliderLayer.CL_CUSTOM8,
    label: 'Custom 8',
  },
];

/** Above this many triangles, colliding against a model's visible geometry gets a warning. */
export const VISIBLE_COLLIDER_TRIANGLE_LIMIT = 10000;

const GLB_MAGIC = 0x46546c67;
const GLB_JSON_CHUNK = 0x4e4f534a;
const COLLIDER_NAME = /_collider/i;

type GltfJson = {
  nodes?: { name?: string; mesh?: number }[];
  meshes?: {
    name?: string;
    primitives?: { mode?: number; indices?: number; attributes?: { POSITION?: number } }[];
  }[];
  accessors?: { count?: number }[];
};

function readGltfJson(content: Uint8Array): GltfJson | null {
  try {
    const view = new DataView(content.buffer, content.byteOffset, content.byteLength);
    const isGlb = content.byteLength >= 20 && view.getUint32(0, true) === GLB_MAGIC;
    if (!isGlb) return JSON.parse(new TextDecoder().decode(content));
    if (view.getUint32(16, true) !== GLB_JSON_CHUNK) return null;
    const length = view.getUint32(12, true);
    return JSON.parse(new TextDecoder().decode(content.subarray(20, 20 + length)));
  } catch {
    return null;
  }
}

/**
 * Triangles in the visible meshes of a .glb or .gltf file, counted once per node that uses a mesh.
 * Meshes named `*_collider` are the invisible layer and are left out. Returns null when the file
 * can't be read as glTF.
 */
export function countVisibleTriangles(content: Uint8Array): number | null {
  const gltf = readGltfJson(content);
  if (!gltf || typeof gltf !== 'object') return null;
  const { nodes = [], meshes = [], accessors = [] } = gltf;

  let triangles = 0;
  for (const node of nodes) {
    const mesh = node.mesh === undefined ? undefined : meshes[node.mesh];
    if (!mesh) continue;
    if (COLLIDER_NAME.test(node.name ?? '') || COLLIDER_NAME.test(mesh.name ?? '')) continue;
    for (const primitive of mesh.primitives ?? []) {
      const accessor = primitive.indices ?? primitive.attributes?.POSITION;
      const count = (accessor === undefined ? undefined : accessors[accessor]?.count) ?? 0;
      const mode = primitive.mode ?? 4;
      if (mode === 4) triangles += Math.floor(count / 3);
      else if (mode === 5 || mode === 6) triangles += Math.max(count - 2, 0);
    }
  }
  return triangles;
}
