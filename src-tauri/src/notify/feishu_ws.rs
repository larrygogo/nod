//! Feishu long connection ("长连接"), so card button clicks reach this machine without a public URL.
//!
//! Protocol (same as the official SDKs):
//! 1. `POST {domain}/callback/ws/endpoint {"AppID","AppSecret"}` → a `wss://` URL (with `service_id`) and client config.
//! 2. Binary WebSocket messages are protobuf `Frame`s (pbbp2). `method` 0 = control (ping/pong), 1 = data.
//! 3. Send a `ping` control frame every `PingInterval` seconds.
//! 4. A data frame of type `event` carries the event JSON (split into `sum` parts when large). Reply with the same
//!    frame whose payload is `{"code":200,"data":"<base64 of the callback result JSON>"}`.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use super::feishu::FeishuClient;
use super::{ClickIds, OnAction};
use crate::{debug, info, warn};

// ---------------------------------------------------------------- protobuf (pbbp2.Frame), hand-written

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    pub seq_id: u64,
    pub log_id: u64,
    pub service: i32,
    pub method: i32,
    pub headers: Vec<(String, String)>,
    pub payload_encoding: Option<String>,
    pub payload_type: Option<String>,
    pub payload: Option<Vec<u8>>,
    pub log_id_new: Option<String>,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn put_key(out: &mut Vec<u8>, field: u32, wire: u8) {
    put_varint(out, ((field as u64) << 3) | wire as u64);
}
fn put_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    put_key(out, field, 2);
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn get_varint(b: &[u8], i: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i).ok_or_else(|| anyhow!("truncated varint"))?;
        *i += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte < 0x80 {
            return Ok(v);
        }
    }
    bail!("varint too long")
}
fn get_bytes<'a>(b: &'a [u8], i: &mut usize) -> Result<&'a [u8]> {
    let n = get_varint(b, i)? as usize;
    let s = b
        .get(*i..*i + n)
        .ok_or_else(|| anyhow!("truncated field"))?;
    *i += n;
    Ok(s)
}
fn skip(b: &[u8], i: &mut usize, wire: u64) -> Result<()> {
    match wire {
        0 => {
            get_varint(b, i)?;
        }
        1 => *i += 8,
        2 => {
            get_bytes(b, i)?;
        }
        5 => *i += 4,
        _ => bail!("unsupported wire type {wire}"),
    }
    Ok(())
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![];
        put_key(&mut o, 1, 0);
        put_varint(&mut o, self.seq_id);
        put_key(&mut o, 2, 0);
        put_varint(&mut o, self.log_id);
        put_key(&mut o, 3, 0);
        put_varint(&mut o, self.service as i64 as u64);
        put_key(&mut o, 4, 0);
        put_varint(&mut o, self.method as i64 as u64);
        for (k, v) in &self.headers {
            let mut h = vec![];
            put_bytes(&mut h, 1, k.as_bytes());
            put_bytes(&mut h, 2, v.as_bytes());
            put_bytes(&mut o, 5, &h);
        }
        if let Some(s) = &self.payload_encoding {
            put_bytes(&mut o, 6, s.as_bytes());
        }
        if let Some(s) = &self.payload_type {
            put_bytes(&mut o, 7, s.as_bytes());
        }
        if let Some(p) = &self.payload {
            put_bytes(&mut o, 8, p);
        }
        if let Some(s) = &self.log_id_new {
            put_bytes(&mut o, 9, s.as_bytes());
        }
        o
    }

    pub fn decode(b: &[u8]) -> Result<Frame> {
        let mut f = Frame::default();
        let mut i = 0;
        let s = |x: &[u8]| String::from_utf8_lossy(x).into_owned();
        while i < b.len() {
            let key = get_varint(b, &mut i)?;
            let (field, wire) = (key >> 3, key & 7);
            match (field, wire) {
                (1, 0) => f.seq_id = get_varint(b, &mut i)?,
                (2, 0) => f.log_id = get_varint(b, &mut i)?,
                (3, 0) => f.service = get_varint(b, &mut i)? as i32,
                (4, 0) => f.method = get_varint(b, &mut i)? as i32,
                (5, 2) => {
                    let h = get_bytes(b, &mut i)?;
                    let (mut j, mut k, mut v) = (0, String::new(), String::new());
                    while j < h.len() {
                        let hk = get_varint(h, &mut j)?;
                        match (hk >> 3, hk & 7) {
                            (1, 2) => k = s(get_bytes(h, &mut j)?),
                            (2, 2) => v = s(get_bytes(h, &mut j)?),
                            (_, w) => skip(h, &mut j, w)?,
                        }
                    }
                    f.headers.push((k, v));
                }
                (6, 2) => f.payload_encoding = Some(s(get_bytes(b, &mut i)?)),
                (7, 2) => f.payload_type = Some(s(get_bytes(b, &mut i)?)),
                (8, 2) => f.payload = Some(get_bytes(b, &mut i)?.to_vec()),
                (9, 2) => f.log_id_new = Some(s(get_bytes(b, &mut i)?)),
                (_, w) => skip(b, &mut i, w)?,
            }
        }
        Ok(f)
    }

    pub fn header(&self, k: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(hk, _)| hk == k)
            .map(|(_, v)| v.as_str())
    }
}

