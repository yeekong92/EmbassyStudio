# Role & Goal
Act as an expert Senior Software Engineer specializing in Rust, desktop GUI development, and embedded systems. Your task is to develop a desktop PC application named "Embassy Studio"—a open source modern alternative to STM32CubeMX. 

Standalone Windows `.exe` and feature a robust local file-system project manager.


## 1. Launch Screen & Project Management
When the `.exe` launches, the user must *not* immediately see the editor. They must be greeted by a "Project Hub" screen with two main sections:

### A. Recent Projects (Left Side)
- Display a clickable list of recent projects.


### B. New Project / Open Project (Right Side)
- **Open Project:** A button that opens a native OS folder-picker dialog to browse and select an existing project folder.
- **New Project Wizard:** 
  1. Input field for "Project Name".
  2. Button to select the "Local Folder Location" (Path picker).
  3. "MCU Selector" dropdown (searchable list of STM32 chips).
  4. "Create" button.
- **Open-Source Data Core (`embassy-rs/stm32-data`):** You MUST NOT hardcode chip names, pinouts, or clock trees. You must utilize the open-source repository `embassy-rs/stm32-data` (specifically the parsed JSON/YAML data). This repository is the absolute source of truth for the app's database.


## 3. Core Editor Requirements (Main GUI)
Once a project is loaded, display the main editor with the following panes:
- **MCU Pinout:** Interactive 2D schematic.
- **Clock Configuration Map:** Visual tree showing LSE/HSE inputs, PLL multiplexers, and dividers for the AHB/APB buses.
- **Peripheral Configuration:** Side panel to enable/disable specific hardware blocks.


## 4. The Code Generation Engine 
Code must be generated directly into the project folder.


## 5. UI/UX & Developer Environment
- Ensure the output project structure plays nicely with standard IDEs (like VS Code or STM32CubeIDE).
- Modern developer aesthetics.

