mod web;

use clap::Parser;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tokio::time::{self, Duration, Instant};

#[derive(Parser)]
#[command(about = "Bridge ADS-B SBS data from dump1090 to XGPS protocol over UDP")]
struct Args {
    /// dump1090 server hostname or IP
    server: String,

    /// Flight callsign to track
    callsign: String,

    /// UDP broadcast address for XGPS output
    #[arg(long, default_value = "255.255.255.255")]
    broadcast: String,

    /// Print all tracked aircraft every second
    #[arg(long)]
    debug: bool,
}

pub struct Aircraft {
    pub callsign: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude_ft: Option<f64>,
    pub ground_speed_kt: Option<f64>,
    pub track: Option<f64>,
    pub last_updated: Instant,
}

pub type AircraftMap = Arc<RwLock<HashMap<String, Aircraft>>>;
pub type TrackedCallsign = Arc<RwLock<String>>;

fn parse_sbs_line(line: &str, aircraft_map: &mut HashMap<String, Aircraft>) {
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 22 {
        return;
    }
    if fields[0] != "MSG" {
        return;
    }

    let msg_type: u8 = match fields[1].trim().parse() {
        Ok(t) => t,
        Err(_) => return,
    };

    let hex_ident = fields[4].trim();
    if hex_ident.is_empty() {
        return;
    }

    let aircraft = aircraft_map
        .entry(hex_ident.to_string())
        .or_insert_with(|| Aircraft {
            callsign: None,
            latitude: None,
            longitude: None,
            altitude_ft: None,
            ground_speed_kt: None,
            track: None,
            last_updated: Instant::now(),
        });

    match msg_type {
        1 => {
            let cs = fields[10].trim();
            if !cs.is_empty() {
                aircraft.callsign = Some(cs.to_string());
            }
        }
        2 => {
            if let Ok(v) = fields[11].trim().parse() {
                aircraft.altitude_ft = Some(v);
            }
            if let Ok(v) = fields[12].trim().parse() {
                aircraft.ground_speed_kt = Some(v);
            }
            if let Ok(v) = fields[13].trim().parse() {
                aircraft.track = Some(v);
            }
            if let Ok(v) = fields[14].trim().parse() {
                aircraft.latitude = Some(v);
            }
            if let Ok(v) = fields[15].trim().parse() {
                aircraft.longitude = Some(v);
            }
        }
        3 => {
            if let Ok(v) = fields[11].trim().parse() {
                aircraft.altitude_ft = Some(v);
            }
            if let Ok(v) = fields[14].trim().parse() {
                aircraft.latitude = Some(v);
            }
            if let Ok(v) = fields[15].trim().parse() {
                aircraft.longitude = Some(v);
            }
        }
        4 => {
            if let Ok(v) = fields[12].trim().parse() {
                aircraft.ground_speed_kt = Some(v);
            }
            if let Ok(v) = fields[13].trim().parse() {
                aircraft.track = Some(v);
            }
        }
        5 | 7 => {
            if let Ok(v) = fields[11].trim().parse() {
                aircraft.altitude_ft = Some(v);
            }
        }
        _ => {}
    }

    aircraft.last_updated = Instant::now();
}

async fn sbs_reader(server: String, aircraft_map: AircraftMap) {
    let addr = format!("{}:30003", server);

    loop {
        println!("Connecting to {}...", addr);

        let stream = match TcpStream::connect(&addr).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to connect to {}: {}. Retrying in 1s...", addr, e);
                time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        println!("Connected to {}", addr);
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let mut map = aircraft_map.write().await;
            parse_sbs_line(&line, &mut map);
        }

        eprintln!("Connection to {} closed. Reconnecting in 1s...", addr);
        time::sleep(Duration::from_secs(1)).await;
    }
}

async fn xgps_broadcaster(callsign: TrackedCallsign, aircraft_map: AircraftMap, broadcast: String) {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("Failed to bind UDP socket");
    socket
        .set_broadcast(true)
        .expect("Failed to enable broadcast");

    let mut interval = time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

        let callsign = callsign.read().await.clone();
        let map = aircraft_map.read().await;
        let found = map.values().find(|a| {
            a.callsign
                .as_ref()
                .is_some_and(|cs| cs.eq_ignore_ascii_case(&callsign))
        });

        let Some(aircraft) = found else {
            continue;
        };

        if aircraft.last_updated.elapsed() > Duration::from_secs(5) {
            continue;
        }

        let (Some(lon), Some(lat), Some(alt_ft), Some(track), Some(gs_kt)) = (
            aircraft.longitude,
            aircraft.latitude,
            aircraft.altitude_ft,
            aircraft.track,
            aircraft.ground_speed_kt,
        ) else {
            continue;
        };

        let alt_m = alt_ft * 0.3048;
        let gs_ms = gs_kt * 0.514444;

        let msg = format!("XGPSadsb_xgps,{lon},{lat},{alt_m:.1},{track:.2},{gs_ms:.1}");
        if let Err(e) = socket
            .send_to(msg.as_bytes(), format!("{}:49002", broadcast))
            .await
        {
            eprintln!("UDP send error: {}", e);
        } else {
            println!("{}", msg);
        }
    }
}

