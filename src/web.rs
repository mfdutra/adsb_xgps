use crate::{AircraftMap, TrackedCallsign};
use axum::extract::State;
use axum::response::{Html, Json, Redirect};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

struct AppState {
    aircraft_map: AircraftMap,
    tracked_callsign: TrackedCallsign,
}

#[derive(Deserialize)]
struct TrackForm {
    callsign: String,
}

#[derive(Serialize)]
struct DataResponse {
    tracked: String,
    aircraft: Vec<AircraftEntry>,
}

#[derive(Serialize)]
struct AircraftEntry {
    hex: String,
    callsign: String,
    lat: Option<f64>,
    lon: Option<f64>,
    alt_ft: Option<f64>,
    gs_kt: Option<f64>,
    track: Option<f64>,
    age: u64,
    tracking: bool,
}

pub async fn run(aircraft_map: AircraftMap, tracked_callsign: TrackedCallsign) {
    let state = Arc::new(AppState {
        aircraft_map,
        tracked_callsign,
    });

    let app = Router::new()
        .route("/", get(get_index))
        .route("/data", get(get_data))
        .route("/track", post(post_track))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8081")
        .await
        .expect("Failed to bind web server on port 8081");
    println!("Web server listening on http://0.0.0.0:8081");

    axum::serve(listener, app)
        .await
        .expect("Web server failed");
}

async fn get_index(State(state): State<Arc<AppState>>) -> Html<String> {
    Html(build_page(&state.aircraft_map, &state.tracked_callsign).await)
}

async fn get_data(State(state): State<Arc<AppState>>) -> Json<DataResponse> {
    let map = state.aircraft_map.read().await;
    let current = state.tracked_callsign.read().await.clone();

    let mut entries: Vec<AircraftEntry> = map
        .iter()
        .map(|(hex, a)| {
            let cs = a.callsign.as_deref().unwrap_or("");
            let tracking = cs.eq_ignore_ascii_case(&current) && !cs.is_empty();
            AircraftEntry {
                hex: hex.clone(),
                callsign: cs.to_string(),
                lat: a.latitude,
                lon: a.longitude,
                alt_ft: a.altitude_ft,
                gs_kt: a.ground_speed_kt,
                track: a.track,
                age: a.last_updated.elapsed().as_secs(),
                tracking,
            }
        })
        .collect();

    entries.sort_by(|a, b| a.hex.cmp(&b.hex));

    Json(DataResponse {
        tracked: current,
        aircraft: entries,
    })
}

