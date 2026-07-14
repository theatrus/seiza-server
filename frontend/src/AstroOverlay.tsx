import { useMemo } from 'react'
import { OverlayObject, Solution } from './api'

export interface OverlayLayers {
  deepSky: boolean
  namedStars: boolean
  fieldStars: boolean
  transients: boolean
  minorBodies: boolean
  historicalTransients: boolean
  grid: boolean
}

const layerLabels: Array<[keyof OverlayLayers, string, string]> = [
  ['deepSky', 'Deep sky', 'deep_sky'],
  ['namedStars', 'Named stars', 'named_stars'],
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
  onChange,
}: {
  layers: OverlayLayers
  counts: Record<string, number>
  available?: Record<string, boolean>
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
        title={enabled ? undefined : `${label} data is unavailable for this solution`}
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
  const width = solution.image_width
  const height = solution.image_height
  const filtered = objects.filter((object) => layerVisible(object, layers))
  const fieldStars = filtered.filter((object) => object.kind === 'field-star')
  const labeled = filtered.filter((object) => object.kind !== 'field-star')
  const encompassing = labeled.filter((object) => encompassesFrame(object, width, height))
  const visible = labeled.filter((object) => !encompassing.includes(object))
  const grid = useMemo(() => makeGrid(solution), [solution])
  const gridFontSize = Math.max(width / 90, 14)
  const stroke = Math.max(width / 1800, 1.5)
  const fontSize = Math.max(width / 75, 14)
  const placedLabels: Array<{ x: number; y: number; halfWidth: number }> = []

  const labelText = (object: OverlayObject) => {
    if (object.common_name && object.common_name !== object.name) {
      return `${object.name} · ${object.common_name}`
    }
    return object.common_name || object.name
  }
  const labelY = (object: OverlayObject) => {
    const label = labelText(object)
    const halfWidth = label.length * fontSize * 0.275
    const radius = Math.max(object.semi_minor_px, fontSize)
    let y = object.y - radius - fontSize * 0.5
    for (let attempt = 0; attempt < 6; attempt += 1) {
      const collision = placedLabels.some((placed) =>
        Math.abs(placed.y - y) < fontSize * 1.3
        && Math.abs(placed.x - object.x) < placed.halfWidth + halfWidth,
      )
      if (!collision) break
      y -= fontSize * 1.4
    }
    placedLabels.push({ x: object.x, y, halfWidth })
    return y
  }

  return <svg
    className="sky-overlay"
    viewBox={`0 0 ${width} ${height}`}
    preserveAspectRatio="none"
    aria-label="Astronomical objects and coordinate grid"
  >
    <style>{`
      .coordinate-grid path { fill: none; stroke: #7ddbe8; stroke-width: 1.2; stroke-dasharray: 7 5; opacity: .72; vector-effect: non-scaling-stroke; }
      .coordinate-grid text { fill: #b9f3f7; stroke: #05090e; stroke-width: .12em; paint-order: stroke; font-family: ui-monospace, monospace; font-weight: 600; }
      .field-stars circle { fill: none; stroke: #eef7ff; stroke-width: 1.25; opacity: .78; vector-effect: non-scaling-stroke; }
      .object-marker { fill: none; vector-effect: non-scaling-stroke; }
      .overlay-label { stroke: rgba(0,0,0,.88); stroke-width: .12em; paint-order: stroke; font-family: ui-sans-serif, system-ui, sans-serif; font-weight: 700; }
      .solution-center { fill: none; stroke: #f2c66d; vector-effect: non-scaling-stroke; }
    `}</style>
    <defs><clipPath id="sky-frame"><rect width={width} height={height} /></clipPath></defs>
    {layers.grid && <g clipPath="url(#sky-frame)" className="coordinate-grid">
      {grid.map((curve, index) => <g key={`${curve.label}-${index}`}>
        <path d={curve.path} />
        <text x={curve.x} y={curve.y} textAnchor={curve.anchor} fontSize={gridFontSize}>{curve.label}</text>
      </g>)}
    </g>}
    <g className="field-stars">
      {fieldStars.map((star, index) => <circle
        key={`${star.ra_deg}-${star.dec_deg}-${index}`}
        cx={star.x}
        cy={star.y}
        r={Math.max(width / 1300, 2.5)}
      />)}
    </g>
    {encompassing.length > 0 && <text
      className="overlay-label encompassing-label"
      fill="#aee8ff"
      x={fontSize}
      y={height - fontSize}
      fontSize={fontSize}
    >Field within: {encompassing.map(labelText).join(' · ')}</text>}
    <g className="catalog-objects">
      {visible.map((object, index) => {
        const namedStar = object.kind === 'star' || object.kind === 'double-star'
        const transient = object.kind === 'transient'
        const moving = object.kind === 'comet' || object.kind === 'asteroid'
        const color = object.kind === 'comet'
          ? '#7bffd0'
          : object.kind === 'asteroid'
            ? '#ffb36b'
            : transient
              ? '#ff7be0'
              : namedStar
                ? '#ffd479'
                : '#5fd3ff'
        const a = Math.max(object.semi_major_px, fontSize)
        const b = Math.max(object.semi_minor_px, fontSize)
        const angle = (object.direction_angle_deg ?? 45) * Math.PI / 180
        const trail = {
          x1: object.x + Math.cos(angle) * a * 1.3,
          y1: object.y + Math.sin(angle) * a * 1.3,
          x2: object.x + Math.cos(angle) * a * 2.4,
          y2: object.y + Math.sin(angle) * a * 2.4,
        }
        return <g key={`${object.kind}-${object.name}-${object.x}-${object.y}-${index}`} data-kind={object.kind}>
          {moving || transient ? <>
            <path
              className="object-marker"
              stroke={color}
              strokeWidth={stroke * 1.5}
              d={`M ${object.x} ${object.y - a} L ${object.x + a} ${object.y} L ${object.x} ${object.y + a} L ${object.x - a} ${object.y} Z`}
            />
            {moving && <line className="object-marker" stroke={color} strokeWidth={stroke * 1.5} {...trail} />}
          </> : namedStar ? <path
            className="object-marker"
            stroke={color}
            strokeWidth={stroke}
            d={`M ${object.x - a} ${object.y} H ${object.x - a / 3} M ${object.x + a / 3} ${object.y} H ${object.x + a}`}
          /> : <ellipse
            className="object-marker"
            stroke={color}
            strokeWidth={stroke}
            cx={0}
            cy={0}
            rx={a}
            ry={b}
            transform={`translate(${object.x} ${object.y}) rotate(${object.angle_deg})`}
          />}
          <text
            className="overlay-label"
            fill={color}
            x={object.x}
            y={labelY(object)}
            textAnchor="middle"
            fontSize={fontSize}
          >{labelText(object)}</text>
        </g>
      })}
    </g>
    <g className="solution-center" strokeWidth={stroke}>
      <circle cx={width / 2} cy={height / 2} r={fontSize} />
      <path d={`M ${width / 2 - fontSize * 1.7} ${height / 2} H ${width / 2 + fontSize * 1.7} M ${width / 2} ${height / 2 - fontSize * 1.7} V ${height / 2 + fontSize * 1.7}`} />
    </g>
  </svg>
}

