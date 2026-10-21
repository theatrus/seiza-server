import { useCallback, useEffect, useRef, useState, type RefObject } from 'react'

interface Point { x: number; y: number }

export interface ImageZoom {
  /** Magnification over the fitted size; 1 is fitted. */
  zoom: number
  /** Most the image can be magnified. */
  maxZoom: number
  /** The image's displacement from the stage's centre, in screen pixels. */
  offset: Point
  /** The fitted size, in screen pixels, measured at zoom 1. */
  fitted: { width: number; height: number } | null
  /** Zoom by `factor`, keeping the screen point `at` (default the centre) fixed. */
  zoomBy: (factor: number, at?: Point) => void
  reset: () => void
}

/**
 * Sent on the stage for a press that ends where it began: a click or tap,
 * not the end of a drag or pinch. The expanded view pins positions on it.
 */
export const SKY_TAP_EVENT = 'seiza-sky-tap'

export interface SkyTap {
  clientX: number
  clientY: number
  target: EventTarget | null
}

/** Screen pixels a press may move and still count as a click. */
const CLICK_SLOP_PX = 4
/** Screen pixels of the image that panning always leaves in view. */
const KEEP_IN_VIEW_PX = 80
/** At the most zoom, one image pixel spans this many screen pixels. */
const SCREEN_PX_PER_IMAGE_PX = 4

/**
 * Scroll, pinch and drag zoom for an image filling `stage`: the wheel (and a
 * trackpad pinch, which browsers send as a wheel) zooms about the pointer,
 * two touches pinch, and a press dragged further than a click pans. A press
 * that doesn't move is announced as a [`SKY_TAP_EVENT`], so a pan never pins
 * a position. The keys + and − zoom about the centre and 0 fits the image
 * again.
 */
