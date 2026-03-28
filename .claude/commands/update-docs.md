Read the following files in this exact order to understand the current state of the Jade project:

1. `Cargo.toml` — read the `version` field under `[package]`; this is the authoritative version string
2. `src/main.rs` — CLI entry point and argument parsing
3. `src/cli/mod.rs`
4. `src/cli/help.rs` — the help text printed to users
5. `src/cli/run.rs` — how files are executed
6. `src/interpreter/mod.rs`
7. `src/interpreter/lexer.rs` — tokenizer and auto-semicolon logic
8. `src/interpreter/parser.rs` — recursive descent parser
9. `src/interpreter/eval.rs` — evaluator and environment
10. `src/interpreter/ast.rs` — AST node definitions
11. `src/interpreter/error.rs` — error types
12. `jade_evals/math_variable_assignment.jde` — a working example of Jade syntax
13. `planning/REQUIREMENTS.md` — the full build plan, to understand what phase we are in
14. `docs/index.html` — the current documentation site that will be updated

After reading all files, do the following:

**Determine what is actually working today.** Base your assessment strictly on what the source code implements — not on comments, planning docs, or aspirational text. Establish these facts before touching the HTML:

- The exact version string from `Cargo.toml` (e.g., `1.0.1`)
- What CLI commands and flags are functional
- What language features are implemented (statements, expressions, operators, types)
- What runtime errors are handled
- What a minimal working Jade program looks like (derive this from `eval.rs` and the test file, not from docs)
- What build phase is currently active (from `REQUIREMENTS.md`)

**Update `docs/index.html`** with the following rules. Be surgical — only change content that can become stale as the codebase evolves. Never restructure the page.

PRESERVE exactly:
- All CSS inside the `<style>` block — do not add, remove, or change any rules
- The overall three-zone layout: `.topbar`, `.sidebar`, `.content`
- The logo `<img>` tag (`src="extras/logo.png"`) in the topbar
- The sidebar nav structure and all existing nav links (`<a href="#...">` entries)
- All section `id` attributes (`#installation`, `#quickstart`, `#variables`, `#expressions`, `#operators`, `#types`, `#cli`, `#changelog`) and their corresponding `<section>` elements
- The GitHub `<a>` link in the topbar

UPDATE only the following content elements, using the version string and capability inventory you determined above:

1. **Version string** — the version appears in multiple places; update all of them consistently:
   - The `<option>` text inside the version `<select>` in the topbar
   - The title `<span>` in the topbar that reads "X.Y.Z Documentation"
   - The `<h1>` in the main content area that reads "Jade X.Y.Z Documentation"
   - The `.lead` paragraph — rewrite it to be a short, honest description of what the interpreter actually supports right now (1–3 sentences, no hype about unbuilt features)
   - The changelog `<h3>` version heading — match whatever heading pattern is already used in that section

2. **Changelog section** (`#changelog`) — update the `<ul>` beneath the current version `<h3>` to list only what is actually implemented, derived strictly from the source code. Do not list features that exist only in planning docs. Remove any changelog entries that describe unbuilt features.

3. **Language feature sections** — if any section contains a status table, feature list, or operator table, update only the cells or items that are factually wrong given what `eval.rs` and the parser actually implement. Do not rewrite prose that is still accurate. If a feature is listed as "planned" and is now implemented, update its status. If a feature is listed as "implemented" but is not present in the source, mark it "planned".

4. **CLI section** (`#cli`) — verify that the documented commands and flags match what `src/cli/` actually exposes. Update only lines that are wrong. Do not reformat working content.

Do not add new sections, headings, cards, nav links, CSS classes, or `<style>` rules. Do not reorder existing sections. Do not change any element's `id` or `class` attribute unless the value is factually wrong. If a section's content is already accurate, leave it byte-for-byte identical.

After writing the updated file to `docs/index.html`:

1. Stage and commit `docs/index.html` with a message summarizing what content changed and why (same evidence-based style: cite the source file that justified each change).
2. Push the commit to the current branch.
3. Print a brief summary (3–5 bullet points) of exactly what content changed and what specific source-code evidence justified each change. Do not list unchanged elements.