function layerVisible(object: OverlayObject, layers: OverlayLayers) {
  if (object.kind === 'field-star') return layers.fieldStars
  if (object.kind === 'star' || object.kind === 'double-star') return layers.namedStars
  if (object.kind === 'transient') {
    return layers.transients && (object.near_capture !== false || layers.historicalTransients)
  }
  if (object.kind === 'comet' || object.kind === 'asteroid') return layers.minorBodies
  return layers.deepSky
}

function encompassesFrame(object: OverlayObject, width: number, height: number) {
  if (object.semi_major_px <= 0) return false
  const radians = object.angle_deg * Math.PI / 180
  const cos = Math.cos(radians)
  const sin = Math.sin(radians)
  return [[0, 0], [width, 0], [width, height], [0, height]].every(([x, y]) => {
    const dx = x - object.x
    const dy = y - object.y
    const u = (dx * cos + dy * sin) / object.semi_major_px
    const v = (-dx * sin + dy * cos) / Math.max(object.semi_minor_px, 1)
    return u * u + v * v <= 1
  })
}

interface GridCurve {
  path: string
  label: string
  x: number
  y: number
  anchor: 'start' | 'middle'
}

function makeGrid(solution: Solution): GridCurve[] {
  const width = solution.image_width
  const height = solution.image_height
  const fontSize = Math.max(width / 90, 14)
  const centerRa = pixelToWorld(solution, width / 2, height / 2)[0]
  let raMin = Number.POSITIVE_INFINITY
  let raMax = Number.NEGATIVE_INFINITY
  let decMin = Number.POSITIVE_INFINITY
  let decMax = Number.NEGATIVE_INFINITY
  for (let xIndex = 0; xIndex <= 8; xIndex += 1) {
    for (let yIndex = 0; yIndex <= 8; yIndex += 1) {
      const [ra, dec] = pixelToWorld(solution, width * xIndex / 8, height * yIndex / 8)
      const unwrappedRa = centerRa + modulo(ra - centerRa + 540, 360) - 180
      raMin = Math.min(raMin, unwrappedRa)
      raMax = Math.max(raMax, unwrappedRa)
      decMin = Math.min(decMin, dec)
      decMax = Math.max(decMax, dec)
    }
  }
  const cosDec = Math.max(Math.abs(Math.cos(solution.center_dec_deg * Math.PI / 180)), 0.05)
  const span = Math.max(decMax - decMin, (raMax - raMin) * cosDec, solution.pixel_scale_arcsec_per_pixel / 3600)
  const decStep = niceGridStep(span / 6)
  const raStep = niceGridStep(span / cosDec / 6)
  const curves: GridCurve[] = []
  for (let ra = Math.floor(raMin / raStep) * raStep, count = 0; ra <= raMax + raStep && count < 32; ra += raStep, count += 1) {
    const samples = sampleCurve(decMin - decStep, decMax + decStep, (dec) => worldToPixel(solution, modulo(ra, 360), Math.max(-89.999999, Math.min(89.999999, dec))))
    const curve = gridCurve(samples, width, height, formatRa(modulo(ra, 360)), 'ra', fontSize)
    if (curve) curves.push(curve)
  }
  for (let dec = Math.floor(decMin / decStep) * decStep, count = 0; dec <= decMax + decStep && dec <= 90 && count < 32; dec += decStep, count += 1) {
    if (dec < -90) continue
    const samples = sampleCurve(raMin - raStep, raMax + raStep, (ra) => worldToPixel(solution, modulo(ra, 360), Math.max(-89.999999, Math.min(89.999999, dec))))
    const curve = gridCurve(samples, width, height, formatDec(dec), 'dec', fontSize)
    if (curve) curves.push(curve)
  }
  return curves
}