// ---------------------------------------------------------------- connection

fn endpoint(client: &FeishuClient) -> Result<(String, i32, u64)> {
    let url = format!("{}/callback/ws/endpoint", client.domain);
    let resp = ureq::post(&url)
        .set("locale", "zh")
        .timeout(Duration::from_secs(30))
        .send_json(json!({"AppID": client.app_id, "AppSecret": client.app_secret}));
    let v: Value = match resp {
        Ok(r) => r.into_json()?,
        Err(ureq::Error::Status(code, r)) => {
            let v: Value = r.into_json().unwrap_or_default();
            bail!(
                "long connection endpoint: HTTP {code} {}",
                v["msg"].as_str().unwrap_or("system busy")
            )
        }
        Err(e) => bail!("long connection endpoint: {e}"),
    };
    if v["code"].as_i64() != Some(0) {
        bail!(
            "long connection endpoint: code={} {}",
            v["code"],
            v["msg"].as_str().unwrap_or("")
        );
    }
    let conn = v["data"]["URL"]
        .as_str()
        .ok_or_else(|| anyhow!("no URL in endpoint response"))?
        .to_string();
    let service: i32 = conn
        .split(['?', '&'])
        .find_map(|p| p.strip_prefix("service_id="))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("no service_id in {conn}"))?;
    let ping = v["data"]["ClientConfig"]["PingInterval"]
        .as_u64()
        .unwrap_or(120)
        .max(10);
    Ok((conn, service, ping))
}

fn set_read_timeout(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, d: Duration) {
    let tcp = match ws.get_mut() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_mut(),
        _ => return,
    };
    let _ = tcp.set_read_timeout(Some(d));
}

fn ping_frame(service: i32) -> Frame {
    Frame {
        service,
        method: 0,
        headers: vec![("type".into(), "ping".into())],
        ..Default::default()
    }
}

/// Turn a `card.action.trigger` event into the callback result (toast + optional replacement card).
pub fn handle_event(event: &Value, on_action: OnAction) -> Value {
    if event["header"]["event_type"].as_str() != Some("card.action.trigger") {
        return Value::Null;
    }
    let ev = &event["event"];
    let g = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let ids = ClickIds {
        open_id: g(&ev["operator"]["open_id"]),
        union_id: g(&ev["operator"]["union_id"]),
        user_id: g(&ev["operator"]["user_id"]),
        message_id: g(&ev["context"]["open_message_id"]),
        chat_id: g(&ev["context"]["open_chat_id"]),
    };
    let value = ev["action"]["value"].clone();
    match on_action(value, ids) {
        Ok(card) => {
            let mut r = json!({"toast": {"type": "info", "content": "OK"}});
            if let Some(c) = card {
                r["card"] = json!({"type": "raw", "data": c});
            }
            r
        }
        // never fail the callback itself: Feishu would only show a generic error
        Err(e) => {
            json!({"toast": {"type": "error", "content": crate::util::truncate(&e.to_string(), 80)}})
        }
    }
}

