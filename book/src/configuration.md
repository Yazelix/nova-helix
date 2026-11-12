# Configuration

To override global configuration parameters, create a `config.toml` file located in your config directory:

- Linux and Mac: `~/.config/helix/config.toml`
- Windows: `%AppData%\helix\config.toml`

> 💡 You can easily open the config file by typing `:config-open` within Helix normal mode.

Nova Helix embeds `helix-term/src/default_config.toml` below your configuration.
It selects `theme = "carbonfox"`, `editor.bufferline = "multiple"`,
`editor.cursorline = true`, `editor.color-modes = true`,
`editor.soft-wrap.enable = true`, `editor.cursor-shape.insert = "bar"` and
`editor.indent-guides.render = true`. It applies even without a user config file
and is also used when startup falls back after a configuration error. The file
contains no keymaps, arrays or workspace-trust settings.

Set any of these keys in your own file to override it; omitted keys retain the
embedded defaults. This example restores the upstream choices:

```toml
theme = "default"

[editor]
bufferline = "never"
cursorline = false
color-modes = false

[editor.soft-wrap]
enable = false

[editor.cursor-shape]
insert = "block"

[editor.indent-guides]
render = false
```

Global configuration (including `-c FILE`) overrides the embedded file, and
trusted `.helix/config.toml` overrides the global configuration. Workspace trust
policy comes only from the global configuration, even if the workspace tries to
change it. `:config-open` opens your file, and `:config-reload` reloads these layers.
Managed applications can supply explicit configuration that overrides these
defaults too. Carbonfox paints its own background, including in a transparent
terminal window. It requires true color; without detected support, Helix falls
back to its default theme. Set `editor.true-color = true` to override detection
when your terminal supports true color but Helix does not detect it.

Example config:

```toml
theme = "onedark"

[editor]
line-number = "relative"
mouse = false

[editor.cursor-shape]
insert = "bar"
normal = "block"
select = "underline"

[editor.file-picker]
hidden = false
```

You can use a custom configuration file by specifying it with the `-c` or
`--config` command line argument, for example `hx -c path/to/custom-config.toml`.
Use `--config-dir path/to/helix` to change the directory used for configuration,
languages, themes, and runtime files while keeping Helix's other data locations
unchanged.
You can reload the config file by issuing the `:config-reload` command. Alternatively, on Unix operating systems, you can reload it by sending the USR1
signal to the Helix process, such as by using the command `pkill -USR1 hx`.

Finally, you can have a `config.toml` and a `languages.toml` local to a project by putting it under a `.helix` directory in your repository.
Its settings will be merged with the configuration directory and the built-in configuration.

