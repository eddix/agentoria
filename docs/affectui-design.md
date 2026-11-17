# AffectUI in Agentoria

Agentoria adapts the supplied `affectui-components` **default / Classic** family to GPUI. The reference library is React; native components implement its visual and interaction rules.

| Reference | Native use |
| --- | --- |
| `button`, `text-field` | 32 px compact controls, 10 px radius, layered shadows, quiet hover, clear focus and disabled states |
| `segmented-control` | Raised-pill app navigation and neutral Recent / Grouped trays |
| `agent-inbox` | Flat, compact session / skill / configuration rows with muted metadata and a clear selected row |
| `ai-answer`, `message-actions` | Unframed conversation, role badges, readable text and message copy feedback |
| `tool-call-card`, `code-block` | Collapsible process records and bounded code / JSON detail |
| `empty-state` | Bare empty states |

Classic light uses white surfaces, a `#F0F0F0` tray, `#171717` ink, `#737373` metadata and `#E7E7E7` edges. Primary buttons have the reference's charcoal `#323137` → `#201E25` bevel; secondary controls use a 1 px edge and a light shadow. Classic dark uses `#161618` surfaces, `#1D1D20` trays, `#EDEDED` ink and a light `#F4F4F5` → `#E4E4E7` primary bevel. Controls use 10 px radii, large overlays 16 px and status badges a pill shape. Blue remains focus/link/USER identity. The theme menu names this pair **AffectUI Classic Light / Dark**; existing palettes remain available.

Shared metrics live in `ui.rs`; semantic colors live in the bundled AffectUI theme pair. Typography uses the native system sans-serif and the theme's monospace family. Content remains full-window with separators, not a grid of cards. Rounding belongs to controls, badges and code wells.

Navigation supports ⌘1–⌘4 without inline keycaps; ⌘L focuses the current list filter. Sessions retain fuzzy work-directory filtering, grouped browsing and native source selection. Message copy and process disclosure are local, read-only operations. History is never animated as if it were streaming. Virtualized lists, cached bodies, exact scroll anchors and in-place refresh remain part of the UI contract.
