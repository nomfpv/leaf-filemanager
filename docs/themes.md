# Themes

leaf ships three themes: **Canopy** (frosted glass over the window backdrop, the default), **Stem** (calm, solid, a thin accent
line marks the current place) and **Index** (dense, ruled rows, monospace numbers). Each has a dark and a light variant and two
visibility levels, Standard and High (the default). Choose them in Settings > Appearance; changes apply at once.

## Your own theme

Put a `.toml` file in the `themes` folder of leaf's settings folder (Settings > Appearance > "Open themes folder"; it is
`%APPDATA%\leaf\themes` on Windows, `~/.config/leaf/themes` on Linux, or `themes` under `LEAF_CONFIG_DIR` when that is set). The
file name without `.toml` is the theme's name in the settings file; it appears in the Theme list under its `name`. A file named
like a built-in theme is ignored.

```toml
name = "Moss"
base = "stem"            # the theme it starts from; without it, Canopy

[dark]
content = "#151b16"
text = "#eef3ec"

[dark_high]
text = "#ffffff"

[[accents]]
name = "Moss"
dark = "#9ccc65"
dark_text = "#10200a"
light = "#3f6e1c"
light_text = "#ffffff"
```

## The fallback rule

A theme is laid over its `base`, and every theme over Canopy. Within one file the values apply in this order: the top of the file
(every variant), then `[dark]` or `[light]`, then `[dark_high]` or `[light_high]` when visibility is High. A key a file leaves out,
or writes wrong (unknown key, wrong kind of value, a bad colour), keeps the base theme's value; the problem is written to leaf's log.
A file that is not valid TOML is logged and skipped. A theme can never blank the window or stop leaf.

Colours are `"#rrggbb"`, `"#rrggbbaa"`, or `"accent"` / `"accent_text"` (the accent in use and the text on it), each optionally
followed by `/alpha` (`"accent/0.3"`, `"#ffffff/0.1"`). Numbers are pixels unless said otherwise.

## File keys

| Key | Meaning |
|---|---|
| `name` | The name shown in Settings. |
| `base` | The theme this one starts from (`canopy`, `stem`, `index` or another file's name). |
| `description` | A note for people reading the file. |
| `accents` | The accent set: `[[accents]]` tables with `name`, `dark`, `dark_text`, `light`, `light_text` (`#rrggbb`). The first one is the default. |

## Tokens

| Key | Meaning |
|---|---|
| `window` | The window frame behind the title bar, the address row and the sidebar; over a backdrop it is the veil. |
| `titlebar` | The tab strip, over the window colour. |
| `toolbar` | The address row (back, forward, the address box). |
| `sidebar` | The sidebar. |
| `content` | The file panel: command bar, list, preview and status line. |
| `raised` | Menus, dialogs, pop-overs and cards that float over other content (keep it opaque). |
| `field` | The address box and other text fields. |
| `field_border` | The outline of a text field. |
| `tab_active` | The tab in front. |
| `tile` | Cards on the content panel (Home tiles, preview boxes). |
| `tile_hover` | A card under the pointer. |
| `status` | The status line; fully transparent means the panel's own colour. |
| `status_text` | Strong text in the status line (the selection count). |
| `status_text_secondary` | The other text in the status line. |
| `stripe` | Every second row of the details list (zebra); fully transparent means no stripes. |
| `border` | Lines around panels and fields. |
| `divider` | Separators inside a panel. |
| `border_strong` | Stronger lines: column header, focused fields. |
| `text` | Body text and file names. |
| `text_secondary` | Sizes, dates and other secondary text. |
| `text_tertiary` | Types, section labels, hints. |
| `text_disabled` | Disabled commands. |
| `hover` | A row or button under the pointer. |
| `pressed` | A pressed button, a stronger hover. |
| `selection` | The fill of a selected item in lists, the sidebar, menus and Settings. |
| `selection_border` | The outline of a selected row; fully transparent means none. |
| `row_selection` | The fill of a selected row or tile in the file list. |
| `row_selection_text` | Text on a selected row or tile in the file list. |
| `cursor` | The outline of the focused item inside a multiple selection. |
| `focus` | The marker of the focused pane and focus rings. |
| `danger` | Delete, errors. |
| `warning` | Warnings. |
| `success` | Done, healthy. |
| `kind_folder` | Folder icons. |
| `kind_image` | Picture icons. |
| `kind_video` | Video and audio icons. |
| `kind_document` | Document and PDF icons. |
| `kind_code` | Source code icons. |
| `kind_text` | Plain text and other files. |
| `kind_archive` | Archive icons. |
| `kind_app` | Programs and spreadsheets. |
| `backdrop` | true/false: the theme lets the window backdrop (Mica, Acrylic, glass) show through its surfaces. |
| `sheet_opacity` | With a backdrop: how opaque the panel surfaces are, 0 to 1. |
| `solid_window` | The window colour when the backdrop is off or not available. |
| `solid_sheet` | The panel colour when the backdrop is off or not available. |
| `solid_raised` | The menu and dialog colour when the backdrop is off or not available. |
| `radius` | Corner radius of panels, in pixels. |
| `control_radius` | Corner radius of tabs, buttons and fields. |
| `row_radius` | Corner radius of list rows. |
| `row_height` | Height of a details row at the default size, in pixels. |
| `marker_width` | Width of the accent line on the left of a selected row; 0 means none. |
| `focus_width` | Thickness of the focused-pane marker. |
| `tab_marker` | Thickness of the accent line on top of the tab in front; 0 means none. |
| `tag_dot` | Diameter of tag dots. |
| `name_weight` | Font weight of file names (400 regular, 500 medium, 600 semibold). |
| `font_ui` | The interface font; empty means the system font. A font that is not installed is ignored. |
| `font_mono` | The font of number columns when mono_numbers is on. |
| `mono_numbers` | true/false: sizes and dates in the details list use font_mono. |
