# Agent Skill

spm ships a skill that teaches an AI coding agent how to operate `spm` in your
project. Once it is installed you can say "add this skill to the project" and the
agent runs the right commands, without you pasting docs into the chat.

The skill lives in the spm repo at
[`skills/spm/SKILL.md`](https://github.com/camunda/spm-cli/blob/main/skills/spm/SKILL.md).
It covers the mental model (`ai.json` is authored, `ai.lock` is generated and
committed, materialized skills are gitignored), every command and its flags, which
command to use in which situation, and how to recover from common errors (ref not
found, auth failures over SSH or HTTPS, a blocked content scan, a taken skill name).

## Install it with spm

The skill installs like any other skill:

```bash
spm add https://github.com/camunda/spm-cli --branch main --path skills/spm
```

Use `--tag <version>` instead of `--branch main` to pin a release that contains
the skill. To make it available in every project, use the global scope. `spm add -g`
needs the global manifest to exist, so run `spm init -g` first (it is safe to repeat):

```bash
spm init -g
spm add -g https://github.com/camunda/spm-cli --branch main --path skills/spm
```

The dependency is named `spm` by default (the last segment of `--path`). Commit
the resulting `ai.json` and `ai.lock`; the materialized skill stays gitignored
like any other. Start a new agent session afterwards so the tool discovers it.

## Contributors

`AGENTS.md` and `CLAUDE.md` in the repo are for people working on `spm-cli`
itself. The skill is the user-facing counterpart and must be updated in the same
change whenever a command, flag or behavior it documents changes.
