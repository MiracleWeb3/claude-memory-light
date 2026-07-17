# claude-memory-light — the memory map

register: product
platform: web (one static, offline HTML file emitted by the `cml` Rust binary)

## Who / where / light

One developer, alone, evenings, dark room, reviewing months of their own Claude Code history. The scene forces dark theme; the crimson "Ultron-class" identity is a committed user choice, not a default.

## The task

Find and read past thoughts fast; see the shape of the work (projects → sessions → thoughts); trust the tool enough to live in it. The map must feel like an instrument, not a demo.

## Hard constraints

- Single self-contained file. No CDN, no server, no build step at view time.
- Must hold a smooth frame rate on a weak integrated GPU (4-core laptop, 3.6 GB RAM).
- All data is local and private.
- Interaction model must be legible without a manual: hover to identify, click to descend, Esc to come back up, one breadcrumb telling you where you are.

## Signature moments (delight budget)

The boot sequence and the idle synaptic pulses. Everything else serves the task.
