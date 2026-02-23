use crate::{AircraftMap, TrackedCallsign};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub async fn run(aircraft_map: AircraftMap, tracked_callsign: TrackedCallsign) {
    let listener = TcpListener::bind("0.0.0.0:8081")
        .await
        .expect("Failed to bind web server on port 8081");
    println!("Web server listening on http://0.0.0.0:8081");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("Web server accept error: {}", e);
                continue;
            }
        };

        let map = aircraft_map.clone();
        let callsign = tracked_callsign.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, map, callsign).await {
                eprintln!("Web server connection error: {}", e);
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    aircraft_map: AircraftMap,
    tracked_callsign: TrackedCallsign,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;

    // Read remaining headers (and body content-length if present)
    let mut content_length: usize = 0;
    let mut header_line = String::new();
    loop {
        header_line.clear();
        buf_reader.read_line(&mut header_line).await?;
        if header_line.trim().is_empty() {
            break;
        }
        if let Some(val) = header_line.strip_prefix("Content-Length: ") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }

    let method = parts[0];
    let path = parts[1];

    match (method, path) {
        ("GET", "/") => {
            let body = build_page(&aircraft_map, &tracked_callsign).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            writer.write_all(response.as_bytes()).await?;
        }
        ("GET", "/data") => {
            let body = build_json(&aircraft_map, &tracked_callsign).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            writer.write_all(response.as_bytes()).await?;
        }
        ("POST", "/track") => {
            let mut body_buf = vec![0u8; content_length];
            if content_length > 0 {
                tokio::io::AsyncReadExt::read_exact(&mut buf_reader, &mut body_buf).await?;
            }
            let body_str = String::from_utf8_lossy(&body_buf);

            // Parse "callsign=VALUE" from form body
            let new_callsign = body_str
                .split('&')
                .find_map(|pair| pair.strip_prefix("callsign="))
                .unwrap_or("")
                .trim();

            if !new_callsign.is_empty() {
                let decoded = url_decode(new_callsign);
                *tracked_callsign.write().await = decoded.clone();
                println!("Web: now tracking callsign '{}'", decoded);
            }

            // Redirect back to /
            let response = "HTTP/1.1 303 See Other\r\nLocation: /\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            writer.write_all(response.as_bytes()).await?;
        }
        _ => {
            let response =
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            writer.write_all(response.as_bytes()).await?;
        }
    }

    Ok(())
}

fn url_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        match b {
            b'+' => result.push(' '),
            b'%' => {
                let hi = chars.next().unwrap_or(b'0');
                let lo = chars.next().unwrap_or(b'0');
                let hex = [hi, lo];
                if let Ok(s) = std::str::from_utf8(&hex) {
                    if let Ok(val) = u8::from_str_radix(s, 16) {
                        result.push(val as char);
                        continue;
                    }
                }
                result.push('%');
                result.push(hi as char);
                result.push(lo as char);
            }
            _ => result.push(b as char),
        }
    }
    result
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

async fn build_json(aircraft_map: &AircraftMap, tracked_callsign: &TrackedCallsign) -> String {
    let map = aircraft_map.read().await;
    let current = tracked_callsign.read().await.clone();

    let mut entries = Vec::new();
    let mut sorted: Vec<_> = map.iter().collect();
    sorted.sort_by_key(|(hex, _)| hex.to_string());

    for (hex, a) in &sorted {
        let cs = a.callsign.as_deref().unwrap_or("");
        let lat = a.latitude.map_or("null".to_string(), |v| format!("{v:.5}"));
        let lon = a
            .longitude
            .map_or("null".to_string(), |v| format!("{v:.5}"));
        let alt = a
            .altitude_ft
            .map_or("null".to_string(), |v| format!("{v:.0}"));
        let gs = a
            .ground_speed_kt
            .map_or("null".to_string(), |v| format!("{v:.0}"));
        let trk = a.track.map_or("null".to_string(), |v| format!("{v:.0}"));
        let age = a.last_updated.elapsed().as_secs();

        let tracking = cs.eq_ignore_ascii_case(&current) && !cs.is_empty();

        entries.push(format!(
            r#"{{"hex":"{}","callsign":"{}","lat":{},"lon":{},"alt_ft":{},"gs_kt":{},"track":{},"age":{},"tracking":{}}}"#,
            hex, cs, lat, lon, alt, gs, trk, age, tracking
        ));
    }

    format!(
        r#"{{"tracked":"{}","aircraft":[{}]}}"#,
        current,
        entries.join(",")
    )
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
