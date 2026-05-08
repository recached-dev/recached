import { defineConfig } from 'vitepress'

export default defineConfig({
  base: '/recached/',
  title: 'Recached',
  titleTemplate: ':title — Recached',
  description:
    'A Rust cache server that runs on your backend and inside the browser. Zero-latency local reads. Automatic WebSocket sync. Works as a Redis drop-in on the server and a WASM module in the browser.',

  head: [
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:image', content: 'https://recached.dev/recached.jpg' }],
    [
      'meta',
      {
        property: 'og:description',
        content:
          'A Rust cache server that runs on your backend and inside the browser. Zero-latency local reads. Automatic WebSocket sync.',
      },
    ],
    ['meta', { name: 'twitter:card', content: 'summary_large_image' }],
    ['meta', { name: 'twitter:image', content: 'https://recached.dev/recached.jpg' }],
    ['meta', { name: 'keywords', content: 'rust cache, redis alternative, redis compatible, webassembly cache, wasm browser cache, local-first, zero-latency reads, websocket sync, in-memory cache, browser cache, edge cache, real-time sync, offline-first, indexeddb cache, pub-sub' }],
  ],

  themeConfig: {
    siteTitle: 'Recached ⚡',

    nav: [
      { text: 'Home', link: '/' },
      { text: 'Guide', link: '/guide/introduction' },
      { text: 'Server', link: '/server/installation' },
      { text: 'Browser', link: '/browser/getting-started' },
      { text: 'Roadmap', link: '/roadmap' },
    ],

    sidebar: {
      '/guide/': [
        {
          text: 'Guide',
          items: [
            { text: 'Introduction', link: '/guide/introduction' },
            { text: 'Quick Start', link: '/guide/quick-start' },
            { text: 'How It Works', link: '/guide/how-it-works' },
          ],
        },
      ],
      '/server/': [
        {
          text: 'Server',
          items: [
            { text: 'Installation', link: '/server/installation' },
            { text: 'Configuration', link: '/server/configuration' },
            { text: 'Commands', link: '/server/commands' },
          ],
        },
      ],
      '/browser/': [
        {
          text: 'Browser (WASM)',
          items: [
            { text: 'Getting Started', link: '/browser/getting-started' },
            { text: 'API Reference', link: '/browser/api-reference' },
            { text: 'Persistence', link: '/browser/persistence' },
          ],
        },
      ],
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/thinkgrid-labs/recached' },
    ],

    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Copyright © 2026 ThinkGrid Labs',
    },

    editLink: {
      pattern: 'https://github.com/thinkgrid-labs/recached/edit/main/docs/:path',
      text: 'Edit this page on GitHub',
    },

    lastUpdated: true,
  },

  sitemap: {
    hostname: 'https://recached.dev',
  },
})
