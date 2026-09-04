# Snap — agent guidance

## Sources of truth

[`SPEC.md`](SPEC.md) is the canonical product contract. Public behavior must be
demonstrated in the language-neutral YAML suite under [`tests/`](tests/).
You may add language-specific unit tests while developing, but they cannot
replace the shared acceptance suite.

When implementation work reveals an ambiguity or contradiction, correct the
spec first or in the same commit and add a regression case to the public YAML
suite. Do not silently make the implementation authoritative.

## Implementation layout

Work in the language directory present at the project root. Keep responsibilities
separate: versions, text/diff and OT, repository validation and replay,
filesystem materialization, working-tree changes, HTTP, commands, and CLI
dispatch.

The YAML harness is implementation-language neutral. Never import reference
code into it or add shell setup operations to test around a missing typed
operation. Extend its tagged unions additively so existing format-1 cases keep
their meaning.

## Verification

After implementation changes, run the shared acceptance suite:

```bash
./capstones/snap/verify --lang ts
```

Replace `ts` with `rust` or `scala` when appropriate.


After harness changes, also run:

```bash
cd capstones/snap/test-harness
npm run check
npm test
```

## Scope discipline

Snap’s small surface is deliberate. Do not add branches, staging, checkout,
push, authentication, object storage, or unresolved-conflict machinery. Spend
complexity on deterministic behavior, strict validation, and exact tests—not
on production scalability or command count.
