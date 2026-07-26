# `docs/` — the jadelang.org site

## What this subtree is

The Docusaurus site published at [jadelang.org](https://jadelang.org). It is the *user-facing* documentation: install instructions, language reference, standard library, changelog. The repository root `README.md` is the opposite audience — it is for people working on the compiler.

The site also serves two things that are not documentation. `static/install.sh` is what `curl -fsSL https://jadelang.org/install.sh | sh` fetches, and the deploy workflow injects the prebuilt provider `.so` packages into the site root so `jade pkg add --url 'https://www.jadelang.org/<name>-{platform}.so'` resolves.

## What each part does

- **`docs/`** — the content, one Markdown file per topic: `index`, `quickstart`, `cli`, `variables`, `types`, `expressions`, `operators`, `control-flow`, `functions`, `structs`, `exceptions`, `async`, `imports`, `llm`, `packages`, `stdlib`, `changelog`.
- **`docusaurus.config.js`** — site configuration.
- **`sidebars.js`** — navigation order.
- **`src/css/`** — theme overrides.
- **`static/`** — served verbatim. `CNAME` (the custom domain), `install.sh` (the install one-liner), `extras/`.
- **`plugins/llms-txt.js`** — generates an `llms.txt` so models can read the docs.
- **`build/`, `.docusaurus/`, `node_modules/`** — generated. Never edit these.

## Who uses it

*Deployed by:* `.github/workflows/deploy-docs.yml`, which runs on pushes to `main` that touch `docs/**`. It builds the site with `npm run build` and publishes the *compiled* output.

*Depends on:* nothing in the Rust tree at build time, but the content tracks it — a language change usually needs a docs change in the same pull request.

## Gotchas

**Only `deploy-docs.yml` may publish to GitHub Pages.** A second deploy job used to live in `ci.yml` and uploaded the raw, unbuilt `docs/` tree. On code-only releases `deploy-docs.yml` is skipped by its path filter, so that job would clobber Pages with source files and 404 the whole site. It was removed. Do not re-add a Pages deploy to `ci.yml`.

The workflow only fires on `docs/**` changes. A docs edit bundled into a code-only commit path will not deploy until something under `docs/` changes, or until someone triggers the workflow by hand.

Keep the changelog in step with the release process: shipped work goes under a `## vX.Y.Z` heading, not under "Unreleased."

## Building and running locally

```sh
cd docs
npm ci
npm start        # dev server with hot reload
npm run build    # what CI publishes
npm run serve    # preview the built site
```

Node 18 or newer.
