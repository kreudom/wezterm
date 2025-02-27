# `gui-startup`

*Since: 20220624-141144-bd1b7c5d*

The `gui-startup` event is emitted once when the GUI server is starting up
when running the `wezterm start` subcommand.

It is triggered before any default program is started.

If no explicit program was passed to `wezterm start`, and if the
`gui-startup` event causes any panes to be created then those will take
precedence over the default program configuration and no additional default
program will be spawned.

This event is useful for starting a set of programs in a standard
configuration to save you the effort of doing it manually each time.

This basic example splits an initial window into thirds:

```lua
local wezterm = require 'wezterm'
local mux = wezterm.mux

wezterm.on("gui-startup", function()
  local tab, pane, window = mux.spawn_window{}
  -- Create a split occupying the right 1/3 of the screen
  pane:split{size=0.3}
  -- Create another split in the right of the remaining 2/3
  -- of the space; the resultant split is in the middle
  -- 1/3 of the display and has the focus.
  pane:split{size=0.5}
end)

return {}
```

This example creates a default window but makes it maximize on startup:

```lua
local wezterm = require 'wezterm'
local mux = wezterm.mux

wezterm.on("gui-startup", function()
  local tab, pane, window = mux.spawn_window{}
  window:gui_window():maximize()
end)

return {}
```

Here's a more elaborate example that configures two workspaces:

```lua
local wezterm = require 'wezterm'
local mux = wezterm.mux

wezterm.on("gui-startup", function()
  -- Set a workspace for coding on a current project
  -- Top pane is for the editor, bottom pane is for the build tool
  local project_dir = wezterm.home_dir .. "/wezterm"
  local tab, build_pane, window = mux.spawn_window{
    workspace="coding",
    cwd=project_dir,
  }
  local editor_pane = build_pane:split{
    direction="Top",
    size=0.6,
    cwd=project_dir
  }
  -- may as well kick off a build in that pane
  build_pane:send_text("cargo build\n")

  -- A workspace for interacting with a local machine that
  -- runs some docker containners for home automation
  local tab, pane, window = mux.spawn_window{
    workspace="automation",
    args={"ssh", "vault"},
  }

  -- We want to startup in the coding workspace
  mux.set_active_workspace("coding")
end)

return {}
```

See also:
* [wezterm.mux](../wezterm.mux/index.md)