/// Connect and process clicks until the connection drops (then returns an error; the caller reconnects).
pub fn run(client: &FeishuClient, on_action: OnAction) -> Result<()> {
    let (url, service, ping_secs) = endpoint(client)?;
    let (mut ws, _) =
        tungstenite::connect(url.as_str()).map_err(|e| anyhow!("long connection: {e}"))?;
    info!("feishu long connection established (service {service})");
    set_read_timeout(&mut ws, Duration::from_secs(5));
    let mut ping_every = Duration::from_secs(ping_secs);
    let mut last_ping = Instant::now() - ping_every; // ping right away
    let mut last_pong = Instant::now();
    // message_id -> (first seen, parts) for payloads split over several frames
    type Parts = (Instant, Vec<Option<Vec<u8>>>);
    let mut parts: HashMap<String, Parts> = HashMap::new();
    loop {
        // A half-open connection (sleep/resume, NAT timeout) never errors on its own: give up when the server has
        // not answered a ping for two intervals, and let the caller reconnect.
        if last_pong.elapsed() > ping_every * 2 + Duration::from_secs(30) {
            bail!(
                "no pong from Feishu for {}s; reconnecting",
                last_pong.elapsed().as_secs()
            );
        }
        if last_ping.elapsed() >= ping_every {
            ws.send(Message::Binary(ping_frame(service).encode()))?;
            last_ping = Instant::now();
            debug!("feishu ping");
        }
        let msg = match ws.read() {
            Ok(m) => m,
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                continue
            }
            Err(e) => bail!("long connection closed: {e}"),
        };
        let data = match msg {
            Message::Binary(b) => b,
            Message::Ping(p) => {
                ws.send(Message::Pong(p))?;
                continue;
            }
            Message::Close(c) => bail!("long connection closed by server: {c:?}"),
            _ => continue,
        };
        let mut frame = match Frame::decode(&data) {
            Ok(f) => f,
            Err(e) => {
                warn!("feishu: undecodable frame: {e}");
                continue;
            }
        };
        if frame.method == 0 {
            // control: a pong may carry new client settings
            if frame.header("type") == Some("pong") {
                last_pong = Instant::now();
                if let Some(p) = &frame.payload {
                    if let Some(n) = serde_json::from_slice::<Value>(p)
                        .ok()
                        .and_then(|v| v["PingInterval"].as_u64())
                    {
                        ping_every = Duration::from_secs(n.max(10));
                    }
                }
            }
            continue;
        }
        let sum: usize = frame
            .header("sum")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let seq: usize = frame
            .header("seq")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let msg_id = frame.header("message_id").unwrap_or("").to_string();
        let mut payload = frame.payload.clone().unwrap_or_default();
        if sum > 1 {
            parts.retain(|_, (t, _)| t.elapsed() < Duration::from_secs(5));
            let slot = parts
                .entry(msg_id.clone())
                .or_insert_with(|| (Instant::now(), vec![None; sum]));
            if seq < slot.1.len() {
                slot.1[seq] = Some(payload);
            }
            if slot.1.iter().any(Option::is_none) {
                continue;
            }
            payload = parts
                .remove(&msg_id)
                .unwrap()
                .1
                .into_iter()
                .flatten()
                .flatten()
                .collect();
        }
        let start = Instant::now();
        let resp = if frame.header("type") == Some("event") {
            match serde_json::from_slice::<Value>(&payload) {
                Ok(ev) => {
                    let result = handle_event(&ev, on_action);
                    if result.is_null() {
                        json!({"code": 200})
                    } else {
                        let b64 =
                            base64::engine::general_purpose::STANDARD.encode(result.to_string());
                        json!({"code": 200, "data": b64})
                    }
                }
                Err(e) => {
                    warn!("feishu: bad event payload: {e}");
                    json!({"code": 500})
                }
            }
        } else {
            continue; // other message types need no reply
        };
        frame
            .headers
            .push(("biz_rt".into(), start.elapsed().as_millis().to_string()));
        frame.payload = Some(resp.to_string().into_bytes());
        ws.send(Message::Binary(frame.encode()))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let f = Frame {
            seq_id: 7,
            log_id: 300,
            service: 33554678,
            method: 1,
            headers: vec![("type".into(), "event".into()), ("sum".into(), "1".into())],
            payload: Some(b"{\"a\":1}".to_vec()),
            log_id_new: Some("x".into()),
            ..Default::default()
        };
        assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
        // required fields are always present, even when zero (proto2)
        assert!(ping_frame(5)
            .encode()
            .starts_with(&[0x08, 0x00, 0x10, 0x00, 0x18, 0x05, 0x20, 0x00]));
    }

    #[test]
    fn decodes_known_bytes_from_the_python_sdk() {
        // Frame(SeqID=1, LogID=2, service=3, method=0, headers=[type=ping]) serialised by lark-oapi
        let bytes = [
            0x08, 0x01, 0x10, 0x02, 0x18, 0x03, 0x20, 0x00, 0x2a, 0x0c, 0x0a, 0x04, b't', b'y',
            b'p', b'e', 0x12, 0x04, b'p', b'i', b'n', b'g',
        ];
        let f = Frame::decode(&bytes).unwrap();
        assert_eq!((f.seq_id, f.log_id, f.service, f.method), (1, 2, 3, 0));
        assert_eq!(f.header("type"), Some("ping"));
        assert_eq!(f.encode(), bytes);
    }

    #[test]
    fn card_event_to_result() {
        let ev = json!({"schema": "2.0", "header": {"event_type": "card.action.trigger"},
            "event": {"operator": {"open_id": "ou_1"}, "action": {"value": {"action": "approve", "key": "o/r#1"}},
                      "context": {"open_message_id": "om_1", "open_chat_id": "oc_1"}}});
        let cb = |v: Value, ids: ClickIds| -> Result<Option<Value>> {
            assert_eq!(v["key"], "o/r#1");
            assert_eq!(
                (
                    ids.open_id.as_str(),
                    ids.message_id.as_str(),
                    ids.chat_id.as_str()
                ),
                ("ou_1", "om_1", "oc_1")
            );
            Ok(Some(json!({"card": 1})))
        };
        let r = handle_event(&ev, &cb);
        assert_eq!(r["card"]["type"], "raw");
        assert_eq!(r["card"]["data"]["card"], 1);
        let err = |_: Value, _: ClickIds| -> Result<Option<Value>> { Err(anyhow!("no")) };
        assert_eq!(handle_event(&ev, &err)["toast"]["type"], "error");
        assert!(handle_event(
            &json!({"header": {"event_type": "im.message.receive_v1"}}),
            &cb
        )
        .is_null());
    }
}
