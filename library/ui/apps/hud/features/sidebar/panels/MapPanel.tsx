// MapPanel — what the Discover panel's Genesis City Map row shows: the HUD's minimap, in a region
// named "Map". The minimap places itself beside the bar and keeps its own controls (collapse,
// zoom, settings, tip, the way back from a world, close), so this file draws no box of its own.

import type { ComponentProps } from 'react'
import { Minimap } from '../../minimap/Minimap'
import type { MapState, MinimapState } from '../../session/useEngineSession'
import styles from './MapPanel.module.css'

interface MapPanelProps {
  minimap: MinimapState
  map: MapState
  /** The player's address, the minimap's copied link's referrer. */
  referrer?: string
  setEngineViewport: ComponentProps<typeof Minimap>['setEngineViewport']
  /** Hide the minimap, as the Genesis City Map row does: the close button after the three dots. */
  onClose?: () => void
}

export function MapPanel({ minimap, map, referrer, setEngineViewport, onClose }: MapPanelProps): React.JSX.Element {
  return (
    <section className={styles.root} aria-label="Map">
      <Minimap minimap={minimap} map={map} sceneTitle={minimap.sceneTitle} referrer={referrer} setEngineViewport={setEngineViewport} onClose={onClose} />
    </section>
  )
}
