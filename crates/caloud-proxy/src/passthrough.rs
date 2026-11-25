//! `:443`：依 SNI 分流（FR-DOM-09，ADR 0009、ADR 0016）。
//!
//! 讀取 TLS ClientHello 的 SNI 後：
//! 1. 網域設為直通 → 整條 TCP 連線原封不動轉給 App（App 自己終結 TLS 並管理憑證，含 ACME TLS-ALPN-01）
//! 2. [`CertStore`] 有憑證 → rustls 終結 TLS，交給與 `:80` 同一套 HTTP 代理
//! 3. 其他 → 關閉連線
use crate::server::Proxy;
use crate::{CertStore, RouteTable, TunnelRegistry};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

/// ClientHello 最多讀取的位元組（單一 TLS record 上限 16 KiB + 標頭）
const MAX_HELLO: usize = 16 * 1024 + 5;

pub async fn serve_tls(addr: SocketAddr, routes: RouteTable, tunnels: TunnelRegistry, certs: CertStore) -> anyhow::Result<()> {
    serve_tls_listener(TcpListener::bind(addr).await?, routes, tunnels, certs).await
}

/// 同 [`serve_tls`]，但使用已綁定的 listener（測試用 port 0）
pub async fn serve_tls_listener(
    listener: TcpListener,
    routes: RouteTable,
    tunnels: TunnelRegistry,
    certs: CertStore,
) -> anyhow::Result<()> {
    tracing::info!(addr = %listener.local_addr()?, "tls listening");
    let acceptor = TlsAcceptor::from(crate::tls::server_config(certs.clone()));
    let proxy = Proxy::new(routes, tunnels, certs);
    loop {
        let (stream, peer) = listener.accept().await?;
        let proxy = proxy.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, peer, proxy, acceptor).await {
                tracing::debug!(%peer, error = %e, "tls connection closed");
            }
        });
    }
}

async fn handle(mut client: TcpStream, peer: SocketAddr, proxy: Proxy, acceptor: TlsAcceptor) -> anyhow::Result<()> {
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
    if let Some(up) = proxy.routes.lookup_passthrough_route(&sni) {
        // FR-OPS-09：環境在 Runner 節點上時經通道
        let mut upstream = proxy.tunnels.dial(up.node.as_deref(), &up.target).await?;
        upstream.write_all(&buf).await?;
        tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
        return Ok(());
    }
    if proxy.certs.resolve(&sni).is_none() {
        anyhow::bail!("no passthrough route or certificate for {sni}");
    }
    // ADR 0016：已讀出的 ClientHello 先交還給 rustls
    let tls = tokio::time::timeout(
        Duration::from_secs(10),
        acceptor.accept(Rewind {
            prefix: buf,
            pos: 0,
            inner: client,
        }),
    )
    .await??;
    proxy.serve_conn(tls, peer, true).await;
    Ok(())
}

/// 先讀回已預讀的位元組，再讀底層連線
struct Rewind {
    prefix: Vec<u8>,
    pos: usize,
    inner: TcpStream,
}

impl AsyncRead for Rewind {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, out: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        if self.pos < self.prefix.len() {
            let n = (self.prefix.len() - self.pos).min(out.remaining());
            let start = self.pos;
            out.put_slice(&self.prefix[start..start + n]);
            self.pos += n;
            if self.pos == self.prefix.len() {
                self.prefix = Vec::new();
                self.pos = 0;
            }
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, out)
    }
}

