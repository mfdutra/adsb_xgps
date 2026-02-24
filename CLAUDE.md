# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

adsb_xgps is a Rust bridge that connects to a dump1090 ADS-B receiver (SBS/BaseStation protocol on port 30003), tracks a specific aircraft by callsign, and broadcasts its position in XGPS format over UDP (default 255.255.255.255:49002, configurable via `--broadcast`). This allows ADS-B-tracked aircraft to appear in apps that support XGPS (e.g., ForeFlight). It also provides a web UI for monitoring all tracked aircraft and changing the tracked callsign at runtime.

## Build & Test Commands

```bash
cargo build                  # Debug build
cargo build --release        # Release build
cargo test --verbose         # Run all tests (33 unit tests)
cargo test <test_name>       # Run a single test by name
cargo clippy                 # Lint (code uses clippy pragmas)
```

## Architecture

Two-file application (~1100 lines total) with four concurrent Tokio async tasks coordinated via `tokio::select!`:

- **`src/main.rs`** (~600 lines) — CLI args, `Aircraft` struct, SBS parsing, XGPS broadcasting, debug printing, and 23 unit tests for SBS parsing
- **`src/web.rs`** (~470 lines) — Axum-based HTTP server serving an HTML dashboard and JSON API, plus 10 unit tests for endpoints

### Async Tasks

1. **`sbs_reader`** — TCP client that connects to dump1090 on port 30003, parses SBS CSV lines via `parse_sbs_line()`, and updates a shared aircraft map
2. **`xgps_broadcaster`** — UDP broadcaster that finds the target callsign in the aircraft map, converts units (feet→meters, knots→m/s), and sends XGPS-formatted position strings every second
3. **`web::run`** — HTTP server on port 8081 with three endpoints: `GET /` (HTML dashboard with auto-refresh), `GET /data` (JSON aircraft list), `POST /track` (change tracked callsign)
4. **`debug_printer`** — Optional task (enabled via `--debug`) that periodically prints all tracked aircraft

### Shared State

- `AircraftMap` = `Arc<RwLock<HashMap<String, Aircraft>>>` keyed by ICAO hex code
- `TrackedCallsign` = `Arc<RwLock<String>>` — the callsign currently being tracked, mutable via web UI

## Protocols

- **Input (SBS):** CSV format, MSG types 1-8 carry different fields (callsign, position, altitude, speed, etc.)
- **Output (XGPS):** `XGPSadsb_xgps,{lon},{lat},{alt_m:.1},{track:.2},{gs_ms:.1}`
- **Web JSON (`GET /data`):** `{"tracked":"...","aircraft":[{hex, callsign, lat, lon, alt_ft, gs_kt, track, age, tracking}]}`

## Dependencies

Four dependencies: `axum` v0.8 for the web server, `clap` v4 (derive) for CLI args, `serde` v1 (derive) for JSON serialization, and `tokio` v1 (full) for the async runtime. Dev-dependencies: `http-body-util`, `serde_json`, `tower` (for axum handler testing via `oneshot`).