export function useImageZoom(
  stageRef: RefObject<HTMLElement | null>,
  imageRef: RefObject<HTMLImageElement | null>,
  imageWidth: number,
  enabled: boolean,
): ImageZoom {
  const [zoom, setZoom] = useState(1)
  const [offset, setOffset] = useState<Point>({ x: 0, y: 0 })
  const [fitted, setFitted] = useState<{ width: number; height: number } | null>(null)
  const view = useRef({ zoom: 1, offset: { x: 0, y: 0 } })
  const maxZoom = fitted ? Math.max(1, imageWidth * SCREEN_PX_PER_IMAGE_PX / fitted.width) : 1

  const apply = useCallback((nextZoom: number, nextOffset: Point) => {
    const stage = stageRef.current?.getBoundingClientRect()
    if (!stage || !fitted) return
    const zoomValue = Math.min(Math.max(nextZoom, 1), maxZoom)
    // Keep some of the image in view, however it is moved; a tighter
    // bound would push the point under the pointer away as the image grows
    // past the stage's size. At zoom 1 it is centred.
    const limit = (size: number, stageSize: number) => zoomValue === 1
      ? 0
      : Math.max(0, (size * zoomValue + stageSize) / 2 - KEEP_IN_VIEW_PX)
    const limitX = limit(fitted.width, stage.width)
    const limitY = limit(fitted.height, stage.height)
    const clamped = {
      x: Math.min(Math.max(nextOffset.x, -limitX), limitX),
      y: Math.min(Math.max(nextOffset.y, -limitY), limitY),
    }
    view.current = { zoom: zoomValue, offset: clamped }
    setZoom(zoomValue)
    setOffset(clamped)
  }, [stageRef, fitted, maxZoom])

  const zoomBy = useCallback((factor: number, at?: Point) => {
    const stage = stageRef.current?.getBoundingClientRect()
    if (!stage) return
    const { zoom: current, offset: shift } = view.current
    const next = Math.min(Math.max(current * factor, 1), maxZoom)
    const k = next / current
    const centre = { x: stage.left + stage.width / 2, y: stage.top + stage.height / 2 }
    const point = at ?? centre
    // The image point under `point` stays under it: its offset from the
    // image's centre grows by k.
    apply(next, {
      x: shift.x + (point.x - centre.x - shift.x) * (1 - k),
      y: shift.y + (point.y - centre.y - shift.y) * (1 - k),
    })
  }, [stageRef, apply, maxZoom])

  const reset = useCallback(() => {
    view.current = { zoom: 1, offset: { x: 0, y: 0 } }
    setZoom(1)
    setOffset({ x: 0, y: 0 })
  }, [])

  // Measure the fitted size while the image is at zoom 1, and again when
  // the window changes size.
  useEffect(() => {
    if (!enabled) return
    const measure = () => {
      if (view.current.zoom !== 1) return
      const box = imageRef.current?.getBoundingClientRect()
      if (box && box.width > 0 && box.height > 0) setFitted({ width: box.width, height: box.height })
    }
    const image = imageRef.current
    measure()
    image?.addEventListener('load', measure)
    const resize = () => {
      reset()
      requestAnimationFrame(measure)
    }
    window.addEventListener('resize', resize)
    return () => {
      image?.removeEventListener('load', measure)
      window.removeEventListener('resize', resize)
    }
  }, [enabled, imageRef, reset])

  useEffect(() => {
    if (!enabled) return
    const stage = stageRef.current
    if (!stage) return
    const pointers = new Map<number, Point>()
    let start: { point: Point; offset: Point } | null = null
    let moved = false
    let pinch: { distance: number; middle: Point } | null = null

    const wheel = (event: WheelEvent) => {
      event.preventDefault()
      const lines = event.deltaMode === WheelEvent.DOM_DELTA_LINE
      zoomBy(Math.exp(-event.deltaY * (lines ? 0.05 : 0.0015)), { x: event.clientX, y: event.clientY })
    }
    const down = (event: PointerEvent) => {
      if ((event.target as Element | null)?.closest('button, a, .sky-pointer-card')) return
      // A primary press starts a new gesture: no other pointer is down, even
      // if one was lifted without an event reaching the stage.
      if (event.isPrimary) {
        pointers.clear()
        pinch = null
      }
      pointers.set(event.pointerId, { x: event.clientX, y: event.clientY })
      if (pointers.size === 1) {
        start = { point: { x: event.clientX, y: event.clientY }, offset: view.current.offset }
        moved = false
      } else if (pointers.size === 2) {
        const [a, b] = [...pointers.values()]
        pinch = { distance: Math.hypot(a.x - b.x, a.y - b.y), middle: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 } }
        moved = true
      }
    }
    const move = (event: PointerEvent) => {
      if (!pointers.has(event.pointerId)) return
      pointers.set(event.pointerId, { x: event.clientX, y: event.clientY })
      if (pointers.size >= 2 && pinch) {
        const [a, b] = [...pointers.values()]
        const distance = Math.hypot(a.x - b.x, a.y - b.y)
        const middle = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }
        if (pinch.distance > 0) zoomBy(distance / pinch.distance, middle)
        const shifted = view.current.offset
        apply(view.current.zoom, { x: shifted.x + middle.x - pinch.middle.x, y: shifted.y + middle.y - pinch.middle.y })
        pinch = { distance, middle }
        return
      }
      if (!start) return
      const dx = event.clientX - start.point.x
      const dy = event.clientY - start.point.y
      if (!moved && Math.hypot(dx, dy) > CLICK_SLOP_PX) {
        moved = true
        stage.setPointerCapture?.(event.pointerId)
      }
      if (moved && view.current.zoom > 1) apply(view.current.zoom, { x: start.offset.x + dx, y: start.offset.y + dy })
    }
    const up = (event: PointerEvent) => {
      pointers.delete(event.pointerId)
      if (pointers.size < 2) pinch = null
      if (pointers.size === 1) {
        const [rest] = [...pointers.values()]
        start = { point: rest, offset: view.current.offset }
        return
      }
      if (pointers.size === 0) {
        // A press that ends where it began, of one pointer, is a tap on
        // the sky; a drag or a pinch is not.
        if (!moved && start) {
          stage.dispatchEvent(new CustomEvent<SkyTap>(SKY_TAP_EVENT, {
            detail: { clientX: event.clientX, clientY: event.clientY, target: event.target },
          }))
        }
        start = null
      }
    }
    const key = (event: KeyboardEvent) => {
      if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) return
      if (event.key === '+' || event.key === '=') zoomBy(1.5)
      else if (event.key === '-' || event.key === '_') zoomBy(1 / 1.5)
      else if (event.key === '0') reset()
      else return
      event.preventDefault()
    }
    stage.addEventListener('wheel', wheel, { passive: false })
    stage.addEventListener('pointerdown', down)
    stage.addEventListener('pointermove', move)
    stage.addEventListener('pointerup', up)
    stage.addEventListener('pointercancel', up)
    window.addEventListener('keydown', key)
    return () => {
      stage.removeEventListener('wheel', wheel)
      stage.removeEventListener('pointerdown', down)
      stage.removeEventListener('pointermove', move)
      stage.removeEventListener('pointerup', up)
      stage.removeEventListener('pointercancel', up)
      window.removeEventListener('keydown', key)
      // Leaving the expanded view, or a new image, starts again fitted.
      reset()
    }
  }, [enabled, stageRef, zoomBy, apply, reset])

  return { zoom, maxZoom, offset, fitted, zoomBy, reset }
}