impl AsyncWrite for Rewind {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, b)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
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
    use std::sync::Arc;

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
            node: None,
            commit: None,
        }]);
        let front = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let front_addr = front.local_addr().unwrap();
        drop(front);
        let r2 = routes.clone();
        tokio::spawn(async move { serve_tls(front_addr, r2, TunnelRegistry::default(), CertStore::default()).await });
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

    /// 回應請求的 x-forwarded-proto 與 host 的上游
    async fn echo_upstream() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                    let proto = req
                        .lines()
                        .find_map(|l| l.strip_prefix("x-forwarded-proto: "))
                        .unwrap_or("-")
                        .to_string();
                    let body = format!("proto={proto}");
                    let _ = s
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                });
            }
        });
        addr
    }

    fn client_config(ca_pem: &str) -> Arc<rustls::ClientConfig> {
        use rustls_pki_types::pem::PemObject;
        let mut roots = rustls::RootCertStore::empty();
        for c in rustls_pki_types::CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
            roots.add(c.unwrap()).unwrap();
        }
        let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(cfg)
    }

    // FR-DOM-05、FR-OPS-10：有憑證的網域由 Edge 終結 TLS；直通照舊；其他關閉
    #[tokio::test]
    async fn terminates_tls_for_hosts_with_certificates() {
        use crate::tls::{parse_pem, tests::self_signed};
        use caloud_core::model::Route;
        let up = echo_upstream().await;
        let routes = RouteTable::default();
        routes.replace(vec![Route {
            hostname: "q7v2m4kx.caloud.tw".into(),
            upstream: format!("http://{up}"),
            passthrough: None,
            node: None,
            commit: None,
        }]);
        let certs = CertStore::default();
        let (cert, key) = self_signed(&["caloud.tw", "*.caloud.tw"]);
        certs.insert(parse_pem(&cert, &key).unwrap());

        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(serve_tls_listener(l, routes, TunnelRegistry::default(), certs));

        let connector = tokio_rustls::TlsConnector::from(client_config(&cert));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = rustls_pki_types::ServerName::try_from("q7v2m4kx.caloud.tw").unwrap();
        let mut tls = connector.connect(name, tcp).await.unwrap();
        tls.write_all(b"GET / HTTP/1.1\r\nhost: q7v2m4kx.caloud.tw\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        tls.read_to_string(&mut out).await.ok();
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        assert!(out.ends_with("proto=https"), "{out}");

        // 有 wildcard 憑證但沒有路由：TLS 成功、回 404
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = rustls_pki_types::ServerName::try_from("nothing.caloud.tw").unwrap();
        let mut tls = connector.connect(name, tcp).await.unwrap();
        tls.write_all(b"GET / HTTP/1.1\r\nhost: nothing.caloud.tw\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        tls.read_to_string(&mut out).await.ok();
        assert!(out.starts_with("HTTP/1.1 404"), "{out}");

        // 沒有憑證的網域（兩層子網域不配 wildcard）：連線被關閉
        let tcp = TcpStream::connect(addr).await.unwrap();
        let name = rustls_pki_types::ServerName::try_from("a.b.caloud.tw").unwrap();
        assert!(connector.connect(name, tcp).await.is_err());
    }

    // FR-DOM-05：:80 有憑證的 host 回 308；ACME token 不轉址；沒有憑證照舊
    #[tokio::test]
    async fn http_redirects_hosts_with_certificates() {
        use crate::tls::{parse_pem, tests::self_signed};
        let certs = CertStore::default();
        let (cert, key) = self_signed(&["*.caloud.tw"]);
        certs.insert(parse_pem(&cert, &key).unwrap());
        let routes = RouteTable::default();
        routes.set_acme_challenge("tok".into(), "tok.key".into());
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(crate::serve_listener(l, routes, TunnelRegistry::default(), certs));

        async fn get(addr: SocketAddr, host: &str, path: &str) -> String {
            let mut c = TcpStream::connect(addr).await.unwrap();
            c.write_all(format!("GET {path} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut out = String::new();
            c.read_to_string(&mut out).await.ok();
            out.to_ascii_lowercase()
        }
        let r = get(addr, "abc.caloud.tw:80", "/x?y=1").await;
        assert!(r.starts_with("http/1.1 308"), "{r}");
        assert!(r.contains("location: https://abc.caloud.tw/x?y=1"), "{r}");
        let r = get(addr, "abc.caloud.tw", "/.well-known/acme-challenge/tok").await;
        assert!(r.starts_with("http/1.1 200") && r.ends_with("tok.key"), "{r}");
        let r = get(addr, "relay.cafeca.io", "/").await;
        assert!(r.starts_with("http/1.1 404"), "{r}");
    }
}
