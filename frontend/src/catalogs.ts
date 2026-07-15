import type { OverlayObject } from './api'

export type DeepSkyCatalogId =
  | 'ngc-ic-messier'
  | 'sharpless-vdb'
  | 'lbn'
  | 'cederblad'
  | 'dark-nebulae'
  | 'snr'
  | 'ugc'
  | 'pgc'
  | 'other-deep-sky'

export const deepSkyCatalogs: ReadonlyArray<readonly [DeepSkyCatalogId, string]> = [
  ['ngc-ic-messier', 'NGC / IC / Messier'],
  ['sharpless-vdb', 'Sharpless / vdB'],
  ['lbn', 'LBN (bright nebulae)'],
  ['cederblad', 'Cederblad'],
  ['dark-nebulae', 'Dark nebulae (B / LDN)'],
  ['snr', 'Supernova remnants'],
  ['ugc', 'UGC galaxies'],
  ['pgc', 'PGC galaxies'],
  ['other-deep-sky', 'Other deep sky'],
]

const nonDeepSkyKinds = new Set([
  'star',
  'double-star',
  'identified-star',
  'field-star',
  'transient',
  'comet',
  'asteroid',
])

/** Mirrors Tenrankai's designation-based catalog grouping for deep-sky objects. */
export function deepSkyCatalogForObject(object: OverlayObject): DeepSkyCatalogId | null {
  if (nonDeepSkyKinds.has(object.kind)) return null
  const name = object.name.trim()
  if (/^PGC(?:\s|$)/i.test(name)) return 'pgc'
  if (/^UGC(?:\s|$)/i.test(name)) return 'ugc'
  if (/^LBN(?:\s|$)/i.test(name)) return 'lbn'
  if (/^(?:Ced|Cederblad)(?:\s|$)/i.test(name)) return 'cederblad'
  if (/^(?:LDN(?:\s|$)|B\s*\d)/i.test(name)) return 'dark-nebulae'
  if (/^SNR(?:\s|$)/i.test(name)) return 'snr'
  if (/^(?:Sh\s*2[- ]|vdB(?:\s|$))/i.test(name)) return 'sharpless-vdb'
  if (/^(?:NGC|IC|M)\s*\d/i.test(name)) return 'ngc-ic-messier'
  return 'other-deep-sky'
}
