import type { OverlayObject } from './api'

export type DeepSkyCatalogId =
  | 'messier'
  | 'ngc'
  | 'ic'
  | 'sharpless-vdb'
  | 'lbn'
  | 'cederblad'
  | 'dark-nebulae'
  | 'snr'
  | 'ugc'
  | 'pgc'
  | 'other-deep-sky'

export const deepSkyCatalogs: ReadonlyArray<readonly [DeepSkyCatalogId, string]> = [
  ['messier', 'Messier'],
  ['ngc', 'NGC'],
  ['ic', 'IC'],
  ['sharpless-vdb', 'Sharpless / vdB'],
  ['lbn', 'LBN (bright nebulae)'],
  ['cederblad', 'Cederblad'],
  ['dark-nebulae', 'Dark nebulae (B / LDN)'],
  ['snr', 'Supernova remnants'],
  ['ugc', 'UGC galaxies'],
  ['pgc', 'PGC galaxies'],
  ['other-deep-sky', 'Other / default catalogs'],
]

/**
 * A deliberately restrained catalog palette: related catalogs stay in the same
 * cool family, with only dark nebulae and remnants receiving distinct accents.
 */
export const deepSkyCatalogColors: Readonly<Record<DeepSkyCatalogId, string>> = Object.freeze({
  messier: '#f2ca72',
  ngc: '#55cfff',
  ic: '#72dfb9',
  'sharpless-vdb': '#ee9a78',
  lbn: '#a2d96f',
  cederblad: '#70d7d0',
  'dark-nebulae': '#b4a3f0',
  snr: '#f18782',
  ugc: '#79aff5',
  pgc: '#a1aed8',
  'other-deep-sky': '#c1d1d3',
})

export function deepSkyCatalogLayer(catalog: DeepSkyCatalogId): string {
  return `deep-sky:${catalog}`
}

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
export function deepSkyCatalogForObject(
  object: Pick<OverlayObject, 'kind' | 'name'>,
): DeepSkyCatalogId | null {
  if (nonDeepSkyKinds.has(object.kind)) return null
  const name = object.name.trim()
  if (/^PGC(?:\s|$)/i.test(name)) return 'pgc'
  if (/^UGC(?:\s|$)/i.test(name)) return 'ugc'
  if (/^LBN(?:\s|$)/i.test(name)) return 'lbn'
  if (/^(?:Ced|Cederblad)(?:\s|$)/i.test(name)) return 'cederblad'
  if (/^(?:LDN(?:\s|$)|B\s*\d)/i.test(name)) return 'dark-nebulae'
  if (/^SNR(?:\s|$)/i.test(name)) return 'snr'
  if (/^(?:Sh\s*2[- ]|vdB(?:\s|$))/i.test(name)) return 'sharpless-vdb'
  if (/^M\s*\d/i.test(name)) return 'messier'
  if (/^NGC\s*\d/i.test(name)) return 'ngc'
  if (/^IC\s*\d/i.test(name)) return 'ic'
  return 'other-deep-sky'
}
