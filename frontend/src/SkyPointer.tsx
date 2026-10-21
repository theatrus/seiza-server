import { useEffect, useState, type MouseEvent, type RefObject } from 'react'
import type { Solution } from './api'
import { catalogLookups, sexagesimalDec, sexagesimalRa, skyAtPixel } from './skyCoordinates'

interface SkyPoint {
  /** Image pixel, counted from 0. */
  x: number
  y: number
  ra: number
  dec: number
  /** Position in the frame, 0 to 1, for placing the readout. */
  left: number
  top: number
  /** Search radius in arcminutes: a few screen pixels of sky. */
  radiusArcmin: number
}

/** Screen pixels of sky a lookup searches around the pointer. */
const LOOKUP_RADIUS_SCREEN_PX = 8

/**
 * The sky position under the pointer, drawn over a solved image. In the
 * expanded view a click pins the position and offers searches of the big
 * catalog services there; the smaller view keeps its click to expand.
 * Give each view its own key, so a pin does not outlive the view it was
 * made in.
 */
export function SkyPointer({
  solution,
  frameRef,
  pinnable,
}: {
  solution: Solution
  frameRef: RefObject<HTMLDivElement | null>
  pinnable: boolean
}) {
  const [hover, setHover] = useState<SkyPoint | null>(null)
  const [pinned, setPinned] = useState<SkyPoint | null>(null)

  useEffect(() => {
    const frame = frameRef.current
    if (!frame) return
    const pointAt = (event: globalThis.PointerEvent | globalThis.MouseEvent): SkyPoint | null => {
      const box = frame.getBoundingClientRect()
      if (box.width <= 0 || box.height <= 0) return null
      const left = (event.clientX - box.left) / box.width
      const top = (event.clientY - box.top) / box.height
      if (left < 0 || left > 1 || top < 0 || top > 1) return null
      const x = left * solution.image_width - 0.5
      const y = top * solution.image_height - 0.5
      // The bands around a screenshot's picture show no sky.
      const region = solution.content_region
      if (region && (x < region.x - 0.5 || y < region.y - 0.5
        || x > region.x + region.width - 0.5 || y > region.y + region.height - 0.5)) return null
      const [ra, dec] = skyAtPixel(solution.wcs, x, y)
      const imagePxPerScreenPx = solution.image_width / box.width
      const radiusArcmin = Math.min(Math.max(
        LOOKUP_RADIUS_SCREEN_PX * imagePxPerScreenPx * solution.pixel_scale_arcsec_per_pixel / 60,
        0.05,
      ), 30)
      return { x, y, ra, dec, left, top, radiusArcmin }
    }
    const move = (event: globalThis.PointerEvent) => {
      if (event.pointerType === 'touch') return
      if ((event.target as Element | null)?.closest('.sky-pointer-card')) {
        setHover(null)
        return
      }
      setHover(pointAt(event))
    }
    const leave = () => setHover(null)
    const click = (event: globalThis.MouseEvent) => {
      if (!pinnable || (event.target as Element | null)?.closest('.sky-pointer-card')) return
      setPinned(pointAt(event))
    }
    frame.addEventListener('pointermove', move)
    frame.addEventListener('pointerleave', leave)
    frame.addEventListener('click', click)
    return () => {
      frame.removeEventListener('pointermove', move)
      frame.removeEventListener('pointerleave', leave)
      frame.removeEventListener('click', click)
    }
  }, [frameRef, solution, pinnable])

  useEffect(() => {
    if (!pinned) return
    const key = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setPinned(null)
    }
    window.addEventListener('keydown', key)
    return () => window.removeEventListener('keydown', key)
  }, [pinned])

  return <>
    {hover && <div
      className="sky-pointer-readout"
      style={placement(hover)}
      aria-hidden="true"
    >
      <span>RA {sexagesimalRa(hover.ra)}</span>
      <span>Dec {sexagesimalDec(hover.dec)}</span>
      <span className="sky-pointer-pixel">x {Math.round(hover.x)} · y {Math.round(hover.y)}</span>
      <span className="sky-pointer-hint">{pinnable ? 'Click to look this up' : 'Click to expand'}</span>
    </div>}
    {pinned && <>
      <span className="sky-pointer-pin" style={{ left: `${pinned.left * 100}%`, top: `${pinned.top * 100}%` }} aria-hidden="true" />
      <PinnedCard key={`${pinned.x},${pinned.y}`} point={pinned} onClose={() => setPinned(null)} />
    </>}
  </>
}

function PinnedCard({ point, onClose }: { point: SkyPoint; onClose: () => void }) {
  const [copied, setCopied] = useState(false)
  const lookups = catalogLookups(point.ra, point.dec, point.radiusArcmin)
  const decimal = `${point.ra.toFixed(6)} ${point.dec >= 0 ? '+' : ''}${point.dec.toFixed(6)}`
  const copy = (event: MouseEvent<HTMLButtonElement>) => {
    event.stopPropagation()
    void navigator.clipboard?.writeText(decimal).then(() => setCopied(true), () => setCopied(false))
  }
  return <div
    className="sky-pointer-card"
    style={placement(point)}
    role="dialog"
    aria-label="Sky position"
    onClick={(event) => event.stopPropagation()}
  >
    <div className="sky-pointer-card-heading">
      <strong>Sky position</strong>
      <button type="button" className="sky-pointer-close" aria-label="Close sky position" onClick={onClose}>×</button>
    </div>
    <dl>
      <div><dt>RA</dt><dd>{sexagesimalRa(point.ra)}</dd></div>
      <div><dt>Dec</dt><dd>{sexagesimalDec(point.dec)}</dd></div>
      <div><dt>Degrees</dt><dd>{decimal}</dd></div>
      <div><dt>Pixel</dt><dd>x {point.x.toFixed(1)} · y {point.y.toFixed(1)}</dd></div>
    </dl>
    <button type="button" className="button small sky-pointer-copy" onClick={copy}>{copied ? 'Copied' : 'Copy degrees'}</button>
    <p className="sky-pointer-lookup-heading">Search within {formatRadius(point.radiusArcmin)}</p>
    <ul className="sky-pointer-lookups">
      {lookups.map((lookup) => <li key={lookup.name}>
        <a href={lookup.url} target="_blank" rel="noopener noreferrer" title={lookup.description}>{lookup.name}</a>
      </li>)}
    </ul>
  </div>
}

function formatRadius(arcmin: number) {
  return arcmin < 1 ? `${(arcmin * 60).toPrecision(2)}″` : `${arcmin.toPrecision(2)}′`
}

/** Put a box beside the point, on whichever side keeps it in the frame. */
function placement(point: SkyPoint) {
  const right = point.left > 0.6
  const below = point.top < 0.55
  return {
    left: `${point.left * 100}%`,
    top: `${point.top * 100}%`,
    transform: `translate(${right ? 'calc(-100% - 14px)' : '14px'}, ${below ? '14px' : 'calc(-100% - 14px)'})`,
  }
}
