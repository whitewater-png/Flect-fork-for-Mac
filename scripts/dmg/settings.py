# dmgbuild settings for the Flect-for-Mac disk image (values come from scripts/make-dmg.sh via -D name=value).
# The Finder layout is written straight into .DS_Store by dmgbuild, so Finder is never scripted
# (macOS 26's Finder does not keep a background set through AppleScript).
import os.path

defines = globals()["defines"]  # provided by dmgbuild
app = defines["app"]
readme = defines["readme"]
background = defines["background"]
icon = defines["icon"]

format = "UDZO"
compression_level = 9
filesystem = "HFS+"
size = None

files = [app, readme]
symlinks = {"Applications": "/Applications"}

# Positions are icon centres in window points. Keep them in step with scripts/dmg/make-background.swift (W, H, arrow).
icon_locations = {
    os.path.basename(app): (190, 185),
    "Applications": (530, 185),
    os.path.basename(readme): (360, 490),
}

# The window is taller than the 540-point background: macOS 26's Finder keeps some toolbar space, so the
# visible content is shorter than the window. The surplus is white, like the background's bottom edge.
window_rect = ((200, 120), (720, 700))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
arrange_by = None
icon_size = 80
text_size = 12
show_icon_preview = False
include_icon_view_settings = True
include_list_view_settings = False
