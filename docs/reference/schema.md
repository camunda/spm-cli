# Schema & Validation

`ai.json` is described by a JSON Schema (draft-07) at
[`schema/ai.schema.json`](https://github.com/camunda/spm-cli/blob/main/schema/ai.schema.json).
It is the **single source of truth** for the shape of `ai.json`.

## Validation on load

spm **embeds** the schema and validates every `ai.json` on load, reporting all
violations at once with their JSON path:

```
error: in ai.json: ai.json does not match schema:
  at /skills/x: {"git":"u"} is not valid under any of the schemas listed in the 'oneOf' keyword
```

Because validation is embedded, you get the same result offline and in CI — there
is no separate validation step to wire up.

## Editor integration

Add a `$schema` reference to your `ai.json` for autocompletion and inline
validation in editors that support JSON Schema:

```json
{ "$schema": "./schema/ai.schema.json", "targets": ["claude"], "skills": {} }
```

## What the schema enforces

- `targets` is a list of supported vendors (`claude`, `codex`, `copilot`, `gemini`).
- Each entry under `skills` requires a `git` URL and **exactly one** version
  selector (`tag`, `branch`, or `commit`) — enforced via a `oneOf` constraint.
- `path` is an optional subdirectory selector.

See the [ai.json reference](/reference/ai-json) for the field-by-field breakdown.
