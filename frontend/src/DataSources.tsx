import type { ReactNode } from 'react'

type SourceLink = {
  label: string
  href: string
}

function SourceCard({ title, role, children, links }: {
  title: string
  role: string
  children: ReactNode
  links: SourceLink[]
}) {
  return <article className="source-card">
    <p className="source-role">{role}</p>
    <h3>{title}</h3>
    <div className="source-copy">{children}</div>
    <div className="source-links">
      {links.map((link) => <a key={link.href} href={link.href}>{link.label} <span aria-hidden="true">↗</span></a>)}
    </div>
  </article>
}

const vizierCatalogs: Array<SourceLink & { contribution: string }> = [
  { label: 'Sharpless H II regions · VII/20', contribution: 'Sharpless (1959) — Sh 2 nebulae and angular extents', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/20' },
  { label: 'Barnard dark objects · VII/220A', contribution: 'Barnard (1927) — dark nebulae and diameters', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/220A' },
  { label: 'Uppsala General Catalogue · VII/26D', contribution: 'Nilson (1973) — UGC galaxies, dimensions, and position angles', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/26D' },
  { label: 'Lynds Dark Nebulae · VII/7A', contribution: 'Lynds (1962) — LDN positions and areas', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/7A' },
  { label: 'van den Bergh reflection nebulae · VII/21', contribution: 'van den Bergh (1966) — vdB positions, radii, and magnitudes', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/21' },
  { label: 'Cederblad bright diffuse nebulae · VII/231', contribution: 'Cederblad (1946) — designations, names, classes, and dimensions', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/231' },
  { label: 'Lynds Bright Nebulae · VII/9', contribution: 'Lynds (1965) — LBN positions, dimensions, names, and cross-identifications', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/9' },
  { label: 'Bright Star Catalogue · V/50', contribution: 'Hoffleit & Warren (1991) — HD stars, traditional names, positions, and magnitudes', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/V/50' },
  { label: 'Principal Galaxies Catalogue · VII/237', contribution: 'Paturel et al. (2003) — PGC identifiers, dimensions, and position angles', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/237' },
  { label: 'Galactic supernova remnants · VII/284', contribution: 'Green (2019) — SNR names, positions, and extents', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/VII/284' },
  { label: 'Galactic Wolf–Rayet stars · III/215', contribution: 'van der Hucht (2001) — WR designations, positions, and cross-identifications', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/III/215' },
]

export function DataSourcesPage() {
  return <main className="sources-page">
    <header className="sources-hero">
      <p className="eyebrow">DATA SOURCES &amp; ACKNOWLEDGEMENTS</p>
      <h1>Built on generations of sky surveys.</h1>
      <p className="intro">Seiza can solve and label the sky because astronomers, survey teams, catalogue curators, observatories, and public archives have made meticulous data available. We are deeply grateful to everyone who created, maintains, and serves these resources.</p>
      <p className="source-license-note"><strong>Code and data are different.</strong> Seiza’s Apache-2.0 license covers Seiza software, not third-party catalogue data. Each upstream dataset retains its own terms, credit, and scientific citation requirements; this page is an acknowledgement, not a replacement for them.</p>
    </header>

    <section className="source-section" aria-labelledby="solving-sources">
      <div className="source-section-heading">
        <p className="eyebrow">PLATE SOLVING</p>
        <h2 id="solving-sources">The stars that anchor every solution.</h2>
      </div>
      <div className="source-grid two-up">
        <SourceCard
          title="Gaia Data Release 3"
          role="Primary deep solver catalogue"
          links={[
            { label: 'Gaia DR3 archive', href: 'https://gea.esac.esa.int/archive/' },
            { label: 'Who produced Gaia DR3', href: 'https://www.cosmos.esa.int/web/gaia/who-produced-the-dr3-data' },
            { label: 'Gaia DR3 citation', href: 'https://ui.adsabs.harvard.edu/abs/2023A%26A...674A...1G/abstract' },
          ]}
        >
          <p>ESA’s Gaia mission and the Gaia Data Processing and Analysis Consortium provide the astrometry and G-band photometry behind Seiza’s G≤15 and G≤17 star catalogues and the maintained G≤16 blind-pattern index.</p>
          <p className="formal-credit">Credit: ESA/Gaia/DPAC. Catalogue citation: Gaia Collaboration, Vallenari et al. (2023).</p>
        </SourceCard>
        <SourceCard
          title="Tycho-2"
          role="Lightweight solver and identifier catalogue"
          links={[
            { label: 'Tycho-2 · CDS I/259', href: 'https://cdsarc.cds.unistra.fr/viz-bin/cat/I/259' },
            { label: 'Tycho-2 paper', href: 'https://ui.adsabs.harvard.edu/abs/2000A%26A...355L..27H/abstract' },
          ]}
        >
          <p>The Tycho-2 Catalogue of the 2.5 Million Brightest Stars (Høg et al., 2000) supplies Seiza’s compact solver catalogue, TYC identifiers, and catalogue-provided Hipparcos cross-identifications.</p>
        </SourceCard>
      </div>
    </section>

    <section className="source-section" aria-labelledby="stellar-identifiers">
      <div className="source-section-heading">
        <p className="eyebrow">STELLAR IDENTIFIERS</p>
        <h2 id="stellar-identifiers">Names and designations for the stars.</h2>
      </div>
      <p className="source-section-intro">The optional offline identifier layer combines Tycho-2 with these maintained catalogues. Together they let Seiza resolve TYC, HIP, HR, HD, SAO, FK5, Bayer/Flamsteed, variable-star, double-star, and IAU proper names without a network lookup.</p>
      <div className="source-list">
        <a href="https://cdsarc.cds.unistra.fr/viz-bin/cat/V/50"><strong>Bright Star Catalogue · V/50</strong><span>Hoffleit &amp; Warren (1991) — HR/HD/SAO/FK5 identifiers, Bayer and Flamsteed designations, and bright-star metadata</span></a>
        <a href="https://cdsarc.cds.unistra.fr/viz-bin/cat/B/gcvs"><strong>General Catalogue of Variable Stars · B/gcvs</strong><span>Samus et al. — variable-star designations, types, magnitudes, and periods</span></a>
        <a href="https://cdsarc.cds.unistra.fr/viz-bin/cat/B/wds"><strong>Washington Double Star Catalog · B/wds</strong><span>Mason et al. and the U.S. Naval Observatory — WDS and discoverer designations, components, separations, and position angles</span></a>
        <a href="https://www.pas.rochester.edu/~emamajek/WGSN/IAU-CSN.txt"><strong>IAU Working Group on Star Names</strong><span>The IAU Catalog of Star Names, maintained by Eric Mamajek for the WGSN, and its standardized proper names</span></a>
      </div>
    </section>

    <section className="source-section" aria-labelledby="object-sources">
      <div className="source-section-heading">
        <p className="eyebrow">OBJECTS &amp; OUTLINES</p>
        <h2 id="object-sources">The nebulae, galaxies, remnants, and stars we label.</h2>
      </div>
      <div className="source-grid two-up source-grid-lead">
        <SourceCard
          title="OpenNGC"
          role="NGC, IC, and Messier objects"
          links={[
            { label: 'OpenNGC project', href: 'https://github.com/mattiaverga/OpenNGC' },
            { label: 'OpenNGC outlines', href: 'https://github.com/mattiaverga/OpenNGC/tree/master/outlines' },
          ]}
        >
          <p>Mattia Verga and OpenNGC contributors provide the principal NGC/IC database, its addendum, aliases, dimensions, and the hand-drawn object contours used for detailed nebula outlines.</p>
        </SourceCard>
        <SourceCard
          title="VizieR and CDS"
          role="Catalogue access and preservation"
          links={[
            { label: 'VizieR', href: 'https://vizier.cds.unistra.fr/' },
            { label: 'CDS', href: 'https://cds.unistra.fr/' },
          ]}
        >
          <p>CDS in Strasbourg preserves and serves the published catalogues below through VizieR. Seiza’s builders retrieve selected columns while retaining the source catalogue identifier with every record.</p>
          <p className="formal-credit">This research has made use of the VizieR catalogue access tool, CDS, Strasbourg, France.</p>
        </SourceCard>
      </div>
      <div className="catalog-table" aria-label="VizieR catalogues used by Seiza">
        {vizierCatalogs.map((catalog) => <a key={catalog.href} href={catalog.href}>
          <strong>{catalog.label}</strong>
          <span>{catalog.contribution}</span>
          <span aria-hidden="true">↗</span>
        </a>)}
      </div>
      <div className="source-curation-note">
        <div>
          <h3>Seiza catalogue curation</h3>
          <p>Explicit identity links, outline associations, preferred geometry, and reviewed corrections live in a public, versioned curation repository. Curation never erases the original source records.</p>
        </div>
        <a href="https://github.com/theatrus/seiza-catalog-curation">View the curation source <span aria-hidden="true">↗</span></a>
      </div>
    </section>

    <section className="source-section" aria-labelledby="changing-sky-sources">
      <div className="source-section-heading">
        <p className="eyebrow">THE CHANGING SKY</p>
        <h2 id="changing-sky-sources">Transient and Solar System data.</h2>
      </div>
      <div className="source-grid three-up">
        <SourceCard
          title="Rochester Astronomy"
          role="Active supernovae and novae"
          links={[{ label: 'Latest Supernovae', href: 'https://www.rochesterastronomy.org/snimages/snactive.html' }]}
        >
          <p>David Bishop’s active transient list supplies recently reported supernovae and novae for Seiza’s nightly refreshed transient overlay.</p>
        </SourceCard>
        <SourceCard
          title="Minor Planet Center"
          role="Comet and asteroid orbital elements"
          links={[
            { label: 'Minor Planet Center', href: 'https://www.minorplanetcenter.net/' },
            { label: 'MPC orbit documentation', href: 'https://docs.minorplanetcenter.net/mpc-ops-docs/orbits/' },
          ]}
        >
          <p>The International Astronomical Union’s Minor Planet Center supplies the comet and numbered-asteroid element sets used to place moving objects at an image’s acquisition time.</p>
        </SourceCard>
        <SourceCard
          title="JPL Small-Body Database"
          role="Historical comet apparitions"
          links={[
            { label: 'Small-body orbits', href: 'https://ssd.jpl.nasa.gov/sb/orbits.html' },
            { label: 'SBDB Query API', href: 'https://ssd-api.jpl.nasa.gov/doc/sbdb_query.html' },
          ]}
        >
          <p>NASA/JPL’s SBDB supplements comet records with historical apparition and orbital data used by Seiza’s minor-body catalogue builder.</p>
        </SourceCard>
      </div>
    </section>

    <section className="source-provenance" aria-labelledby="reproducible-provenance">
      <div>
        <p className="eyebrow">REPRODUCIBLE PROVENANCE</p>
        <h2 id="reproducible-provenance">Every published bundle carries its receipts.</h2>
      </div>
      <div>
        <p>Seiza’s v4 object catalogue records source URLs, raw-file hashes, the curation revision, and build policy. The server’s object-details API exposes source-qualified records and catalogue provenance so users can trace a label back to its origin.</p>
        <div className="source-links">
          <a href="https://downloads.seiza.fyi/data/v4/manifest.json">Current data manifest <span aria-hidden="true">↗</span></a>
          <a href="/docs/api#catalog-api">Catalogue API documentation <span aria-hidden="true">→</span></a>
        </div>
      </div>
    </section>

    <p className="source-thanks">To the observers who gathered the photons, the teams who reduced them, the authors who assembled catalogues, and the institutions that keep them public: thank you. Seiza could not exist without your work.</p>
  </main>
}
