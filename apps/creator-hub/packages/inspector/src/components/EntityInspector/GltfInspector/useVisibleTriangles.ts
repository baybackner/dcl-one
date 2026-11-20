import { useEffect, useState } from 'react';

import { getDataLayerInterface } from '../../../redux/data-layer';
import { countVisibleTriangles } from './utils';

/** Triangle count of a model's visible meshes, or null while unknown. */
export function useVisibleTriangles(src: string | undefined, enabled: boolean): number | null {
  const [triangles, setTriangles] = useState<number | null>(null);

  useEffect(() => {
    setTriangles(null);
    if (!src || !enabled) return;
    let cancelled = false;
    getDataLayerInterface()
      ?.getFile({ path: src })
      .then(({ content }) => {
        if (!cancelled) setTriangles(countVisibleTriangles(content));
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [src, enabled]);

  return triangles;
}
