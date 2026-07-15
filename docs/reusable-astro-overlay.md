# Reusable astrometry overlay package

The SVG overlay and browser PNG export are shared application infrastructure,
not seiza-server page components. Their canonical home is the dedicated
[`theatrus/seiza-overlay`](https://github.com/theatrus/seiza-overlay)
repository, which publishes `@seiza/astro-overlay` under Apache-2.0.
Seiza-server consumes the exact published
`@seiza/astro-overlay@0.1.1` package from npm rather than carrying a vendored
copy.

## Package boundary

The package owns:

- the shared WCS, solution, and projected-object TypeScript contract;
- default semantic layers and object counting;
- prominence-based label-density selection from Tenrankai;
- TAN pixel/world transforms and unclipped RA/Dec grid geometry from
  seiza-server;
- deep-sky, stellar, transient, comet, asteroid, field-star, and center marker
  SVG geometry;
- label collision handling and frame-encompassing captions; and
- live-SVG serialization plus browser canvas PNG compositing.

The consuming application owns:

- HTTP requests, caching, progress, and error states;
- buttons, menus, control placement, and preference persistence;
- image zoom/pan layout and the transformed container holding image plus SVG;
- the catalog-to-layer resolver when its groups differ from the defaults; and
- branding, watermarks, and other PNG decorations.

The split is deliberate. Tenrankai can retain its catalog dropdown and density
slider, seiza-server can retain its explicit layer buttons, and PSF Guard can
place controls in its image-detail toolbar without forking the rendering code.

## Public entry points

| Import | Responsibility |
| --- | --- |
| `@seiza/astro-overlay` | Types, layer selection, density, WCS and grid geometry |
| `@seiza/astro-overlay/react` | SVG-only `AstroOverlay` component |
| `@seiza/astro-overlay/export` | SVG serialization, raster compositing, PNG download helper |

The SVG exposes stable `seiza-overlay__*` classes and CSS custom properties.
The React `theme` prop writes those variables inline so the same values survive
SVG serialization. External layout CSS may position the SVG anywhere; the
component itself does not set absolute positioning or z-index.

`defaultOverlayTheme` and `defaultOverlayDensity` expose the production tuning
as typed configuration. Applications can spread the theme and override stroke
widths, label and grid font weights, halo width, colors, opacity, dash patterns,
or font families. The default density is `0.6`; callers that need every ranked
object can pass `density={1}`.

## Application adapters

Seiza-server already speaks the package's canonical `image_width`,
`image_height`, `wcs`, and `objects` response. Its local adapter only translates
camel-case UI toggle state to semantic snake-case layer IDs.

PSF Guard's in-flight `AstrometrySolutionResponse` is a compatible superset,
including stable IDs, aliases, hierarchy, and provenance. It can consume the
component directly when its WCS phase lands.

Tenrankai currently returns `width`, `height`, `scale_arcsec_px`, and a reduced
object shape. Its fetch hook should normalize those three field names once and
provide a WCS when available. Its name-prefix catalog grouping belongs in an
application `layerForObject` callback; its controls remain unchanged.

## Release and adoption

The standalone repository owns CI, Dependabot, and a guarded manual release
workflow. Version 0.1.1 is published with npm provenance and is consumed by
seiza-server from the registry. Tenrankai and PSF Guard can adopt releases
independently without coupling their application release cycles.

Keeping a single package with subpath exports is preferable to three packages
at this size: it preserves one versioned geometry contract while React remains
an optional peer dependency for consumers that only need core calculations or
PNG export.
