# `docs/`: the jadelang.org site

## What this subtree is

This is the Docusaurus site published at [jadelang.org](https://jadelang.org). It is the *user-facing* documentation: install instructions, the language reference, the standard library, and the changelog. The repository root `README.md` serves the opposite audience, meaning people working on the compiler.

The site also serves two things that are not documentation. `static/install.sh` is what `curl -fsSL https://jadelang.org/install.sh | sh` fetches. And the deploy workflow puts the prebuilt provider `.so` packages into the site root, so that `jade pkg add --url 'https://www.jadelang.org/<name>-{platform}.so'` resolves.

`static/install.sh` is the *only* copy of the installer. An identical one sat at the repository root until v1.1.32, kept in step by hand and served by nothing. An edit reaching one and not the other would have changed what the docs promised without changing what users got. Edit it here.

## What each part does

- *`docs/`* holds the content, one Markdown file per topic: `index`, `quickstart`, `cli`, `variables`, `types`, `expressions`, `operators`, `control-flow`, `functions`, `structs`, `exceptions`, `async`, `imports`, `llm`, `packages`, `stdlib`, and `changelog`.
- *`docusaurus.config.js`* holds the site configuration.
- *`sidebars.js`* sets the navigation order.
- *`src/css/`* holds the theme overrides.
- *`static/`* is served unchanged. It holds `CNAME` for the custom domain, `install.sh` for the install one-liner and the only copy of it, and `extras/`.
- *`plugins/llms-txt.js`* generates an `llms.txt` so models can read the docs.
- *`build/`, `.docusaurus/`, and `node_modules/`* are generated. Never edit them.

## Who uses it

*Deployed by:* `.github/workflows/deploy-docs.yml`, which runs on pushes to `main` that touch `docs/**`. It builds the site with `npm run build` and publishes the *compiled* output.

*Depends on:* nothing in the Rust tree at build time. The content still tracks it, so a language change usually needs a docs change in the same pull request.

## Gotchas

*Only `deploy-docs.yml` may publish to GitHub Pages.* A second deploy job used to live in `ci.yml`, and it uploaded the raw, unbuilt `docs/` tree. On a code-only release, `deploy-docs.yml` is skipped by its path filter, so that job would overwrite Pages with source files and 404 the whole site. It was removed. Do not add a Pages deploy back into `ci.yml`.

The workflow only fires on `docs/**` changes. A docs edit bundled into a commit that touches no path under `docs/` will not deploy until something under `docs/` changes, or until someone triggers the workflow by hand.

Keep the changelog in step with the release process. Shipped work goes under a `## vX.Y.Z` heading, never under "Unreleased".

## Building and running locally

```sh
cd docs
npm ci
npm start        # dev server with hot reload
npm run build    # what CI publishes
npm run serve    # preview the built site
```

Node 18 or newer.
