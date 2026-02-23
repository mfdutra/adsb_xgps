# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

adsb_xgps is a Rust bridge that connects to a dump1090 ADS-B receiver (SBS/BaseStation protocol on port 30003), tracks a specific aircraft by callsign, and broadcasts its position in XGPS format over UDP (255.255.255.255:49002). This allows ADS-B-tracked aircraft to appear in apps that support XGPS (e.g., ForeFlight).

## Build & Test Commands

```bash
cargo build                  # Debug build
cargo build --release        # Release build
cargo test --verbose         # Run all tests (44 unit tests)
cargo test <test_name>       # Run a single test by name
cargo clippy                 # Lint (code uses clippy pragmas)
```

## Architecture

Single-file application (`src/main.rs`, ~590 lines) with three concurrent Tokio async tasks coordinated via `tokio::select!`:

1. **`sbs_reader`** — TCP client that connects to dump1090, parses SBS CSV lines via `parse_sbs_line()`, and updates a shared aircraft map
2. **`xgps_broadcaster`** — UDP broadcaster that finds the target callsign in the aircraft map, converts units (feet→meters, knots→m/s), and sends XGPS-formatted position strings every second
3. **`debug_printer`** — Optional task (enabled via `--debug`) that periodically prints all tracked aircraft

Shared state is `Arc<RwLock<HashMap<String, Aircraft>>>` keyed by ICAO hex code.

## Protocols

- **Input (SBS):** CSV format, MSG types 1-8 carry different fields (callsign, position, altitude, speed, etc.)
- **Output (XGPS):** `XGPSadsb_xgps,{lon},{lat},{alt_m:.1},{track:.2},{gs_ms:.1}`

## Dependencies

Only two dependencies: `clap` v4 (derive) for CLI args, `tokio` v1 (full) for async runtime.
