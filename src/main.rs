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

    /// Print all tracked aircraft every second
    #[arg(long)]
    debug: bool,
}

struct Aircraft {
    callsign: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    altitude_ft: Option<f64>,
    ground_speed_kt: Option<f64>,
    track: Option<f64>,
    last_updated: Instant,
}

type AircraftMap = Arc<RwLock<HashMap<String, Aircraft>>>;

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
    println!("Connecting to {}...", addr);

    let stream = TcpStream::connect(&addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to connect to {}: {}", addr, e));

    println!("Connected to {}", addr);
    let reader = BufReader::new(stream);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let mut map = aircraft_map.write().await;
        parse_sbs_line(&line, &mut map);
    }

    eprintln!("Connection to {} closed", addr);
}

async fn xgps_broadcaster(callsign: String, aircraft_map: AircraftMap) {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .expect("Failed to bind UDP socket");
    socket
        .set_broadcast(true)
        .expect("Failed to enable broadcast");

    let mut interval = time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

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
            .send_to(msg.as_bytes(), "255.255.255.255:49002")
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
            let alt = a.altitude_ft.map_or("-".to_string(), |v| format!("{v:.0}ft"));
            let gs = a.ground_speed_kt.map_or("-".to_string(), |v| format!("{v:.0}kt"));
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

    let reader_handle = tokio::spawn(sbs_reader(args.server, aircraft_map.clone()));
    let broadcaster_handle = tokio::spawn(xgps_broadcaster(args.callsign, aircraft_map.clone()));

    if args.debug {
        let debug_handle = tokio::spawn(debug_printer(aircraft_map));
        tokio::select! {
            r = reader_handle => { if let Err(e) = r { eprintln!("SBS reader task failed: {}", e); } }
            r = broadcaster_handle => { if let Err(e) = r { eprintln!("XGPS broadcaster task failed: {}", e); } }
            r = debug_handle => { if let Err(e) = r { eprintln!("Debug printer task failed: {}", e); } }
        }
    } else {
        tokio::select! {
            r = reader_handle => { if let Err(e) = r { eprintln!("SBS reader task failed: {}", e); } }
            r = broadcaster_handle => { if let Err(e) = r { eprintln!("XGPS broadcaster task failed: {}", e); } }
        }
    }
}
