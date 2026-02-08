import { defineConfig } from 'vitepress';

export default defineConfig({
  title: 'mica',
  description: 'A TUI for managing Nix environments',
  cleanUrls: true,
  themeConfig: {
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Getting Started', link: '/getting-started' },
      { text: 'CLI', link: '/cli' },
      { text: 'TUI', link: '/tui' }
    ],
    sidebar: [
      {
        text: 'Introduction',
        items: [
          { text: 'Getting Started', link: '/getting-started' },
          { text: 'Configuration', link: '/configuration' }
        ]
      },
      {
        text: 'Workflow',
        items: [
          { text: 'TUI Guide', link: '/tui' },
          { text: 'CLI Reference', link: '/cli' },
          { text: 'Presets', link: '/presets' }
        ]
      },
      {
        text: 'Advanced',
        items: [
          { text: 'Pinning and Index', link: '/pinning-and-index' },
          { text: 'Troubleshooting', link: '/troubleshooting' }
        ]
      }
    ],
    socialLinks: [
      { icon: 'github', link: 'https://github.com/gemologic/mica' }
    ],
    search: {
      provider: 'local'
    },
    footer: {
      message: 'MIT Licensed',
      copyright: 'Copyright 2026 gemologic'
    }
  }
});
