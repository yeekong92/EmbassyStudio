# Embassy Studio

An open-source desktop alternative to STM32CubeMX for [Embassy](https://embassy.dev) firmware,
written in Rust ([egui](https://github.com/emilk/egui)). It ships as a single Windows `.exe` and
generates projects for **STM32** (embassy-stm32) and **ESP32** (esp-hal + esp-rtos) chips.

Nothing chip-specific is hardcoded:

- **STM32** knowledge comes from [`embassy-rs/stm32-data`](https://github.com/embassy-rs/stm32-data):
  the chip list, packages and pin positions, alternate functions, peripherals, interrupts and the
  RCC register layout that the clock tree is built from.
- **ESP32** knowledge comes from esp-hal's
  [`esp-metadata`](https://github.com/esp-rs/esp-hal/tree/main/esp-metadata) device files, read at
  the esp-hal release the generated code uses: GPIOs and their IO MUX / analog functions, pin
  restrictions (strapping, flash, input-only), driver instances and their GPIO-matrix signals,
  the Rust target and esp-hal's CPU clock presets. See [ESP32 support](#esp32-support).

## Features

- **Project Hub** on launch: recent projects (left), *Open Project* folder picker and the
  *New Project* wizard (name, location, searchable MCU selector, package) on the right.
- **File menu** in a project: *New Project*, *Open Project*, *Recent Projects* and *Save As*
  (`Ctrl+Shift+S`), which copies the configuration and the schematic sheets into a new folder.
- **Pinout**: interactive package drawing for QFP/QFN, SO/TSSOP and BGA/WLCSP. Click a pin to
  choose GPIO or any alternate function; the label prompt follows by itself. Wheel to zoom, drag to
  pan. Conflicts (e.g. two EXTI pins on one line) are flagged.
- **Schematic**: import your hardware schematic and configure pins where they are drawn. The MCU's
  pins are located from the text of the drawing and an interactive node is placed beside each one;
  see [Schematic overlay](#schematic-overlay).
- **Clock Configuration**: oscillators → PLL source → /M → ×N → P/Q/R → SYSCLK mux → AHB → APB
  buses, discovered from the RCC enums (`SW`, `PLLSRC`, `PLLM`, `HPRE`, `PPRE*`, …), with live
  frequencies and kernel-clock muxes per peripheral.
- **Peripherals**: enable/disable hardware blocks grouped by kind, route each signal to a pin, set
  baud rate / SPI and I²C frequency, see bus clock, IRQs and DMA requests.
- **Code generation** straight into the project folder: an Embassy crate with `Cargo.toml`,
  `build.rs`, `.cargo/config.toml` (probe-rs runner for STM32, espflash for ESP32),
  `rust-toolchain.toml`, `src/board.rs` (clocks, GPIO, EXTI, UART, SPI, I²C drivers) and a
  `src/main.rs` that is yours to edit. `.vscode/` gets rust-analyzer, probe-rs debug and build
  task settings.

### Clock code support

The clock tree is shown for every family. `board.rs` gets real `config.rcc` code for the F2/F4/F7
and G4 RCC flavours; for other families the chosen tree is written as comments next to
`Config::default()`, so the project still builds and you can fill in `config.rcc` by hand.

## ESP32 support

ESP32, -C2, -C3, -C5, -C6, -C61, -H2, -P4, -S2, -S3 and -S31 are listed in the MCU selector
(filter *ESP32*) next to the STM32 parts. They use the same editor:

- **Pinout** shows every GPIO of the chip. ESP32 parts route peripheral signals through a GPIO
  matrix, so any signal can go to any pin that can drive it (outputs are not offered on input-only
  pins); the pin's IO MUX and ADC functions are listed with it. Pins wired to flash or PSRAM on most
  modules are drawn as system pins, and the inspector warns about strapping pins. The metadata has
  no package pinouts, so the drawing is a GPIO overview, not the physical package.
- **Peripherals**: UART, SPI master, I²C master, I²S, TWAI, LEDC, RMT and ADC instances from the
  metadata. UART (baud rate), SPI (SCK frequency) and I²C (bus frequency) get async drivers in
  `board.rs`; the others are listed in `board::init` with the module to build them from.
- **Clock Configuration**: esp-hal sets up the clock tree from one `CpuClock` preset; pick one of
  the chip's presets (the maximum by default).
- **Code**: an [esp-generate](https://github.com/esp-rs/esp-generate)-style crate on esp-hal 1.2,
  esp-rtos (Embassy executor and time driver), esp-bootloader-esp-idf, esp-println and
  esp-backtrace, with `log` or `defmt` logging. `cargo run --release` flashes with
  [espflash](https://github.com/esp-rs/espflash) and opens the monitor. RISC-V chips build on stable
  Rust; Xtensa chips (ESP32, -S2, -S3) need the `esp` toolchain from
  [espup](https://github.com/esp-rs/espup).

ESP32 data is fetched from `github.com/esp-rs/esp-hal` at the pinned release tag and cached in
`%LOCALAPPDATA%\EmbassyStudio\esp-metadata`. Schematic import also recognises `IO4`-style pin
labels as `GPIO4`.

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
  label, `Del` to remove it, and place pins that were not found from the list on the right.
  `Ctrl+Z` / `Ctrl+Y` undo and redo. A **snap grid** makes nodes line up: type a pitch in the side
  panel or pick one (the symbol's pin pitch, half or a quarter of it) there or in the right-click
  menu; hold `Alt` to place a node off the grid. *Save* writes the positions to
  `embassy-studio.json`, relative to the sheet (0–1), so they do not depend on zoom or window size.
- **View mode** is for configuration: nodes are locked and glued to the drawing while you pan and
  zoom. Click a node to pick the pin's function, then type its label in the prompt that follows.
  Colours match the Pinout tab; the assigned function is written next to the node, and pins the
  selected peripheral can use get a ring. The side panel lists every pin with its status, function
  and label; picking a row points the pin out on the schematic.

Beside the sheet's name, **Refresh** reads the source file again (the nodes you calibrated stay
where they are) and **Close** closes the schematic so another one can be imported. The same commands are in the
menu that opens when you right-click the canvas.

The overlay is purely visual. There is no netlist: wires are not traced, other components are not
interpreted and nothing is checked against the schematic.

## Data source

By default chip files are fetched on demand from `github.com/embassy-rs/stm32-data-generated` and
cached in `%LOCALAPPDATA%\EmbassyStudio\stm32-data`, so the app works offline after first use.
*Sync database* refreshes the chip list (STM32 and ESP32). *Data source → Local stm32-data folder…* points at a local
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

The layout is the same for both platforms; only the dependencies, runner and toolchain differ.
| `src/main.rs`, `README.md`, `.gitignore` | created once, then yours |
