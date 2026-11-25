export function ModelReferencesPage() {
  return <main className="sources-page models-page">
    <header className="sources-hero">
      <h1>AI denoise and sharpen</h1>
      <p className="intro">Seiza’s experimental image restoration models run on your computer. Start with a calibrated, stacked linear FITS or XISF image, in monochrome or RGB.</p>
    </header>

    <div className="source-grid two-up model-grid">
      <section className="source-card" aria-labelledby="ai-denoise">
        <p className="source-role">Published model · noise-v1-d33</p>
        <h2 id="ai-denoise">AI denoise</h2>
        <div className="source-copy">
          <p>Reduce fine-grained noise and broader background mottling independently with <code>--high-amount</code> and <code>--low-amount</code>.</p>
          <p>When no denoise model is installed, the CLI downloads and verifies the published bundle on first use. Later runs use the local copy.</p>
          <pre><code>seiza model denoise stack.fits -o clean.fits</code></pre>
        </div>
        <div className="source-links">
          <a href="https://downloads.seiza.fyi/models/v1/manifest.json">Published model manifest <span aria-hidden="true">↗</span></a>
          <a href="https://github.com/theatrus/seiza/blob/v0.56.0/seiza-cli/README.md#learned-models-experimental">Denoise CLI guide <span aria-hidden="true">↗</span></a>
        </div>
      </section>

      <section className="source-card" aria-labelledby="ai-sharpen">
        <p className="source-role">In development</p>
        <h2 id="ai-sharpen">AI sharpen</h2>
        <div className="source-copy">
          <p>Reduce blur with separate controls for stars and other structures. The model runtime measures how the shape of stars varies across the image.</p>
          <p>Trying the experimental <code>seiza model sharpen</code> command requires a build from the development branch and a local sharpening model bundle.</p>
        </div>
        <div className="source-links">
          <a href="https://github.com/theatrus/seiza/blob/sharpen-runtime/seiza-cli/README.md#learned-models-experimental">Sharpen development guide <span aria-hidden="true">↗</span></a>
        </div>
      </section>
    </div>
  </main>
}
