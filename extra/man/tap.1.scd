TAP(1)

# NAME

Tap - A cross-platform, GPU-accelerated terminal emulator.

# SYNOPSIS

Tap is a modern terminal emulator built with Rust and WebGPU that provides high performance rendering with support for modern terminal features including sixel graphics, hyperlinks, and custom shaders.

# FLAGS

*-h, --help*

	Prints help information.

*-V, --version*

	Prints version information.

# OPTIONS

*-e, --command* _<COMMAND>..._

	Command and args to execute (must be last argument).

*-w, --working-dir* _<WORKING_DIR>_

	Start the shell in the specified working directory.

*--write-config* _[<PATH>]_

	Writes the config to a given path or the default location.

	Tap doesn't create the config file for you, but it looks for one in the
	following locations on UNIX systems:

		. _$TAP_CONFIG_HOME/config.toml_
		. _$XDG_CONFIG_HOME/tap/config.toml_
		. _$HOME/.config/tap/config.toml_

	On Windows, the config file will be looked for in:

		. _%USERPROFILE%\\AppData\\Local\\tap\\config.toml_

*--enable-log-file*

	Writes the logs to a file inside the config directory.

*--title-placeholder* _<TITLE>_

	Start window with specified title.

*--title* _<TITLE>_

	Set a fixed window title, ignoring titles set by the shell.

*--hold*

	Keep the window open after the command exits, until a key is pressed.

*--app-id* _<APP_ID>_

	Set the Wayland app_id or X11 WM_CLASS (Linux/BSD only).

# CONFIGURATION

Tap uses TOML format for configuration. The configuration file is automatically
reloaded when changed. See *tap*(5) for detailed configuration documentation.

# FEATURES

Tap supports many modern terminal features:

- GPU-accelerated rendering with WebGPU
- Sixel graphics protocol
- iTerm2 image protocol
- Kitty keyboard protocol
- Hyperlinks (OSC 8)
- Vi mode for navigation
- Split panels
- Custom shaders (RetroArch compatible)
- Adaptive themes
- Font ligatures
- Wide color gamut support

# ENVIRONMENT

*TAP_CONFIG_HOME*

	Custom path for Tap configuration directory. When set, Tap will look for
	configuration files in this directory instead of the default locations.

# SEE ALSO

*tap*(5)

# BUGS

Found a bug? Please report it at _https://github.com/raphamorim/tap/issues_.

# MAINTAINERS

- Raphael Amorim <rapha850@gmail.com>
