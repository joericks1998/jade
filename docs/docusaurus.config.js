// @ts-check
const { themes: prismThemes } = require('prism-react-renderer');

/** @type {import('@docusaurus/types').Config} */
const config = {
  title: 'Jade',
  tagline: 'The AI-native programming language',
  favicon: 'extras/logo.png',
  url: 'https://www.jadelang.org',
  baseUrl: '/',
  organizationName: 'joericks1998',
  projectName: 'jade',
  onBrokenLinks: 'throw',

  // Docusaurus 4 removes the top-level `onBrokenMarkdownLinks`; it lives under
  // `markdown.hooks` now, and setting it in the old place logs a deprecation
  // warning on every build.
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  plugins: [
    // Emits /llms.txt (+ /llms) — all docs as raw markdown for LLM ingestion.
    require.resolve('./plugins/llms-txt.js'),
  ],

  presets: [
    [
      'classic',
      /** @type {import('@docusaurus/preset-classic').Options} */
      ({
        docs: {
          routeBasePath: '/',
          sidebarPath: './sidebars.js',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
        gtag: {
          trackingID: 'G-TQ3H38BVHE',
          anonymizeIP: true,
        },
      }),
    ],
  ],

  themeConfig:
    /** @type {import('@docusaurus/preset-classic').ThemeConfig} */
    ({
      navbar: {
        title: 'Jade',
        logo: {
          alt: 'Jade Logo',
          src: 'extras/logo.png',
        },
        items: [
          {
            type: 'docSidebar',
            sidebarId: 'docs',
            position: 'left',
            label: 'Docs',
          },
          {
            // `pathname://` opts out of SPA routing + broken-link checks so this
            // resolves to the raw static file emitted by the llms-txt plugin.
            href: 'pathname:///llms.txt',
            label: 'llms.txt',
            position: 'right',
          },
          {
            href: 'https://github.com/joericks1998/jade',
            label: 'GitHub',
            position: 'right',
          },
        ],
      },
      colorMode: {
        defaultMode: 'light',
        disableSwitch: false,
      },
      prism: {
        theme: prismThemes.github,
        darkTheme: prismThemes.dracula,
      },
    }),
};

module.exports = config;
