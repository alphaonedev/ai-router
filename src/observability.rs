use crate::fusion::FusionEvent;
use ai_router::{read_events, savings, Measurement, RouterEvent, Tier};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct Snapshot {
    events: Vec<RouterEvent>,
    fusion_events: Vec<FusionEvent>,
    model_counts: Vec<(String, usize)>,
    source_counts: Vec<(String, usize)>,
    tier_counts: [usize; 3],
    p50: u128,
    p95: u128,
    cache_rate: f64,
    regain: Option<f64>,
    target_met: Option<bool>,
}
fn measurements(path: Option<&Path>) -> Option<(f64, bool)> {
    let raw = fs::read_to_string(path?).ok()?;
    let records: Vec<Measurement> = raw
        .lines()
        .filter(|s| !s.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .ok()?;
    let result = savings(&records).ok()?;
    Some((result.regain_fraction, result.target_met))
}
fn snapshot(cache_dir: &Path, measurements_path: Option<&Path>) -> Snapshot {
    let mut events = read_events(cache_dir);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    events.retain(|e| e.timestamp <= now && now - e.timestamp < 86_400);
    let mut fusion_events: Vec<FusionEvent> =
        fs::read_to_string(cache_dir.join("fusion-events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .filter(|e: &FusionEvent| e.timestamp <= now && now - e.timestamp < 86_400)
            .collect();
    fusion_events.sort_by_key(|e| e.timestamp);
    events.sort_by_key(|e| e.timestamp);
    let mut models = BTreeMap::new();
    let mut sources = BTreeMap::new();
    let mut tiers = [0; 3];
    let mut latencies = Vec::new();
    for e in &events {
        *models.entry(e.model.clone()).or_insert(0) += 1;
        *sources.entry(e.source.clone()).or_insert(0) += 1;
        tiers[match e.tier {
            Tier::Fast => 0,
            Tier::Balanced => 1,
            Tier::Deep => 2,
        }] += 1;
        latencies.push(e.duration_ms);
    }
    latencies.sort_unstable();
    let percentile = |p: f64| -> u128 {
        if latencies.is_empty() {
            0
        } else {
            latencies[((latencies.len() as f64 * p).ceil() as usize)
                .saturating_sub(1)
                .min(latencies.len() - 1)]
        }
    };
    let mut model_counts: Vec<_> = models.into_iter().collect();
    model_counts.sort_by_key(|b| std::cmp::Reverse(b.1));
    let mut source_counts: Vec<_> = sources.into_iter().collect();
    source_counts.sort_by_key(|b| std::cmp::Reverse(b.1));
    let cache_rate = if events.is_empty() {
        0.0
    } else {
        events.iter().filter(|e| e.source == "cache").count() as f64 / events.len() as f64
    };
    let (regain, target_met) =
        measurements(measurements_path).map_or((None, None), |(r, t)| (Some(r), Some(t)));
    Snapshot {
        events,
        fusion_events,
        model_counts,
        source_counts,
        tier_counts: tiers,
        p50: percentile(0.5),
        p95: percentile(0.95),
        cache_rate,
        regain,
        target_met,
    }
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn ago(ts: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let delta = now.saturating_sub(ts);
    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else {
        format!("{}h ago", delta / 3600)
    }
}
fn rows(items: &[(String, usize)], total: usize, class: &str) -> String {
    let mut out = String::new();
    for (label, count) in items.iter().take(6) {
        let pct = if total == 0 {
            0.0
        } else {
            100.0 * (*count as f64) / (total as f64)
        };
        out.push_str(&format!("<div class='barrow'><span class='barlabel'>{}</span><div class='bartrack'><div class='barfill {}' style='width:{:.1}%'></div></div><strong>{}</strong></div>",esc(label),class,pct,count));
    }
    if out.is_empty() {
        "<div class='empty'>Decisions will appear here as tasks are routed.</div>".into()
    } else {
        out
    }
}
fn render(s: &Snapshot) -> String {
    let total = s.events.len();
    let tier_items = vec![
        ("Fast".to_string(), s.tier_counts[0]),
        ("Balanced".into(), s.tier_counts[1]),
        ("Deep".into(), s.tier_counts[2]),
    ];
    let mut feed = String::new();
    for e in s.events.iter().rev().take(12) {
        feed.push_str(&format!("<tr><td><span class='dot {}'></span>{}</td><td>{}</td><td><span class='pill'>{:?}</span></td><td>{}</td><td class='mono'>{} ms</td><td class='mono dim'>{}</td></tr>",esc(&e.source),esc(&e.client),esc(&e.model),e.tier,esc(&e.source),e.duration_ms,ago(e.timestamp)));
    }
    if feed.is_empty() {
        feed="<tr><td colspan='6' class='empty'>No decisions yet. Run ai-router in another terminal to start the live feed.</td></tr>".into();
    }
    let regain = s
        .regain
        .map_or("—".to_string(), |r| format!("{:.1}%", r * 100.0));
    let regain_note = match s.target_met {
        Some(true) => "50% target met with quality parity",
        Some(false) => "Target not met or quality below baseline",
        None => "Add a measurements file to assess the 50% target",
    };
    let mut fusion_feed = String::new();
    for e in s.fusion_events.iter().rev().take(12) {
        fusion_feed.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class='mono'>{}</td><td class='mono'>{}</td><td class='mono dim'>{}</td></tr>", esc(&e.stage), esc(&e.client), esc(&e.model), esc(&e.status), e.input_tokens.map_or("—".into(), |n| n.to_string()), e.output_tokens.map_or("—".into(), |n| n.to_string()), ago(e.timestamp)));
    }
    if fusion_feed.is_empty() {
        fusion_feed = "<tr><td colspan='7' class='empty'>No Fusion runs yet.</td></tr>".into();
    }
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="refresh" content="2"><meta name="color-scheme" content="dark"><title>ai-router / live observatory</title><style>{CSS}</style></head><body><div class="shell"><header><div class="brand"><div class="logo">A<span>↗</span></div><div><strong>ai-router</strong><small>LIVE OBSERVATORY</small></div></div><div class="status"><span class="beacon"></span> LOCAL STREAM <span class="divider">/</span> 24H WINDOW <span class="divider">/</span> 2S REFRESH</div></header><main><section class="hero"><div><div class="eyebrow"><span class="pulse"></span> ROUTING INTELLIGENCE · RUST ENGINE</div><h1>Every decision,<br><em>in view.</em></h1><p>See how tasks flow through local policy, cache, PAW, System One, Jev, and Fusion. Prompt text stays out of this view and the event log.</p></div><div class="orb" role="img" aria-label="Three orbiting model tiers"><div class="ring ring1"></div><div class="ring ring2"></div><div class="ring ring3"></div><div class="orbcore"><span>{total}</span><small>DECISIONS</small></div><i class="spark spark1"></i><i class="spark spark2"></i><i class="spark spark3"></i></div></section><section class="metrics" aria-label="Routing metrics"><div class="metric"><span class="metric-label">DECISIONS · 24H</span><strong>{total}</strong><small>Local routing events</small></div><div class="metric"><span class="metric-label">CACHE REUSE</span><strong>{:.0}<sup>%</sup></strong><small>Decisions served from cache</small></div><div class="metric"><span class="metric-label">DECISION P50</span><strong>{}<sup>ms</sup></strong><small>Observed local latency</small></div><div class="metric"><span class="metric-label">DECISION P95</span><strong>{}<sup>ms</sup></strong><small>Tail latency</small></div><div class="metric featured"><span class="metric-label">COMPUTE REGAIN</span><strong>{}</strong><small>{}</small></div></section><section class="charts"><article class="panel"><div class="panelhead"><div><span class="overline">01 / ALLOCATION</span><h2>Model mix</h2></div><span class="glyph">◈</span></div>{}</article><article class="panel"><div class="panelhead"><div><span class="overline">02 / DECISION SOURCE</span><h2>Router path</h2></div><span class="glyph">⌁</span></div>{}</article><article class="panel"><div class="panelhead"><div><span class="overline">03 / CAPABILITY</span><h2>Tier distribution</h2></div><span class="glyph">◇</span></div>{}</article></section><section class="pipeline"><div class="panelhead"><div><span class="overline">DECISION PIPELINE</span><h2>From task to model</h2></div></div><div class="steps"><div><b>01</b><strong>Explicit</strong><small>Caller choice</small></div><span>→</span><div><b>02</b><strong>Cache</strong><small>Equivalent task</small></div><span>→</span><div><b>03</b><strong>Rules</strong><small>Local policy</small></div><span>→</span><div><b>04</b><strong>PAW / System One / Jev</strong><small>Optional signal</small></div><span>→</span><div><b>05</b><strong>Harness</strong><small>Claude · Codex · Grok</small></div></div></section><section class="feed"><div class="panelhead"><div><span class="overline">FUSION WORKFLOW</span><h2>Lead and sidekick</h2></div><span class="live"><span class="beacon"></span> LIVE</span></div><div class="tablewrap"><table><thead><tr><th>STAGE</th><th>HARNESS</th><th>MODEL</th><th>STATUS</th><th>INPUT</th><th>OUTPUT</th><th>WHEN</th></tr></thead><tbody>{fusion_feed}</tbody></table></div></section><section class="feed"><div class="panelhead"><div><span class="overline">MOST RECENT</span><h2>Decision stream</h2></div><span class="live"><span class="beacon"></span> LIVE</span></div><div class="tablewrap"><table><thead><tr><th>HARNESS</th><th>MODEL</th><th>TIER</th><th>SOURCE</th><th>LATENCY</th><th>WHEN</th></tr></thead><tbody>{feed}</tbody></table></div></section></main><footer><span>ai-router · localhost only · no task text logged</span><span>Rust-powered routing observability</span></footer></div></body></html>"##,
        s.cache_rate * 100.0,
        s.p50,
        s.p95,
        esc(&regain),
        regain_note,
        rows(&s.model_counts, total, "gold"),
        rows(&s.source_counts, total, "teal"),
        rows(&tier_items, total, "purple")
    )
}
const CSS: &str = r#"
@import url('https://fonts.googleapis.com/css2?family=Archivo:wght@500;600;700;800;900&family=IBM+Plex+Mono:wght@400;500;600&family=IBM+Plex+Sans:wght@400;500;600&display=swap');
:root{--bg:#0b0b0b;--card:#151511;--card2:#1b1a15;--line:#323025;--text:#f5f0e3;--muted:#a8a190;--gold:#e3ad33;--teal:#68b69b;--purple:#a699d3}*{box-sizing:border-box}html{background:var(--bg)}body{margin:0;background:radial-gradient(circle at 75% 0%,#33260d 0,transparent 27%),radial-gradient(circle at 0% 60%,#11241f 0,transparent 28%),var(--bg);color:var(--text);font:15px/1.55 'IBM Plex Sans',sans-serif}.shell{max-width:1440px;margin:auto;padding:0 34px}header{height:78px;display:flex;justify-content:space-between;align-items:center;border-bottom:1px solid var(--line)}.brand{display:flex;gap:12px;align-items:center}.logo{width:43px;height:43px;background:var(--gold);border-radius:11px;color:#111;font:900 26px Archivo,sans-serif;display:grid;place-items:center;letter-spacing:-.12em}.logo span{font-size:15px;align-self:start;margin-left:-6px}.brand strong{font:800 20px Archivo;letter-spacing:-.04em;display:block;line-height:1.1}.brand small,.status,.eyebrow,.overline,.metric-label,.live,thead,footer{font:600 11px 'IBM Plex Mono',monospace;letter-spacing:.11em}.brand small{color:var(--muted)}.status{color:var(--muted);display:flex;align-items:center;gap:10px}.divider{color:#645b47}.beacon,.pulse{display:inline-block;width:8px;height:8px;border-radius:50%;background:#65d69d;box-shadow:0 0 0 5px #65d69d22,0 0 18px #65d69d88}.hero{min-height:340px;display:flex;justify-content:space-between;align-items:center;padding:34px 0 28px;border-bottom:1px solid var(--line);overflow:hidden}.eyebrow{color:var(--gold);display:flex;gap:11px;align-items:center}h1,h2{font-family:Archivo,sans-serif;letter-spacing:-.05em}h1{font-size:clamp(3rem,5.8vw,5.6rem);line-height:.98;margin:18px 0}h1 em{font-style:normal;color:var(--gold)}.hero p{max-width:520px;color:var(--muted);font-size:17px}.orb{width:295px;height:295px;position:relative;margin-right:8%;flex:none}.ring{position:absolute;border:1px solid #9b793555;border-radius:50%;inset:0}.ring1{inset:24px;border-color:#a47a3290}.ring2{inset:56px;border-style:dashed;border-color:#817249a0}.ring3{inset:87px;border-color:#bc9d54bb}.orbcore{position:absolute;inset:105px;border-radius:50%;background:var(--gold);color:#171209;display:flex;flex-direction:column;align-items:center;justify-content:center;box-shadow:0 0 55px #b4862e55}.orbcore span{font:800 44px Archivo;line-height:1}.orbcore small{font:600 9px 'IBM Plex Mono';letter-spacing:.1em}.spark{position:absolute;width:10px;height:10px;border-radius:50%;background:var(--teal);box-shadow:0 0 22px var(--teal)}.spark1{top:17px;left:85px}.spark2{right:34px;bottom:55px;background:var(--gold)}.spark3{left:47px;bottom:76px;background:var(--purple)}.metrics{display:grid;grid-template-columns:repeat(5,1fr);gap:12px;padding:24px 0}.metric,.panel,.pipeline,.feed{background:linear-gradient(135deg,#1b1a15,#12120f);border:1px solid var(--line);border-radius:15px}.metric{padding:20px;min-height:132px}.metric-label{color:var(--muted)}.metric strong{display:block;font:800 37px Archivo;letter-spacing:-.06em;margin:12px 0 1px;line-height:1}.metric strong sup{font-size:.45em;color:var(--gold);margin-left:3px}.metric small{color:var(--muted);font-size:11px}.metric.featured{background:linear-gradient(125deg,#51370d,#28200e);border-color:#8c6828}.metric.featured strong{color:var(--gold)}.charts{display:grid;grid-template-columns:repeat(3,1fr);gap:13px}.panel{padding:24px;min-height:305px}.panelhead{display:flex;justify-content:space-between;align-items:flex-start;margin-bottom:27px}.overline{color:var(--gold)}h2{font-size:23px;margin:5px 0 0}.glyph{font:34px Archivo;color:#b39a5e}.barrow{display:grid;grid-template-columns:90px 1fr 28px;gap:10px;align-items:center;margin:15px 0;font-size:12px}.barlabel{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;color:#dbd5c5}.bartrack{height:9px;border-radius:9px;background:#353329;overflow:hidden}.barfill{height:100%;border-radius:9px}.barfill.gold{background:linear-gradient(90deg,#9d6916,#e9b952)}.barfill.teal{background:linear-gradient(90deg,#1a735c,#70c8a4)}.barfill.purple{background:linear-gradient(90deg,#675a95,#b4a5e2)}.barrow strong{font:600 12px 'IBM Plex Mono';text-align:right}.empty{color:var(--muted);font-size:13px;padding:20px 0}.pipeline,.feed{padding:24px;margin-top:13px}.steps{display:flex;align-items:center;justify-content:space-between;gap:10px}.steps>div{flex:1;background:#23221b;border:1px solid #413a2b;border-radius:11px;padding:14px;min-height:88px}.steps b{color:var(--gold);font:600 11px 'IBM Plex Mono';display:block}.steps strong{display:block;font:700 16px Archivo}.steps small{color:var(--muted);font-size:11px}.steps>span{color:var(--gold);font-size:22px}.feed .panelhead{margin-bottom:15px}.live{color:#80cdaa;display:flex;gap:9px;align-items:center}.tablewrap{overflow:auto}table{width:100%;border-collapse:collapse;text-align:left;font-size:13px}th{color:var(--muted);font-weight:600;padding:12px 10px;border-bottom:1px solid var(--line)}td{padding:12px 10px;border-bottom:1px solid #2c2a22}tbody tr:last-child td{border:0}.dot{display:inline-block;width:7px;height:7px;border-radius:50%;background:var(--gold);margin-right:8px}.dot.cache{background:var(--teal)}.dot.paw,.dot.system_one{background:var(--purple)}.pill{background:#2c281c;color:#e5bf68;border-radius:6px;padding:4px 8px;font-size:11px}.mono{font-family:'IBM Plex Mono',monospace}.dim{color:var(--muted)}footer{border-top:1px solid var(--line);margin-top:22px;padding:28px 0 40px;color:var(--muted);display:flex;justify-content:space-between}@media(max-width:1050px){.metrics{grid-template-columns:repeat(3,1fr)}.charts{grid-template-columns:1fr 1fr}.charts .panel:last-child{grid-column:1/-1}.orb{margin-right:0}}@media(max-width:730px){.shell{padding:0 18px}.status{display:none}.hero{min-height:300px}.orb{display:none}.metrics{grid-template-columns:1fr 1fr}.charts{grid-template-columns:1fr}.charts .panel:last-child{grid-column:auto}.steps{overflow-x:auto}.steps>div{min-width:130px}.steps>span{flex:none}.metric.featured{grid-column:1/-1}footer{gap:10px;flex-wrap:wrap}}@media(prefers-reduced-motion:no-preference){.beacon,.pulse{animation:pulse 2s ease-in-out infinite}@keyframes pulse{50%{box-shadow:0 0 0 8px #65d69d11,0 0 24px #65d69daa}}}
"#;
pub fn serve(cache_dir: &Path, measurements_path: Option<&Path>, port: u16) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    println!("Dashboard: http://{addr}/");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        let body = render(&snapshot(cache_dir, measurements_path));
        let response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; img-src data:\r\nConnection: close\r\n\r\n",body.len());
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(body.as_bytes());
    }
    Ok(())
}
fn terminal(s: &Snapshot) -> String {
    let total = s.events.len();
    let mut out = String::new();
    out.push_str(
        "\x1b[2J\x1b[H\x1b[38;5;220m◆ ai-router\x1b[0m  LIVE OBSERVATORY  ·  24h window\n\n",
    );
    out.push_str(&format!(
        "Decisions {:>5}   Cache {:>5.0}%   P50 {:>4} ms   P95 {:>4} ms   Regain {}\n\n",
        total,
        s.cache_rate * 100.0,
        s.p50,
        s.p95,
        s.regain
            .map_or("—".into(), |r| format!("{:.1}%", r * 100.0))
    ));
    out.push_str("MODEL MIX\n");
    for (m, n) in s.model_counts.iter().take(6) {
        out.push_str(&format!(
            "  {:<24} {:>4}  {}\n",
            m,
            n,
            "█".repeat((n * 24).checked_div(total).unwrap_or(0).max(1))
        ));
    }
    out.push_str("\nRECENT DECISIONS\n");
    for e in s.events.iter().rev().take(10) {
        out.push_str(&format!(
            "  {:<7} {:<22} {:<9} {:<8} {:>4}ms  {}\n",
            e.client,
            e.model,
            format!("{:?}", e.tier),
            e.source,
            e.duration_ms,
            ago(e.timestamp)
        ));
    }
    if s.events.is_empty() {
        out.push_str("  Waiting for routed tasks...\n")
    };
    out.push_str("\nFUSION PHASES\n");
    for e in s.fusion_events.iter().rev().take(8) {
        out.push_str(&format!(
            "  {:<16} {:<8} {:<22} {:<9} in {:>6} out {:>6}  {}\n",
            e.stage,
            e.client,
            e.model,
            e.status,
            e.input_tokens.map_or("—".into(), |n| n.to_string()),
            e.output_tokens.map_or("—".into(), |n| n.to_string()),
            ago(e.timestamp)
        ));
    }
    if s.fusion_events.is_empty() {
        out.push_str("  Waiting for Fusion runs...\n");
    }
    out.push_str("\nPress Ctrl-C to stop. Prompt text is never logged.\n");
    out
}
pub fn watch(cache_dir: &Path, measurements_path: Option<&Path>, interval: u64, once: bool) {
    loop {
        print!("{}", terminal(&snapshot(cache_dir, measurements_path)));
        let _ = std::io::stdout().flush();
        if once {
            break;
        }
        std::thread::sleep(Duration::from_secs(interval.max(1)));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dashboard_has_live_sections() {
        let d = tempfile::tempdir().unwrap();
        let html = render(&snapshot(d.path(), None));
        assert!(html.contains("Decision stream"));
        assert!(html.contains("meta http-equiv=\"refresh\""));
        assert!(!html.contains("<script"));
    }
}
