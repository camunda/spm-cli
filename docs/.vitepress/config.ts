import { defineConfig } from "vitepress";
import { withMermaid } from "vitepress-plugin-mermaid";

const description =
  "A skill package manager: declare AI skills as git dependencies in ai.json and materialize them for Claude Code and GitHub Copilot CLI — without committing skills to your repo.";

// `withMermaid` renders ```mermaid blocks (e.g. the Architecture page).
export default withMermaid(defineConfig({
  title: "spm",
  description,
  lang: "en-US",

  // Project pages are served from https://camunda.github.io/spm-cli/
  base: "/spm-cli/",
  lastUpdated: true,
  cleanUrls: true,

  // Docs are derived from README/schema; dead internal links must fail the build.
  ignoreDeadLinks: false,

  // vitepress-plugin-mermaid bundles mermaid's core (~700 kB) into the app
  // chunk; budget for it instead of warning on every build, while still
  // flagging any further growth.
  vite: {
    build: {
      chunkSizeWarningLimit: 800,
    },
  },

  head: [
    ["link", { rel: "icon", href: "/spm-cli/favicon.svg", type: "image/svg+xml" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:title", content: "spm — skill package manager" }],
    ["meta", { property: "og:description", content: description }],
    ["meta", { name: "twitter:card", content: "summary" }],
    ["meta", { name: "twitter:title", content: "spm — skill package manager" }],
    ["meta", { name: "twitter:description", content: description }],
  ],

  themeConfig: {
    logo: "/logo.svg",
    siteTitle: "spm",

    nav: [
      { text: "Guide", link: "/getting-started/installation" },
      { text: "Reference", link: "/reference/cli-commands" },
      { text: "FAQ", link: "/faq" },
      {
        text: "Links",
        items: [
          { text: "npm (@camunda8/spm)", link: "https://www.npmjs.com/package/@camunda8/spm" },
          { text: "crates.io (spm-cli)", link: "https://crates.io/crates/spm-cli" },
          { text: "Releases", link: "https://github.com/camunda/spm-cli/releases" },
        ],
      },
    ],

    sidebar: [
      {
        text: "Getting Started",
        items: [
          { text: "Installation", link: "/getting-started/installation" },
          { text: "Quick Start", link: "/getting-started/quick-start" },
        ],
      },
      {
        text: "Guide",
        items: [
          { text: "Why spm?", link: "/guide/why-spm" },
          { text: "How It Works", link: "/guide/how-it-works" },
          { text: "Global Skills", link: "/guide/global-skills" },
          { text: "Referencing a Skill", link: "/guide/referencing-skills" },
          { text: "Targets & Vendors", link: "/guide/targets" },
          { text: "Transitive Dependencies", link: "/guide/transitive-dependencies" },
          { text: "Source Policy", link: "/guide/source-policy" },
          { text: "Worktrees & Fresh Clones", link: "/guide/worktrees" },
          { text: "Authoring Skills", link: "/guide/authoring-skills" },
          { text: "CI & Automated Updates", link: "/guide/ci" },
          { text: "Agent Skill", link: "/guide/agent-skill" },
          { text: "Design Notes", link: "/guide/design-notes" },
          { text: "Architecture", link: "/guide/architecture" },
        ],
      },
      {
        text: "Reference",
        items: [
          { text: "CLI Commands", link: "/reference/cli-commands" },
          { text: "ai.json Manifest", link: "/reference/ai-json" },
          { text: "ai.lock Lockfile", link: "/reference/ai-lock" },
          { text: "Schema & Validation", link: "/reference/schema" },
        ],
      },
      { text: "FAQ", link: "/faq" },
    ],

    socialLinks: [
      { icon: "github", link: "https://github.com/camunda/spm-cli" },
    ],

    editLink: {
      pattern: "https://github.com/camunda/spm-cli/edit/main/docs/:path",
      text: "Edit this page on GitHub",
    },

    search: {
      provider: "local",
    },

    footer: {
      message: "Released under the Apache License 2.0.",
      copyright: "Copyright © Camunda",
    },
  },
}));
