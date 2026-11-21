# Icon packs

An icon pack replaces leaf's line icons for files and folders with your own pictures. A pack is a folder in the config folder:

```
<config>\icons\<pack>\pack.toml
<config>\icons\<pack>\folder.svg
<config>\icons\<pack>\rust.svg
...
```

Choose it in Settings > Appearance > Icons ("Open icons folder" opens `<config>\icons`). A pack applies to the details list, the grid,
tiles, cards and the columns view. Pictures are loaded the first time they are needed and kept in the picture cache.

## pack.toml

```toml
[pack]
name = "My icons"        # shown in Settings (default: the folder's name)
author = "me"
licence = "CC0-1.0"
tint = false             # true: the pictures are one-colour shapes, painted in the theme's icon colour

[folder]
default = "folder.svg"   # every folder

[file]
default = "file.svg"     # every file that nothing below names

[ext]                    # by extension, without the dot, any case
rs = "rust.svg"
ts = "js.svg"

[name]                   # by exact file name, any case; wins over [ext]
"Cargo.toml" = "cargo.svg"

[folder_name]            # by folder name, any case; wins over [folder]
src = "folder-src.svg"
node_modules = "folder-npm.svg"
```

Every entry is optional, but a pack must name at least one picture. File names are relative to the pack's folder (no `..`, no drive
letters). Pictures are SVG or PNG (other bitmap formats the program can read work too); an SVG is drawn at the size needed, a PNG is
scaled to it, so give PNGs at 64 px or more.

## Order

A file: its name in `[name]`, then its extension in `[ext]`, then `[file] default`. A folder: its name in `[folder_name]`, then
`[folder] default`. Anything the pack has no picture for keeps leaf's built-in line icon, as does a picture that cannot be read.

## Tinting

With `tint = true` the pictures' colours are ignored: every pixel is painted in the theme's colour for that kind of item (folder,
picture, document...), keeping the picture's transparency. Use it for black or single-colour glyphs so they follow the theme. With
`tint = false` (the default) the pictures are drawn as they are.

## A broken pack

A pack whose `pack.toml` cannot be read, or that names no picture, is written to the log and left out of the list. A picture that
cannot be drawn is logged and the line icon is used for that item. If you edit a pack while leaf is running, choose the pack again
in Settings (or restart leaf) to load it anew.

## Built in

leaf ships one small example, "Canopy leaves" (`leaf-canopy`, twelve plain pictures, CC0): copy its idea, not its files. To change it,
make the folder `<config>\icons\leaf-canopy\` with your own `pack.toml`; a folder in the config folder replaces the built-in pack of the same name.
