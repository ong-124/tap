# Tap Terminal Man Pages

This directory contains manual pages for Tap terminal emulator in scdoc format.

## Files

- `tap.1.scd` - Main Tap terminal manual page (section 1)
- `tap.5.scd` - Tap configuration file format manual page (section 5)  
- `tap-bindings.5.scd` - Tap key bindings manual page (section 5)

## Building

To build the man pages, you need `scdoc` installed:

### Install scdoc

**macOS (Homebrew):**
```bash
brew install scdoc
```

**Ubuntu/Debian:**
```bash
sudo apt install scdoc
```

**Arch Linux:**
```bash
sudo pacman -S scdoc
```

**From source:**
```bash
git clone https://git.sr.ht/~sircmpwn/scdoc
cd scdoc
make
sudo make install
```

### Build man pages

```bash
# Build all man pages
make -C extra/man

# Or build individually
scdoc < extra/man/tap.1.scd > tap.1
scdoc < extra/man/tap.5.scd > tap.5
scdoc < extra/man/tap-bindings.5.scd > tap-bindings.5
```

### Install man pages

```bash
# Install to system man directory (requires sudo)
sudo cp tap.1 /usr/local/share/man/man1/
sudo cp tap.5 /usr/local/share/man/man5/
sudo cp tap-bindings.5 /usr/local/share/man/man5/

# Update man database
sudo mandb
```

### View man pages

```bash
man tap
man 5 tap
man 5 tap-bindings
```

## Format

The man pages are written in scdoc format, which is a simple markup language for writing man pages. See the [scdoc documentation](https://git.sr.ht/~sircmpwn/scdoc) for syntax details.