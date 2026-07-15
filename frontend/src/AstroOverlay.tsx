import { useState } from 'react'
import { AstroOverlay as ReusableAstroOverlay } from '@seiza/astro-overlay/react'
import {
  defaultOverlayDensity,
  defaultOverlayTheme,
  type OverlayLayerVisibility,
} from '@seiza/astro-overlay'
import type { OverlayObject, Solution } from './api'
import { deepSkyCatalogForObject, deepSkyCatalogs } from './catalogs'
import type { DeepSkyCatalogId } from './catalogs'

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
  objects,
  hiddenCatalogs,
  onChange,
  onHiddenCatalogsChange,
}: {
  layers: OverlayLayers
  counts: Record<string, number>
  available?: Record<string, boolean>
  disabledReasons?: Record<string, string>
  objects: OverlayObject[]
  hiddenCatalogs: DeepSkyCatalogId[]
  onChange: (layers: OverlayLayers) => void
  onHiddenCatalogsChange: (catalogs: DeepSkyCatalogId[]) => void
}) {
  return <div className="overlay-control-row">
    <div className="overlay-options" role="group" aria-label="Overlay layers">
      {layerLabels.map(([key, label, countKey]) => {
        const enabled = available?.[countKey] !== false
        return <button
          key={key}
          type="button"
          aria-pressed={enabled && layers[key]}
          disabled={!enabled}
          title={enabled ? undefined : disabledReasons?.[countKey] ?? `${label} data is unavailable for this solution`}
          onClick={() => onChange({ ...layers, [key]: !layers[key] })}
        >{label}{counts[countKey] == null ? '' : ` · ${counts[countKey]}`}</button>
      })}
    </div>
    <DeepSkyCatalogMenu
      objects={objects}
      disabled={available?.deep_sky === false || !layers.deepSky}
      hiddenCatalogs={hiddenCatalogs}
      onChange={onHiddenCatalogsChange}
    />
  </div>
}

function DeepSkyCatalogMenu({
  objects,
  disabled,
  hiddenCatalogs,
  onChange,
}: {
  objects: OverlayObject[]
  disabled: boolean
  hiddenCatalogs: DeepSkyCatalogId[]
  onChange: (catalogs: DeepSkyCatalogId[]) => void
}) {
  const [open, setOpen] = useState(false)
  const counts = new Map<DeepSkyCatalogId, number>()
  for (const object of objects) {
    const catalog = deepSkyCatalogForObject(object)
    if (catalog) counts.set(catalog, (counts.get(catalog) ?? 0) + 1)
  }
  const availableCatalogs = deepSkyCatalogs.filter(([id]) => counts.has(id))
  if (availableCatalogs.length < 2) return null
  const activeCatalogs = availableCatalogs.filter(([id]) => !hiddenCatalogs.includes(id)).length
  const toggleCatalog = (id: DeepSkyCatalogId) => onChange(
    hiddenCatalogs.includes(id)
      ? hiddenCatalogs.filter((catalog) => catalog !== id)
      : [...hiddenCatalogs, id],
  )

  return <span className="catalog-filter">
    <button
      className="catalog-filter-trigger"
      type="button"
      aria-expanded={open}
      aria-haspopup="true"
      data-filtered={hiddenCatalogs.length > 0}
      disabled={disabled}
      title="Choose which deep-sky catalogs to label"
      onClick={() => setOpen(!open)}
    >Filter catalogs <span>{activeCatalogs}/{availableCatalogs.length}</span> <span aria-hidden="true">{open ? '▴' : '▾'}</span></button>
    {open && !disabled && <span className="catalog-menu" role="group" aria-label="Deep sky catalogs">
      {availableCatalogs.map(([id, label]) => <label key={id}>
        <input
          type="checkbox"
          checked={!hiddenCatalogs.includes(id)}
          onChange={() => toggleCatalog(id)}
        />
        <span>{label} · {counts.get(id)}</span>
      </label>)}
    </span>}
  </span>
}

export function AstroOverlay({
  solution,
  objects,
  layers,
  hiddenCatalogs,
}: {
  solution: Solution
  objects: OverlayObject[]
  layers: OverlayLayers
  hiddenCatalogs: DeepSkyCatalogId[]
}) {
  const visibleObjects = objects.filter((object) => {
    const catalog = deepSkyCatalogForObject(object)
    return catalog == null || !hiddenCatalogs.includes(catalog)
  })
  return <ReusableAstroOverlay
    className="sky-overlay"
    solution={solution}
    objects={visibleObjects}
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
