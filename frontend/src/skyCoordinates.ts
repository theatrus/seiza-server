import type { Solution } from './api'

type Wcs = Solution['wcs']

/**
 * The sky position of an image pixel, in degrees. Pixels count from 0 with
 * their centres on whole numbers, as the solution's CRPIX does, and SIP
 * distortion applies before the gnomonic projection, as in Seiza.
 */
export function skyAtPixel(wcs: Wcs, x: number, y: number): [ra: number, dec: number] {
  let u = x - wcs.crpix[0]
  let v = y - wcs.crpix[1]
  if (wcs.sip) {
    const f = sipSum(wcs.sip.a, u, v)
    const g = sipSum(wcs.sip.b, u, v)
    u += f
    v += g
  }
  const xi = (wcs.cd[0][0] * u + wcs.cd[0][1] * v) * Math.PI / 180
  const eta = (wcs.cd[1][0] * u + wcs.cd[1][1] * v) * Math.PI / 180
  const rho = Math.hypot(xi, eta)
  if (rho === 0) return [wcs.crval[0], wcs.crval[1]]
  const ra0 = wcs.crval[0] * Math.PI / 180
  const dec0 = wcs.crval[1] * Math.PI / 180
  const c = Math.atan(rho)
  const dec = Math.asin(Math.cos(c) * Math.sin(dec0) + eta * Math.sin(c) * Math.cos(dec0) / rho)
  const ra = ra0 + Math.atan2(
    xi * Math.sin(c),
    rho * Math.cos(dec0) * Math.cos(c) - eta * Math.sin(dec0) * Math.sin(c),
  )
  return [((ra * 180 / Math.PI) % 360 + 360) % 360, dec * 180 / Math.PI]
}

function sipSum(terms: Array<[number, number, number]>, u: number, v: number) {
  return terms.reduce((sum, [p, q, value]) => sum + value * u ** p * v ** q, 0)
}

/** Right ascension as hours, minutes and seconds to a hundredth. */
export function sexagesimalRa(ra: number) {
  const hundredths = Math.round(((ra % 360) + 360) % 360 / 15 * 360_000) % 8_640_000
  const hours = Math.floor(hundredths / 360_000)
  const minutes = Math.floor(hundredths % 360_000 / 6_000)
  const seconds = (hundredths % 6_000) / 100
  return `${pad(hours)}h ${pad(minutes)}m ${seconds.toFixed(2).padStart(5, '0')}s`
}

/** Declination as degrees, arcminutes and arcseconds to a tenth. */
export function sexagesimalDec(dec: number) {
  const tenths = Math.round(Math.abs(dec) * 36_000)
  const degrees = Math.floor(tenths / 36_000)
  const minutes = Math.floor(tenths % 36_000 / 600)
  const seconds = (tenths % 600) / 10
  return `${dec < 0 ? '−' : '+'}${pad(degrees)}° ${pad(minutes)}′ ${seconds.toFixed(1).padStart(4, '0')}″`
}

function pad(value: number) {
  return String(value).padStart(2, '0')
}

export interface CatalogLookup {
  name: string
  description: string
  url: string
}

/**
 * Links that search the big catalog services around a position. The radius
 * is in arcminutes, and the image viewers open a field eight radii across.
 */
export function catalogLookups(ra: number, dec: number, radiusArcmin: number): CatalogLookup[] {
  const raText = ra.toFixed(6)
  const decText = `${dec < 0 ? '-' : '+'}${Math.abs(dec).toFixed(6)}`
  const position = encodeURIComponent(`${raText} ${decText}`)
  const radius = radiusArcmin.toPrecision(3)
  const fovDeg = Math.min(Math.max(radiusArcmin * 8 / 60, 0.02), 60).toPrecision(3)
  return [
    {
      name: 'SIMBAD',
      description: 'Named objects and their literature, from CDS',
      url: `https://simbad.cds.unistra.fr/simbad/sim-coo?Coord=${position}&CooFrame=ICRS&CooEpoch=2000&CooEqui=2000&Radius=${radius}&Radius.unit=arcmin&submit=submit%20query`,
    },
    {
      name: 'NED',
      description: 'Galaxies and other objects beyond the Milky Way, from NASA/IPAC',
      url: `https://ned.ipac.caltech.edu/cgi-bin/objsearch?search_type=Near+Position+Search&in_csys=Equatorial&in_equinox=J2000.0&lon=${raText}d&lat=${encodeURIComponent(decText)}d&radius=${radius}`,
    },
    {
      name: 'VizieR',
      description: 'Every published catalog with an entry here, from CDS',
      url: `https://vizier.cds.unistra.fr/viz-bin/VizieR?-c=${position}&-c.rm=${radius}`,
    },
    {
      name: 'Aladin Lite',
      description: 'Survey images of this field, from CDS',
      url: `https://aladin.cds.unistra.fr/AladinLite/?target=${position}&fov=${fovDeg}`,
    },
    {
      name: 'ESASky',
      description: 'Images and observations from ESA and other missions',
      url: `https://sky.esa.int/esasky/?target=${position}&fov=${fovDeg}&cooframe=J2000`,
    },
  ]
}
