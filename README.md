# Embassy Studio

An open-source desktop alternative to STM32CubeMX for [Embassy](https://embassy.dev) firmware,
written in Rust ([egui](https://github.com/emilk/egui)). It ships as a single Windows `.exe`.

All chip knowledge comes from [`embassy-rs/stm32-data`](https://github.com/embassy-rs/stm32-data):
the chip list, packages and pin positions, alternate functions, peripherals, interrupts and the RCC
register layout that the clock tree is built from. Nothing chip-specific is hardcoded.

## Features

- **Project Hub** on launch: recent projects (left), *Open Project* folder picker and the
  *New Project* wizard (name, location, searchable MCU selector, package) on the right.
- **Pinout**: interactive package drawing for QFP/QFN, SO/TSSOP and BGA/WLCSP. Click a pin to
  configure it, right-click to assign GPIO or any alternate function, wheel to zoom, drag to pan.
  Conflicts (e.g. two EXTI pins on one line) are flagged.
- **Schematic**: import your hardware schematic and configure pins where they are drawn. The MCU's
  pins are located from the text of the drawing and an interactive node is placed beside each one;
  see [Schematic overlay](#schematic-overlay).
- **Clock Configuration**: oscillators → PLL source → /M → ×N → P/Q/R → SYSCLK mux → AHB → APB
  buses, discovered from the RCC enums (`SW`, `PLLSRC`, `PLLM`, `HPRE`, `PPRE*`, …), with live
  frequencies and kernel-clock muxes per peripheral.
- **Peripherals**: enable/disable hardware blocks grouped by kind, route each signal to a pin, set
  baud rate / SPI and I²C frequency, see bus clock, IRQs and DMA requests.
- **Code generation** straight into the project folder: an Embassy crate with `Cargo.toml`,
  `build.rs`, `.cargo/config.toml` (probe-rs runner), `rust-toolchain.toml`, `src/board.rs`
  (clocks, GPIO, EXTI, UART, SPI, I²C drivers) and a `src/main.rs` that is yours to edit.
  `.vscode/` gets rust-analyzer, probe-rs debug and build task settings.

### Clock code support

The clock tree is shown for every family. `board.rs` gets real `config.rcc` code for the F2/F4/F7
and G4 RCC flavours; for other families the chosen tree is written as comments next to
`Config::default()`, so the project still builds and you can fill in `config.rcc` by hand.

## Schematic overlay

*Schematic → Import schematic…* accepts:

| Format | Background | How pins are found |
|--------|------------|--------------------|
| SVG | the file as it is | `<text>` elements, and KiCad's stroked-text groups |
| PDF | each page converted to SVG | glyph positions, including invisible (searchable) text |
| KiCad `.kicad_sch` (6+) | redrawn from the file | symbol pin positions |
| Altium `.SchDoc` | redrawn from the file | pin records |

Text is cross-referenced against the pin names of the project's package (`PA0`, `PC14-OSC32_IN`,
`VDD`, …). For a multi-page PDF only the pages that show pins of the MCU are imported. Every sheet is
stored as an SVG in the project's `schematic/` folder and rendered sharply at any zoom.

- **Edit mode** (calibration) opens after an import. Drag a node beside its pin, press `R` to turn its
  label, `Del` to remove it, and place pins that were not found from the list on the right. *Save*
  writes the positions to `embassy-studio.json`, relative to the sheet (0–1), so they do not depend
  on zoom or window size.
- **View mode** is for configuration: nodes are locked and glued to the drawing while you pan and
  zoom. Click a node to pick the pin's function, right-click for its label. Colours match the Pinout
  tab; the assigned function is written next to the node, and pins the selected peripheral can use
  get a ring.

Importing a sheet again refreshes the drawing and keeps the nodes you calibrated.

The overlay is purely visual. There is no netlist: wires are not traced, other components are not
interpreted and nothing is checked against the schematic.

## Data source

By default chip files are fetched on demand from `github.com/embassy-rs/stm32-data-generated` and
cached in `%LOCALAPPDATA%\EmbassyStudio\stm32-data`, so the app works offline after first use.
*Sync database* refreshes the chip list. *Data source → Local stm32-data folder…* points at a local
clone of `stm32-data-generated` (or an `stm32-data` checkout with `build/data`).

## Building

```powershell
cargo build --release          # MSVC toolchain (Visual Studio Build Tools installed)
./build.ps1                    # works on any Windows Rust setup, see below
```

`build.ps1` exists for machines on the `x86_64-pc-windows-gnu` toolchain without 64-bit MinGW
binutils (or with an old 32-bit `C:\MinGW` on `PATH`). There, current `windows-*` crates fail to
link or produce an `.exe` that crashes on start. The script uses only Rust-shipped tools
(`llvm-tools` as `dlltool`, `rust-lld` as the linker) and changes nothing outside `target/`.

The result is `target/release/embassy-studio.exe`.

## Command line

```text
embassy-studio                      # Project Hub
embassy-studio <project-dir>        # open a project directly
embassy-studio --generate <dir>     # regenerate code without the GUI (scripts / CI)
embassy-studio --import-schematic <dir> <file>   # import a schematic and auto-locate the pins
```

## Generated project layout

| Path | Regenerated? |
|------|--------------|
| `embassy-studio.json` | the project file |
| `schematic/*.svg` | written by *Import schematic*; node positions are in the project file |
| `src/board.rs` | always |
| `Cargo.toml` | yes, except the user section at the bottom |
| `build.rs`, `.cargo/config.toml`, `rust-toolchain.toml`, `.vscode/*.json` | yes |
| `src/main.rs`, `README.md`, `.gitignore` | created once, then yours |
