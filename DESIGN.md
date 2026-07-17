# DESIGN — memory map

## Color (restrained, dark)

- bg `#060407`, panel `#12080a` (solid — no glass), border `#3a1418`
- ink `#f5e9e9`, secondary `#b3a0a0` (≥4.5:1 on panel), faint `#8a7676` (labels ≥3:1 only)
- accent `#ef4444` — selection, primary state, the core. Never decoration.
- data colors are semantic and fixed: user `#fb923c`, assistant `#60a5fa`, summary `#94a3b8`, memory `#34d399`, wiki `#c084fc`, code `#22d3ee`, session `#7f1d1d`, project `#f59e0b`

## Type

One family: ui-monospace stack everywhere. Fixed rem scale, ratio ~1.2: 11px labels, 13px body, 16px panel titles, 19px wordmark. No display fonts.

## Motion

- UI transitions 150–220 ms, ease-out-quart. Camera flights 900 ms, same ease.
- Motion conveys state only: descend/ascend, select, found. Idle synaptic pulses are the one delight.
- `prefers-reduced-motion`: no idle drift, no pulses, instant transitions.

## Depth / z scale

hud 10 · labels 6 · detail 20 · boot 100. No arbitrary values.

## Interaction vocabulary

- Hover: highlight + one-line readout. Nothing pops.
- Click: descend / select. Detail lives in a fixed right panel; the selected node gets a ring, not a floating card.
- Esc: up one level. Breadcrumb: always shows where you are, every crumb clickable.
- Every control: default / hover / focus-visible / active states.