async fn debug_printer(aircraft_map: AircraftMap) {
    let mut interval = time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

        let map = aircraft_map.read().await;
        if map.is_empty() {
            continue;
        }

        println!("--- Aircraft ({}) ---", map.len());
        for (hex, a) in map.iter() {
            let cs = a.callsign.as_deref().unwrap_or("-");
            let lat = a.latitude.map_or("-".to_string(), |v| format!("{v:.5}"));
            let lon = a.longitude.map_or("-".to_string(), |v| format!("{v:.5}"));
            let alt = a
                .altitude_ft
                .map_or("-".to_string(), |v| format!("{v:.0}ft"));
            let gs = a
                .ground_speed_kt
                .map_or("-".to_string(), |v| format!("{v:.0}kt"));
            let trk = a.track.map_or("-".to_string(), |v| format!("{v:.0}°"));
            let age = a.last_updated.elapsed().as_secs();
            println!("  {hex} {cs:>8}  {lat:>10} {lon:>11}  {alt:>7} {gs:>5} {trk:>4}  {age}s ago");
        }
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let aircraft_map: AircraftMap = Arc::new(RwLock::new(HashMap::new()));
    let tracked_callsign: TrackedCallsign = Arc::new(RwLock::new(args.callsign));

    let reader_handle = tokio::spawn(sbs_reader(args.server, aircraft_map.clone()));
    let broadcaster_handle =
        tokio::spawn(xgps_broadcaster(tracked_callsign.clone(), aircraft_map.clone(), args.broadcast));
    let web_handle = tokio::spawn(web::run(aircraft_map.clone(), tracked_callsign.clone()));

    #[allow(clippy::collapsible_if)]
    if args.debug {
        let debug_handle = tokio::spawn(debug_printer(aircraft_map));
        tokio::select! {
            r = reader_handle => { if let Err(e) = r { eprintln!("SBS reader task failed: {}", e); } }
            r = broadcaster_handle => { if let Err(e) = r { eprintln!("XGPS broadcaster task failed: {}", e); } }
            r = debug_handle => { if let Err(e) = r { eprintln!("Debug printer task failed: {}", e); } }
            r = web_handle => { if let Err(e) = r { eprintln!("Web server task failed: {}", e); } }
        }
    } else {
        tokio::select! {
            r = reader_handle => { if let Err(e) = r { eprintln!("SBS reader task failed: {}", e); } }
            r = broadcaster_handle => { if let Err(e) = r { eprintln!("XGPS broadcaster task failed: {}", e); } }
            r = web_handle => { if let Err(e) = r { eprintln!("Web server task failed: {}", e); } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_map() -> HashMap<String, Aircraft> {
        HashMap::new()
    }

    fn sbs_line(msg_type: u8, hex: &str, fields: &[(usize, &str)]) -> String {
        let mut line_parts: Vec<String> = vec![String::new(); 22];
        line_parts[0] = "MSG".to_string();
        line_parts[1] = msg_type.to_string();
        line_parts[4] = hex.to_string();
        for &(idx, val) in fields {
            line_parts[idx] = val.to_string();
        }
        line_parts.join(",")
    }

    // --- MSG type 1: callsign ---

    #[test]
    fn msg1_sets_callsign() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(1, "ABC123", &[(10, "TEST456")]), &mut map);

        assert_eq!(map.len(), 1);
        let a = map.get("ABC123").unwrap();
        assert_eq!(a.callsign.as_deref(), Some("TEST456"));
    }

    #[test]
    fn msg1_empty_callsign_ignored() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(1, "ABC123", &[]), &mut map);

        let a = map.get("ABC123").unwrap();
        assert!(a.callsign.is_none());
    }

    #[test]
    fn msg1_does_not_set_position() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(1, "ABC123", &[(10, "TEST456")]), &mut map);

        let a = map.get("ABC123").unwrap();
        assert!(a.latitude.is_none());
        assert!(a.longitude.is_none());
        assert!(a.altitude_ft.is_none());
    }

    // --- MSG type 3: airborne position ---

    #[test]
    fn msg3_sets_position_and_altitude() {
        let mut map = empty_map();
        parse_sbs_line(
            &sbs_line(
                3,
                "ABC123",
                &[(11, "35000"), (14, "50.123"), (15, "-6.456")],
            ),
            &mut map,
        );

        let a = map.get("ABC123").unwrap();
        assert_eq!(a.altitude_ft, Some(35000.0));
        assert_eq!(a.latitude, Some(50.123));
        assert_eq!(a.longitude, Some(-6.456));
    }

    #[test]
    fn msg3_altitude_only_no_position() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(3, "ABC123", &[(11, "24000")]), &mut map);

        let a = map.get("ABC123").unwrap();
        assert_eq!(a.altitude_ft, Some(24000.0));
        assert!(a.latitude.is_none());
        assert!(a.longitude.is_none());
    }

    // --- MSG type 4: velocity ---

    #[test]
    fn msg4_sets_speed_and_track() {
        let mut map = empty_map();
        parse_sbs_line(
            &sbs_line(4, "ABC123", &[(12, "420"), (13, "179")]),
            &mut map,
        );

        let a = map.get("ABC123").unwrap();
        assert_eq!(a.ground_speed_kt, Some(420.0));
        assert_eq!(a.track, Some(179.0));
    }

    #[test]
    fn msg4_does_not_set_position() {
        let mut map = empty_map();
        parse_sbs_line(
            &sbs_line(4, "ABC123", &[(12, "420"), (13, "179")]),
            &mut map,
        );

        let a = map.get("ABC123").unwrap();
        assert!(a.latitude.is_none());
        assert!(a.altitude_ft.is_none());
    }

    // --- MSG type 2: surface position ---

    #[test]
    fn msg2_sets_all_fields() {
        let mut map = empty_map();
        parse_sbs_line(
            &sbs_line(
                2,
                "ABC123",
                &[
                    (11, "100"),
                    (12, "25"),
                    (13, "90"),
                    (14, "51.47"),
                    (15, "-0.46"),
                ],
            ),
            &mut map,
        );

        let a = map.get("ABC123").unwrap();
        assert_eq!(a.altitude_ft, Some(100.0));
        assert_eq!(a.ground_speed_kt, Some(25.0));
        assert_eq!(a.track, Some(90.0));
        assert_eq!(a.latitude, Some(51.47));
        assert_eq!(a.longitude, Some(-0.46));
    }

    // --- MSG type 5 and 7: altitude only ---

    #[test]
    fn msg5_sets_altitude() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(5, "ABC123", &[(11, "37000")]), &mut map);

        let a = map.get("ABC123").unwrap();
        assert_eq!(a.altitude_ft, Some(37000.0));
        assert!(a.ground_speed_kt.is_none());
    }

    #[test]
    fn msg7_sets_altitude() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(7, "ABC123", &[(11, "39000")]), &mut map);

        let a = map.get("ABC123").unwrap();
        assert_eq!(a.altitude_ft, Some(39000.0));
    }

    // --- Aggregation across multiple messages ---

    #[test]
    fn multiple_messages_aggregate_into_single_aircraft() {
        let mut map = empty_map();

        parse_sbs_line(&sbs_line(1, "AABBCC", &[(10, "UAL123")]), &mut map);
        parse_sbs_line(
            &sbs_line(3, "AABBCC", &[(11, "35000"), (14, "40.0"), (15, "-74.0")]),
            &mut map,
        );
        parse_sbs_line(
            &sbs_line(4, "AABBCC", &[(12, "450"), (13, "270")]),
            &mut map,
        );

        assert_eq!(map.len(), 1);
        let a = map.get("AABBCC").unwrap();
        assert_eq!(a.callsign.as_deref(), Some("UAL123"));
        assert_eq!(a.altitude_ft, Some(35000.0));
        assert_eq!(a.latitude, Some(40.0));
        assert_eq!(a.longitude, Some(-74.0));
        assert_eq!(a.ground_speed_kt, Some(450.0));
        assert_eq!(a.track, Some(270.0));
    }

    #[test]
    fn position_updates_overwrite_previous() {
        let mut map = empty_map();

        parse_sbs_line(
            &sbs_line(3, "ABC123", &[(11, "30000"), (14, "50.0"), (15, "-6.0")]),
            &mut map,
        );
        parse_sbs_line(
            &sbs_line(3, "ABC123", &[(11, "31000"), (14, "50.1"), (15, "-5.9")]),
            &mut map,
        );

        let a = map.get("ABC123").unwrap();
        assert_eq!(a.altitude_ft, Some(31000.0));
        assert_eq!(a.latitude, Some(50.1));
        assert_eq!(a.longitude, Some(-5.9));
    }

    #[test]
    fn different_hex_idents_create_separate_entries() {
        let mut map = empty_map();

        parse_sbs_line(&sbs_line(1, "AAA111", &[(10, "FLIGHT1")]), &mut map);
        parse_sbs_line(&sbs_line(1, "BBB222", &[(10, "FLIGHT2")]), &mut map);

        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get("AAA111").unwrap().callsign.as_deref(),
            Some("FLIGHT1")
        );
        assert_eq!(
            map.get("BBB222").unwrap().callsign.as_deref(),
            Some("FLIGHT2")
        );
    }

    // --- Invalid / malformed input ---

    #[test]
    fn non_msg_line_ignored() {
        let mut map = empty_map();
        parse_sbs_line("STA,,,,,,,,,,,,,,,,,,,,,", &mut map);
        assert!(map.is_empty());
    }

    #[test]
    fn too_few_fields_ignored() {
        let mut map = empty_map();
        parse_sbs_line("MSG,1,,,ABC123", &mut map);
        assert!(map.is_empty());
    }

    #[test]
    fn empty_hex_ident_ignored() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(1, "", &[(10, "TEST")]), &mut map);
        assert!(map.is_empty());
    }

    #[test]
    fn invalid_msg_type_ignored() {
        let mut map = empty_map();
        parse_sbs_line("MSG,X,,,ABC123,,,,,,,,,,,,,,,,,", &mut map);
        assert!(map.is_empty());
    }

    #[test]
    fn non_numeric_altitude_ignored() {
        let mut map = empty_map();
        parse_sbs_line(
            &sbs_line(
                3,
                "ABC123",
                &[(11, "notanumber"), (14, "50.0"), (15, "-6.0")],
            ),
            &mut map,
        );

        let a = map.get("ABC123").unwrap();
        assert!(a.altitude_ft.is_none());
        assert_eq!(a.latitude, Some(50.0));
    }

    // --- XGPS format ---

    #[test]
    fn xgps_format_string() {
        let lon: f64 = -80.11;
        let lat: f64 = 34.55;
        let alt_ft: f64 = 3937.0;
        let track: f64 = 359.05;
        let gs_kt: f64 = 108.089;

        let alt_m = alt_ft * 0.3048;
        let gs_ms = gs_kt * 0.514444;

        let msg = format!("XGPSadsb_xgps,{lon},{lat},{alt_m:.1},{track:.2},{gs_ms:.1}");
        assert!(msg.starts_with("XGPSadsb_xgps,"));

        let parts: Vec<&str> = msg
            .strip_prefix("XGPSadsb_xgps,")
            .unwrap()
            .split(',')
            .collect();
        assert_eq!(parts.len(), 5);

        let parsed_lon: f64 = parts[0].parse().unwrap();
        let parsed_lat: f64 = parts[1].parse().unwrap();
        let parsed_alt: f64 = parts[2].parse().unwrap();
        let parsed_track: f64 = parts[3].parse().unwrap();
        let parsed_gs: f64 = parts[4].parse().unwrap();

        assert!((parsed_lon - (-80.11)).abs() < 0.01);
        assert!((parsed_lat - 34.55).abs() < 0.01);
        assert!((parsed_alt - 1200.1).abs() < 0.2);
        assert!((parsed_track - 359.05).abs() < 0.01);
        assert!((parsed_gs - 55.6).abs() < 0.2);
    }

    // --- Unit conversions ---

    #[test]
    fn feet_to_meters_conversion() {
        let feet = 10000.0_f64;
        let meters = feet * 0.3048;
        assert!((meters - 3048.0).abs() < 0.01);
    }

    #[test]
    fn knots_to_ms_conversion() {
        let knots = 100.0_f64;
        let ms = knots * 0.514444;
        assert!((ms - 51.4444).abs() < 0.001);
    }

    // --- MSG type 6 and 8: no crash ---

    #[test]
    fn msg6_creates_entry_no_position() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(6, "ABC123", &[]), &mut map);

        let a = map.get("ABC123").unwrap();
        assert!(a.altitude_ft.is_none());
        assert!(a.callsign.is_none());
    }

    #[test]
    fn msg8_creates_entry_no_data() {
        let mut map = empty_map();
        parse_sbs_line(&sbs_line(8, "ABC123", &[]), &mut map);

        let a = map.get("ABC123").unwrap();
        assert!(a.callsign.is_none());
        assert!(a.altitude_ft.is_none());
    }
}
