// Local Docusaurus plugin: emit /llms.txt — every docs page as raw markdown,
// concatenated in sidebar order — so other LLMs/tools can ingest the whole
// documentation set in one request. `.txt` is served as text/plain by the host,
// so it's the canonical, correctly-typed URL (linked from the navbar).
//
// Runs in `postBuild` (i.e. on `docusaurus build`), writing into the build
// output root. Not generated during `docusaurus start` (dev) — production build
// is what gets deployed.

const fs = require('fs');
const path = require('path');

// Doc order mirrors sidebars.js. Any *.md not listed here is appended at the end
// (alphabetically) so a new page is never silently dropped from the digest.
const ORDER = [
  'index',
  'quickstart',
  'variables',
  'types',
  'expressions',
  'operators',
  'control-flow',
  'functions',
  'async',
  'structs',
  'imports',
  'packages',
  'exceptions',
  'llm',
  'stdlib',
  'cli',
  'changelog',
];

/** Strip the YAML frontmatter block, returning { title, body }. */
function splitFrontmatter(raw) {
  const m = raw.match(/^---\n([\s\S]*?)\n---\n?/);
  if (!m) return { title: null, body: raw };
  const titleMatch = m[1].match(/^title:\s*(.+)$/m);
  const title = titleMatch ? titleMatch[1].trim().replace(/^['"]|['"]$/g, '') : null;
  return { title, body: raw.slice(m[0].length).replace(/^\n+/, '') };
}

module.exports = function llmsTxtPlugin(context) {
  return {
    name: 'jade-llms-txt',
    async postBuild({ outDir, siteConfig }) {
      const docsDir = path.join(context.siteDir, 'docs');
      const present = fs.readdirSync(docsDir).filter((f) => f.endsWith('.md'));
      const ids = present.map((f) => f.replace(/\.md$/, ''));

      const ordered = [
        ...ORDER.filter((id) => ids.includes(id)),
        ...ids.filter((id) => !ORDER.includes(id)).sort(),
      ];

      const header = [
        `# ${siteConfig.title} — ${siteConfig.tagline}`,
        '',
        '> Full documentation for the Jade programming language, as raw markdown,',
        '> concatenated into one page for LLM ingestion.',
        `> Canonical site: ${siteConfig.url}`,
        '',
        '## Contents',
        ...ordered.map((id, i) => {
          const { title } = splitFrontmatter(fs.readFileSync(path.join(docsDir, `${id}.md`), 'utf8'));
          return `${i + 1}. ${title || id}`;
        }),
      ].join('\n');

      const sections = ordered.map((id) => {
        const raw = fs.readFileSync(path.join(docsDir, `${id}.md`), 'utf8');
        const { title, body } = splitFrontmatter(raw);
        const heading = title ? `# ${title}` : `# ${id}`;
        // Re-key the page heading to H1 and drop the original frontmatter.
        return `${heading}\n\n${body.trim()}`;
      });

      const out = `${header}\n\n---\n\n${sections.join('\n\n---\n\n')}\n`;

      fs.writeFileSync(path.join(outDir, 'llms.txt'), out);
    },
  };
};