function sampleCurve(start: number, end: number, project: (coordinate: number) => [number, number] | null) {
  return Array.from({ length: 97 }, (_, index) => project(start + (end - start) * index / 96))
}

function gridCurve(
  samples: Array<[number, number] | null>,
  width: number,
  height: number,
  label: string,
  axis: 'ra' | 'dec',
  fontSize: number,
): GridCurve | null {
  const commands: string[] = []
  const inFrame: Array<[number, number]> = []
  let penDown = false
  for (const sample of samples) {
    if (!sample || sample[0] < -4 * width || sample[0] > 5 * width || sample[1] < -4 * height || sample[1] > 5 * height) {
      penDown = false
      continue
    }
    commands.push(`${penDown ? 'L' : 'M'}${sample[0].toFixed(2)},${sample[1].toFixed(2)}`)
    penDown = true
    if (sample[0] >= 4 && sample[0] <= width - 4 && sample[1] >= 4 && sample[1] <= height - 4) inFrame.push(sample)
  }
  if (commands.length < 2 || inFrame.length === 0) return null
  const point = inFrame.reduce((best, candidate) =>
    axis === 'ra'
      ? (candidate[1] < best[1] ? candidate : best)
      : (candidate[0] < best[0] ? candidate : best),
  )
  const padding = Math.max(4, fontSize * 0.25)
  const labelWidth = label.length * fontSize * 0.64
  const minimumBaseline = padding + fontSize
  const maximumBaseline = height - padding - fontSize * 0.2
  return {
    path: commands.join(' '),
    label,
    x: axis === 'ra'
      ? clamp(point[0], padding + labelWidth / 2, width - padding - labelWidth / 2)
      : clamp(point[0] + padding, padding, width - padding - labelWidth),
    y: clamp(
      axis === 'ra' ? point[1] + fontSize * 1.25 : point[1] - padding,
      minimumBaseline,
      maximumBaseline,
    ),
    anchor: axis === 'ra' ? 'middle' : 'start',
  }
}