async fn post_track(
    State(state): State<Arc<AppState>>,
    Form(form): Form<TrackForm>,
) -> Redirect {
    let callsign = form.callsign.trim().to_string();
    if !callsign.is_empty() {
        *state.tracked_callsign.write().await = callsign.clone();
        println!("Web: now tracking callsign '{}'", callsign);
    }
    Redirect::to("/")
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

async fn build_page(aircraft_map: &AircraftMap, tracked_callsign: &TrackedCallsign) -> String {
    let map = aircraft_map.read().await;
    let current = tracked_callsign.read().await.clone();

    let mut rows = String::new();
    let mut sorted: Vec<_> = map.iter().collect();
    sorted.sort_by_key(|(hex, _)| hex.to_string());

    for (hex, a) in &sorted {
        let cs = a.callsign.as_deref().unwrap_or("-");
        let lat = a.latitude.map_or("-".to_string(), |v| format!("{v:.5}"));
        let lon = a.longitude.map_or("-".to_string(), |v| format!("{v:.5}"));
        let alt = a.altitude_ft.map_or("-".to_string(), |v| format!("{v:.0}"));
        let gs = a
            .ground_speed_kt
            .map_or("-".to_string(), |v| format!("{v:.0}"));
        let trk = a.track.map_or("-".to_string(), |v| format!("{v:.0}"));
        let age = a.last_updated.elapsed().as_secs();

        let is_tracked = cs.eq_ignore_ascii_case(&current) && cs != "-";
        let highlight = if is_tracked {
            r#" class="tracked""#
        } else {
            ""
        };

        let track_btn = if cs != "-" && !is_tracked {
            format!(
                r#"<form method="POST" action="/track" style="margin:0"><input type="hidden" name="callsign" value="{}"><button type="submit">Track</button></form>"#,
                escape_html(cs)
            )
        } else if is_tracked {
            "Tracking".to_string()
        } else {
            String::new()
        };

        rows.push_str(&format!(
            "<tr{}><td>{}</td><td>{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}s</td><td>{}</td></tr>\n",
            highlight,
            escape_html(hex),
            escape_html(cs),
            lat, lon, alt, gs, trk, age, track_btn
        ));
    }

    let count = map.len();
    drop(map);

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>adsb_xgps</title>
<style>
body {{ font-family: monospace; background: #1a1a2e; color: #e0e0e0; margin: 20px; }}
h1 {{ color: #00d4ff; }}
table {{ border-collapse: collapse; width: 100%; }}
th, td {{ border: 1px solid #333; padding: 6px 10px; text-align: left; }}
th {{ background: #16213e; color: #00d4ff; }}
tr:nth-child(even) {{ background: #1f2b47; }}
tr:nth-child(odd) {{ background: #1a1a2e; }}
tr.tracked {{ background: #0a3d0a !important; }}
.r {{ text-align: right; }}
button {{ background: #00d4ff; color: #1a1a2e; border: none; padding: 3px 10px; cursor: pointer; font-family: monospace; }}
button:hover {{ background: #00a8cc; }}
#status {{ color: #888; margin-bottom: 10px; }}
</style>
</head>
<body>
<h1>adsb_xgps</h1>
<div id="status">Tracking: <strong>{current}</strong> &mdash; {count} aircraft</div>
<table>
<thead><tr><th>Hex</th><th>Callsign</th><th>Latitude</th><th>Longitude</th><th>Alt (ft)</th><th>GS (kt)</th><th>Track</th><th>Age</th><th></th></tr></thead>
<tbody id="tbody">
{rows}</tbody>
</table>
<script>
function refresh() {{
  fetch('/data')
    .then(r => r.json())
    .then(d => {{
      document.getElementById('status').innerHTML =
        'Tracking: <strong>' + d.tracked + '</strong> &mdash; ' + d.aircraft.length + ' aircraft';
      let html = '';
      for (const a of d.aircraft) {{
        const cls = a.tracking ? ' class="tracked"' : '';
        const cs = a.callsign || '-';
        const lat = a.lat !== null ? a.lat.toFixed(5) : '-';
        const lon = a.lon !== null ? a.lon.toFixed(5) : '-';
        const alt = a.alt_ft !== null ? a.alt_ft : '-';
        const gs = a.gs_kt !== null ? a.gs_kt : '-';
        const trk = a.track !== null ? a.track : '-';
        let btn = '';
        if (cs !== '-' && !a.tracking) {{
          btn = '<form method="POST" action="/track" style="margin:0">' +
            '<input type="hidden" name="callsign" value="' + cs + '">' +
            '<button type="submit">Track</button></form>';
        }} else if (a.tracking) {{
          btn = 'Tracking';
        }}
        html += '<tr' + cls + '><td>' + a.hex + '</td><td>' + cs +
          '</td><td class="r">' + lat + '</td><td class="r">' + lon +
          '</td><td class="r">' + alt + '</td><td class="r">' + gs +
          '</td><td class="r">' + trk + '</td><td class="r">' + a.age + 's</td><td>' + btn + '</td></tr>';
      }}
      document.getElementById('tbody').innerHTML = html;
    }})
    .catch(() => {{}});
}}
setInterval(refresh, 1000);
</script>
</body>
</html>"#,
        current = escape_html(&current),
        count = count,
        rows = rows,
    )
}
