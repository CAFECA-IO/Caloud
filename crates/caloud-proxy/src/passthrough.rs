//! 443 TLS 直通（FR-DOM-09，ADR 0009）。
//!
//! 讀取 TLS ClientHello 的 SNI，網域設為直通時把整條 TCP 連線原封不動轉給 App；
//! App 自己終結 TLS 並管理憑證（含 ACME TLS-ALPN-01）。其他網域在 Caloud 自己的 HTTPS（M2-01）完成前一律關閉連線。
use crate::RouteTable;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// ClientHello 最多讀取的位元組（單一 TLS record 上限 16 KiB + 標頭）
const MAX_HELLO: usize = 16 * 1024 + 5;

pub async fn serve_tls_passthrough(addr: SocketAddr, routes: RouteTable) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "tls passthrough listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        let routes = routes.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, &routes).await {
                tracing::debug!(%peer, error = %e, "tls passthrough closed");
            }
        });
    }
}

async fn handle(mut client: TcpStream, routes: &RouteTable) -> anyhow::Result<()> {
    let mut buf = Vec::with_capacity(2048);
    let sni = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match parse_sni(&buf) {
                Hello::Done(sni) => return Ok(sni),
                Hello::NeedMore if buf.len() < MAX_HELLO => {
                    let mut chunk = [0u8; 4096];
                    let n = client.read(&mut chunk).await?;
                    if n == 0 {
                        anyhow::bail!("client closed before ClientHello");
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Hello::NeedMore => anyhow::bail!("ClientHello too large"),
            }
        }
    })
    .await??;
    let Some(sni) = sni else { anyhow::bail!("no SNI") };
    let Some(target) = routes.lookup_passthrough(&sni) else {
        anyhow::bail!("no passthrough route for {sni}");
    };
    let mut upstream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&target)).await??;
    upstream.write_all(&buf).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Hello {
    /// 還需要更多資料
    NeedMore,
    /// 解析完成；None 表示不是 TLS 或沒有 SNI
    Done(Option<String>),
}

/// 從（可能不完整的）TLS 位元組解析 ClientHello 的 server_name
pub fn parse_sni(buf: &[u8]) -> Hello {
    if buf.len() < 5 {
        return Hello::NeedMore;
    }
    // TLS record：type 22（handshake）
    if buf[0] != 0x16 {
        return Hello::Done(None);
    }
    let rec_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if buf.len() < 5 + rec_len {
        return Hello::NeedMore;
    }
    Hello::Done(client_hello_sni(&buf[5..5 + rec_len]))
}

fn client_hello_sni(h: &[u8]) -> Option<String> {
    let mut r = Reader(h);
    if r.u8()? != 1 {
        return None; // 不是 ClientHello
    }
    r.skip(3)?; // handshake 長度
    r.skip(2 + 32)?; // version + random
    let sid = r.u8()? as usize;
    r.skip(sid)?;
    let cs = r.u16()? as usize;
    r.skip(cs)?;
    let comp = r.u8()? as usize;
    r.skip(comp)?;
    let ext_len = r.u16()? as usize;
    let mut ext = Reader(r.take(ext_len)?);
    while !ext.0.is_empty() {
        let ty = ext.u16()?;
        let len = ext.u16()? as usize;
        let body = ext.take(len)?;
        if ty == 0 {
            let mut sn = Reader(body);
            let list = sn.u16()? as usize;
            let mut list = Reader(sn.take(list)?);
            while !list.0.is_empty() {
                let kind = list.u8()?;
                let n = list.u16()? as usize;
                let name = list.take(n)?;
                if kind == 0 {
                    return std::str::from_utf8(name).ok().map(str::to_ascii_lowercase);
                }
            }
        }
    }
    None
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 組出最小的 ClientHello（含 server_name 與 ALPN 兩個擴充）
    fn hello(sni: &str) -> Vec<u8> {
        let name = sni.as_bytes();
        let mut sn = vec![];
        sn.extend(((name.len() + 3) as u16).to_be_bytes());
        sn.push(0);
        sn.extend((name.len() as u16).to_be_bytes());
        sn.extend(name);
        let alpn = b"\x00\x0b\x0aacme-tls/1";
        let mut ext = vec![];
        ext.extend(16u16.to_be_bytes());
        ext.extend((alpn.len() as u16).to_be_bytes());
        ext.extend(alpn);
        ext.extend(0u16.to_be_bytes());
        ext.extend((sn.len() as u16).to_be_bytes());
        ext.extend(sn);
        let mut body = vec![0x03, 0x03];
        body.extend([7u8; 32]);
        body.push(0); // session id
        body.extend(2u16.to_be_bytes());
        body.extend([0x13, 0x01]);
        body.extend([1, 0]); // compression
        body.extend((ext.len() as u16).to_be_bytes());
        body.extend(ext);
        let mut hs = vec![1];
        hs.extend(&(body.len() as u32).to_be_bytes()[1..]);
        hs.extend(body);
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend((hs.len() as u16).to_be_bytes());
        rec.extend(hs);
        rec
    }

    // FR-DOM-09
    #[test]
    fn parses_sni_and_waits_for_full_record() {
        let h = hello("Relay.CAFECA.io");
        assert_eq!(parse_sni(&h), Hello::Done(Some("relay.cafeca.io".into())));
        assert_eq!(parse_sni(&h[..3]), Hello::NeedMore);
        assert_eq!(parse_sni(&h[..h.len() - 1]), Hello::NeedMore);
        assert_eq!(parse_sni(b"GET / HTTP/1.1\r\n"), Hello::Done(None));
    }

    // FR-DOM-09：未設定直通的網域關閉連線；設定後原封不動轉發
    #[tokio::test]
    async fn forwards_raw_bytes_for_passthrough_domains() {
        use caloud_core::model::Route;
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_addr = upstream.local_addr().unwrap();
        let routes = RouteTable::default();
        routes.replace(vec![Route {
            hostname: "relay.cafeca.io".into(),
            upstream: "http://x".into(),
            passthrough: Some(up_addr.to_string()),
        }]);
        let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let front_addr = front.local_addr().unwrap();
        drop(front);
        let r2 = routes.clone();
        tokio::spawn(async move { serve_tls_passthrough(front_addr, r2).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let h = hello("relay.cafeca.io");
        let expect = h.clone();
        let server = tokio::spawn(async move {
            let (mut s, _) = upstream.accept().await.unwrap();
            let mut got = vec![0u8; expect.len()];
            s.read_exact(&mut got).await.unwrap();
            assert_eq!(got, expect);
            s.write_all(b"pong").await.unwrap();
        });
        let mut c = TcpStream::connect(front_addr).await.unwrap();
        c.write_all(&h).await.unwrap();
        let mut pong = [0u8; 4];
        c.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        server.await.unwrap();

        // 沒有直通設定的網域：直接關閉
        let mut c = TcpStream::connect(front_addr).await.unwrap();
        c.write_all(&hello("other.cafeca.io")).await.unwrap();
        let mut b = [0u8; 1];
        assert_eq!(c.read(&mut b).await.unwrap_or(0), 0);
    }
}
