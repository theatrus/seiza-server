import { useState } from 'react'
import { AstroOverlay as ReusableAstroOverlay } from '@seiza/astro-overlay/react'
import {
  defaultOverlayDensity,
  defaultOverlayTheme,
  suggestedDeepSkyCatalogColors as deepSkyCatalogColors,
  suggestedDeepSkyCatalogForObject as deepSkyCatalogForObject,
  suggestedDeepSkyCatalogLayer as deepSkyCatalogLayer,
  suggestedDeepSkyCatalogs as deepSkyCatalogs,
  suggestedDeepSkyColorForObject,
  suggestedDeepSkyLayerForObject,
  type OverlayLayerVisibility,
  type SuggestedDeepSkyCatalogId as DeepSkyCatalogId,
} from '@seiza/astro-overlay'
import type { OverlayObject, SatelliteTrack, Solution } from './api'

export interface OverlayLayers {
  deepSky: boolean
  namedStars: boolean
  starIdentifiers: boolean
  fieldStars: boolean
  transients: boolean
  minorBodies: boolean
  satelliteTracks: boolean
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
  ['satelliteTracks', 'Satellite tracks', 'satellite_tracks'],
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
  showCatalogOutlines,
  onChange,
  onHiddenCatalogsChange,
  onShowCatalogOutlinesChange,
}: {
  layers: OverlayLayers
  counts: Record<string, number>
  available?: Record<string, boolean>
  disabledReasons?: Record<string, string>
  objects: OverlayObject[]
  hiddenCatalogs: DeepSkyCatalogId[]
  showCatalogOutlines: boolean
  onChange: (layers: OverlayLayers) => void
  onHiddenCatalogsChange: (catalogs: DeepSkyCatalogId[]) => void
  onShowCatalogOutlinesChange: (show: boolean) => void
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
      showCatalogOutlines={showCatalogOutlines}
      onChange={onHiddenCatalogsChange}
      onShowCatalogOutlinesChange={onShowCatalogOutlinesChange}
    />
  </div>
}

function DeepSkyCatalogMenu({
  objects,
  disabled,
  hiddenCatalogs,
  showCatalogOutlines,
  onChange,
  onShowCatalogOutlinesChange,
}: {
  objects: OverlayObject[]
  disabled: boolean
  hiddenCatalogs: DeepSkyCatalogId[]
  showCatalogOutlines: boolean
  onChange: (catalogs: DeepSkyCatalogId[]) => void
  onShowCatalogOutlinesChange: (show: boolean) => void
}) {
  const [open, setOpen] = useState(false)
  const counts = new Map<DeepSkyCatalogId, number>()
  for (const object of objects) {
    const catalog = deepSkyCatalogForObject(object)
    if (catalog) counts.set(catalog, (counts.get(catalog) ?? 0) + 1)
  }
  const availableCatalogs = deepSkyCatalogs.filter(([id]) => counts.has(id))
  const hasCatalogOutlines = objects.some((object) => (object.outlines?.length ?? 0) > 0)
  if (availableCatalogs.length < 2 && !hasCatalogOutlines) return null
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
        <span
          className="catalog-color-swatch"
          style={{ backgroundColor: deepSkyCatalogColors[id] }}
          aria-hidden="true"
        />
        <span>{label} · {counts.get(id)}</span>
      </label>)}
      {hasCatalogOutlines && <label className="catalog-outline-option">
        <input
          type="checkbox"
          checked={showCatalogOutlines}
          onChange={(event) => onShowCatalogOutlinesChange(event.currentTarget.checked)}
        />
        <span>Detailed OpenNGC outlines</span>
      </label>}
    </span>}
  </span>
}

export function AstroOverlay({
  solution,
  objects,
  satelliteTracks,
  layers,
  hiddenCatalogs,
  showCatalogOutlines,
}: {
  solution: Solution
  objects: OverlayObject[]
  satelliteTracks: SatelliteTrack[]
  layers: OverlayLayers
  hiddenCatalogs: DeepSkyCatalogId[]
  showCatalogOutlines: boolean
}) {
  const visibleObjects = objects
    .filter((object) => {
      const catalog = deepSkyCatalogForObject(object)
      return catalog == null || !hiddenCatalogs.includes(catalog)
    })
    .map((object) => showCatalogOutlines || (object.outlines?.length ?? 0) === 0
      ? object
      : { ...object, outlines: [] })
    .concat(satelliteTracks.map(satelliteTrackObject))

  return <ReusableAstroOverlay
    className="sky-overlay"
    solution={solution}
    objects={visibleObjects}
    layers={toPackageLayers(layers)}
    layerForObject={(object) => object.kind === 'satellite' ? 'satellite_tracks' : suggestedDeepSkyLayerForObject(object)}
    colorForObject={(object) => object.kind === 'satellite' ? '#ff8f70' : suggestedDeepSkyColorForObject(object)}
    density={defaultOverlayDensity}
    theme={defaultOverlayTheme}
  />
}

function toPackageLayers(layers: OverlayLayers): OverlayLayerVisibility {
  const visibility: Record<string, boolean> = {
    deep_sky: layers.deepSky,
    named_stars: layers.namedStars,
    star_identifiers: layers.starIdentifiers,
    field_stars: layers.fieldStars,
    transients: layers.transients,
    minor_bodies: layers.minorBodies,
    satellite_tracks: layers.satelliteTracks,
    historical_transients: layers.historicalTransients,
    grid: layers.grid,
  }
  for (const [catalog] of deepSkyCatalogs) {
    visibility[deepSkyCatalogLayer(catalog)] = layers.deepSky
  }
  return visibility
}

function satelliteTrackObject(track: SatelliteTrack): OverlayObject {
  const representative = track.segments.reduce<typeof track.segments[number] | null>((longest, segment) => {
    if (!longest) return segment
    const length = Math.hypot(segment.end[0] - segment.start[0], segment.end[1] - segment.start[1])
    const longestLength = Math.hypot(longest.end[0] - longest.start[0], longest.end[1] - longest.start[1])
    return length > longestLength ? segment : longest
  }, null)
  return {
    stable_id: track.stable_id,
    name: track.label,
    common_name: '',
    kind: 'satellite',
    mag: null,
    x: representative ? (representative.start[0] + representative.end[0]) / 2 : 0,
    y: representative ? (representative.start[1] + representative.end[1]) / 2 : 0,
    semi_major_px: 0,
    semi_minor_px: 0,
    angle_deg: null,
    source: 'satellite_prediction',
    catalog_source: track.source,
    aliases: track.cospar_id ? [track.cospar_id] : [],
    alternate_ids: track.norad_id == null ? [] : [`NORAD ${track.norad_id}`],
    motion_arcsec_per_hour: track.maximum_apparent_rate_arcsec_per_second == null
      ? undefined
      : track.maximum_apparent_rate_arcsec_per_second * 3600,
    outlines: [{
      geometry_id: `${track.stable_id}:predicted-track`,
      source_record_id: track.stable_id,
      role: 'predicted-track',
      quality: 'propagated',
      level: track.risk.level,
      contours: track.segments.map((segment) => ({
        closed: false,
        points: [segment.start, segment.end],
      })),
    }],
  }
}
