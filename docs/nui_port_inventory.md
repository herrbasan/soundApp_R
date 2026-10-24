# nui_wc2 -> Rust nui port inventory (2026-10-23, minimax audit)

PORT-HIGH: app shell (header/sidebar/content/footer, M), textarea (M), progress (M), table (M), tooltip (M), popover (M), banner (S), notification/toast service (M), dropzone file-drop (S), app-window mgmt (M), file-list (M), file-tree (M)
PORT-LOW: loading spinner (S), accordion (M), details (S), tag-input (M), date-range (M), sortable (M), file-icon (S), code (S), media-player (M), graph (L), notepad (L), code-editor (L), wizard (M), syntax-highlight (M)
SKIP: skip-links, markdown, lightbox, rich-text, slides, blocks-editor, table-editor, json-grid family, monitor
Already ported: button, icon-button, icon, badge, card, layout, link-list, segmented, input-group/input, checkbox, radio, switch, select, slider, tabs, dialog, overlay, virtual-scroll, context-menu, menu, list
Note: reuse Scene::begin_overlay / select popup machinery for tooltip/popover.
