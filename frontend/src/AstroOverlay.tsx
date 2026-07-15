import { AstroOverlay as ReusableAstroOverlay } from '@seiza/astro-overlay/react'
import {
  defaultOverlayDensity,
  defaultOverlayTheme,
  type OverlayLayerVisibility,
} from '@seiza/astro-overlay'
import type { OverlayObject, Solution } from './api'

export interface OverlayLayers {
  deepSky: boolean
  namedStars: boolean
  starIdentifiers: boolean
  fieldStars: boolean
  transients: boolean
  minorBodies: boolean
  historicalTransients: boolean
  grid: boolean
}

const layerLabels: Array<[keyof OverlayLayers, string, string]> = [
  ['deepSky', 'Deep sky', 'deep_sky'],
  ['namedStars', 'Named stars', 'named_stars'],
  ['starIdentifiers', 'Star identifiers', 'star_identifiers'],
  ['fieldStars', 'Field stars', 'field_stars'],
  ['transients', 'Transients', 'transients'],
  ['minorBodies', 'Solar system', 'minor_bodies'],
  ['historicalTransients', 'Older transients', 'historical_transients'],
  ['grid', 'RA / Dec grid', 'grid'],
]

export function OverlayControls({
  layers,
  counts,
  available,
  disabledReasons,
  onChange,
}: {
  layers: OverlayLayers
  counts: Record<string, number>
  available?: Record<string, boolean>
  disabledReasons?: Record<string, string>
  onChange: (layers: OverlayLayers) => void
}) {
  return <div className="overlay-options" role="group" aria-label="Overlay layers">
    {layerLabels.map(([key, label, countKey]) => {
      const enabled = available?.[countKey] !== false
      return <button
        type="button"
        key={key}
        aria-pressed={enabled && layers[key]}
        disabled={!enabled}
        title={enabled ? undefined : disabledReasons?.[countKey] ?? `${label} data is unavailable for this solution`}
        onClick={() => onChange({ ...layers, [key]: !layers[key] })}
      >{label}{counts[countKey] == null ? '' : ` · ${counts[countKey]}`}</button>
    })}
  </div>
}

export function AstroOverlay({
  solution,
  objects,
  layers,
}: {
  solution: Solution
  objects: OverlayObject[]
  layers: OverlayLayers
}) {
  return <ReusableAstroOverlay
    className="sky-overlay"
    solution={solution}
    objects={objects}
    layers={toPackageLayers(layers)}
    density={defaultOverlayDensity}
    theme={defaultOverlayTheme}
  />
}

function toPackageLayers(layers: OverlayLayers): OverlayLayerVisibility {
  return {
    deep_sky: layers.deepSky,
    named_stars: layers.namedStars,
    star_identifiers: layers.starIdentifiers,
    field_stars: layers.fieldStars,
    transients: layers.transients,
    minor_bodies: layers.minorBodies,
    historical_transients: layers.historicalTransients,
    grid: layers.grid,
  }
}