function pixelToWorld(solution: Solution, x: number, y: number): [number, number] {
  const { crval, crpix, cd } = solution.wcs
  const dx = x - crpix[0]
  const dy = y - crpix[1]
  const xi = (cd[0][0] * dx + cd[0][1] * dy) * Math.PI / 180
  const eta = (cd[1][0] * dx + cd[1][1] * dy) * Math.PI / 180
  const ra0 = crval[0] * Math.PI / 180
  const dec0 = crval[1] * Math.PI / 180
  const rho = Math.hypot(xi, eta)
  if (rho === 0) return [crval[0], crval[1]]
  const c = Math.atan(rho)
  const dec = Math.asin(Math.cos(c) * Math.sin(dec0) + eta * Math.sin(c) * Math.cos(dec0) / rho)
  const ra = ra0 + Math.atan2(xi * Math.sin(c), rho * Math.cos(dec0) * Math.cos(c) - eta * Math.sin(dec0) * Math.sin(c))
  return [modulo(ra * 180 / Math.PI, 360), dec * 180 / Math.PI]
}

function worldToPixel(solution: Solution, raDegrees: number, decDegrees: number): [number, number] | null {
  const { crval, crpix, cd } = solution.wcs
  const ra0 = crval[0] * Math.PI / 180
  const dec0 = crval[1] * Math.PI / 180
  const ra = raDegrees * Math.PI / 180
  const dec = decDegrees * Math.PI / 180
  const deltaRa = ra - ra0
  const cosC = Math.sin(dec0) * Math.sin(dec) + Math.cos(dec0) * Math.cos(dec) * Math.cos(deltaRa)
  if (cosC <= 1e-9) return null
  const xi = Math.cos(dec) * Math.sin(deltaRa) / cosC * 180 / Math.PI
  const eta = (Math.cos(dec0) * Math.sin(dec) - Math.sin(dec0) * Math.cos(dec) * Math.cos(deltaRa)) / cosC * 180 / Math.PI
  const determinant = cd[0][0] * cd[1][1] - cd[0][1] * cd[1][0]
  if (determinant === 0) return null
  return [
    crpix[0] + (cd[1][1] * xi - cd[0][1] * eta) / determinant,
    crpix[1] + (-cd[1][0] * xi + cd[0][0] * eta) / determinant,
  ]
}

const gridSteps = [1 / 3600, 2 / 3600, 5 / 3600, 10 / 3600, 15 / 3600, 30 / 3600, 1 / 60, 2 / 60, 5 / 60, 10 / 60, 15 / 60, 30 / 60, 1, 2, 5, 10, 15, 30, 45, 90]
function niceGridStep(target: number) { return gridSteps.find((step) => step >= target) ?? 90 }
function modulo(value: number, divisor: number) { return ((value % divisor) + divisor) % divisor }
function clamp(value: number, minimum: number, maximum: number) { return Math.max(Math.min(value, Math.max(minimum, maximum)), Math.min(minimum, maximum)) }

function formatRa(ra: number) {
  const totalTenths = Math.round(modulo(ra, 360) / 15 * 36_000) % 864_000
  const hours = Math.floor(totalTenths / 36_000)
  const minutes = Math.floor((totalTenths % 36_000) / 600)
  const seconds = totalTenths % 600
  return `RA ${String(hours).padStart(2, '0')}h${String(minutes).padStart(2, '0')}m${String(Math.floor(seconds / 10)).padStart(2, '0')}.${seconds % 10}s`
}

function formatDec(dec: number) {
  const totalTenths = Math.round(Math.abs(dec) * 36_000)
  const degrees = Math.floor(totalTenths / 36_000)
  const minutes = Math.floor((totalTenths % 36_000) / 600)
  const seconds = totalTenths % 600
  return `Dec ${dec < 0 ? '−' : '+'}${String(degrees).padStart(2, '0')}°${String(minutes).padStart(2, '0')}′${String(Math.floor(seconds / 10)).padStart(2, '0')}.${seconds % 10}″`
}
