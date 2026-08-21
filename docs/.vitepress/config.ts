import { defineConfig } from "vitepress";

const description =
  "A skill package manager: declare AI skills as git dependencies in ai.json and materialize them for Claude Code and GitHub Copilot CLI — without committing skills to your repo.";

export default defineConfig({
  title: "spm",
  description,
  lang: "en-US",

  // Project pages are served from https://camunda.github.io/spm-cli/
  base: "/spm-cli/",
  lastUpdated: true,
  cleanUrls: true,

  // Docs are derived from README/schema; dead internal links must fail the build.
  ignoreDeadLinks: false,

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
          { text: "Targets & Vendors", link: "/guide/targets" },
          { text: "Worktrees & Fresh Clones", link: "/guide/worktrees" },
          { text: "Design Notes", link: "/guide/design-notes" },
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
});
