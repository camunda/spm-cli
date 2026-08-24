---
layout: home

hero:
  name: spm
  text: Skill Package Manager
  tagline: Declare AI skills as git dependencies in ai.json and materialize them for Claude Code, OpenAI Codex CLI, GitHub Copilot CLI, Cursor, and Gemini CLI — without ever committing skills to your repo.
  image:
    src: /logo.svg
    alt: spm logo
  actions:
    - theme: brand
      text: Get Started
      link: /getting-started/installation
    - theme: alt
      text: Why spm?
      link: /guide/why-spm
    - theme: alt
      text: View on GitHub
      link: https://github.com/camunda/spm-cli

features:
  - icon: 📦
    title: Skills as Git Dependencies
    details: Declare AI skills in ai.json the same way you declare code dependencies. Pin them by tag, branch, or commit and lock them to an immutable SHA in ai.lock.
  - icon: 🔒
    title: Nothing Committed to Your Repo
    details: Everything spm materializes into the working tree is gitignored — no symlinks, no skills under version control. Same model as node_modules.
  - icon: 🛠️
    title: Multi-Vendor Projection
    details: One declaration resolves once and projects independently into Claude Code, OpenAI Codex CLI, GitHub Copilot CLI, Cursor, and Gemini CLI, each in the project-local location that vendor expects.
  - icon: ♻️
    title: Reproducible Installs
    details: A committed ai.lock pins every version selector to a commit SHA. Teammates run spm install on a fresh clone and get exactly the same skills.
  - icon: 🌍
    title: Cross-Platform, Single Binary
    details: Ships as one self-contained binary that shells out to the system git. Runs on Linux, macOS, and Windows — install via npm, crates.io, or a prebuilt release.
  - icon: ⚡
    title: Shared Global Fetch Cache
    details: Each repo@commit is cloned once into ~/.spm/store and shared across all your projects, so repeated installs never re-clone.
---
