// The Mabat documentation site, built with Starlight.
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { mermaidBlocks } from './scripts/remark-mermaid.mjs';

// Served from GitHub Pages at https://ddalton.github.io/mabat/ unless SITE and BASE say otherwise
const site = process.env.SITE ?? 'https://ddalton.github.io';
const base = process.env.BASE ?? '/mabat';

export default defineConfig({
  site,
  base,
  markdown: { remarkPlugins: [mermaidBlocks] },
  integrations: [
    starlight({
      title: 'Mabat',
      description:
        'Typed aggregate reads and writes for Rust on PostgreSQL, MySQL and SQLite, with SQL you can tune without changing code.',
      logo: { src: './src/assets/logo.svg', replacesTitle: false },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/ddalton/mabat' }],
      editLink: { baseUrl: 'https://github.com/ddalton/mabat/edit/main/docs/site/' },
      lastUpdated: true,
      customCss: [
        '@fontsource-variable/inter',
        '@fontsource-variable/jetbrains-mono',
        './src/styles/theme.css',
      ],
      components: { Head: './src/components/Head.astro' },
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'Introduction', slug: 'index' },
            { label: 'Getting started', slug: 'start/getting-started' },
            { label: 'Why Mabat', slug: 'start/why' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Declaring views', slug: 'guides/views' },
            { label: 'Enums with data', slug: 'guides/enums' },
            { label: 'Collections and recursion', slug: 'guides/collections' },
            { label: 'Shared values and graphs', slug: 'guides/graphs' },
            { label: 'Loading', slug: 'guides/loading' },
            { label: 'Tuning with overrides', slug: 'guides/overrides' },
            { label: 'The mabat CLI', slug: 'guides/cli' },
            { label: 'JSON and selections', slug: 'guides/json' },
            { label: 'GraphQL', slug: 'guides/graphql' },
            { label: 'Saving aggregates', slug: 'guides/writing' },
            { label: 'Databases and concurrency', slug: 'guides/databases' },
          ],
        },
        {
          label: 'Specification',
          items: [
            { label: 'MPA specification', slug: 'spec/mpa' },
            { label: 'Capabilities', slug: 'reference/capabilities' },
            { label: 'Attributes', slug: 'reference/attributes' },
            { label: 'Errors and diagnostics', slug: 'reference/errors' },
          ],
        },
        {
          label: 'Project',
          items: [
            { label: 'Design document', slug: 'design' },
            { label: 'Changelog', slug: 'changelog' },
          ],
        },
      ],
    }),
  ],
});
