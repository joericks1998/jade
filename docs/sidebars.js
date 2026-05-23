// @ts-check

/** @type {import('@docusaurus/plugin-content-docs').SidebarsConfig} */
const sidebars = {
  docs: [
    { type: 'doc', id: 'index', label: 'Installation' },
    { type: 'doc', id: 'quickstart', label: 'Quick Start' },
    {
      type: 'category',
      label: 'Language Reference',
      collapsed: false,
      items: [
        'variables',
        'types',
        'expressions',
        'operators',
        'control-flow',
        'functions',
        'async',
        'structs',
        'imports',
        'exceptions',
      ],
    },
    {
      type: 'category',
      label: 'Guides',
      collapsed: false,
      items: ['llm', 'stdlib', 'cli'],
    },
    { type: 'doc', id: 'changelog', label: 'Changelog' },
  ],
};

module.exports = sidebars;
