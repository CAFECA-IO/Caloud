//! Runner 通道的共用工具（docs/06 §6.1，ADR 0011）：控制面與 `caloud-agent` 共用。
//!
//! - `ws_io`：把「Bytes 串流＋Sink」（WebSocket 的 binary frame）包成 AsyncRead＋AsyncWrite，供 yamux 使用
//! - `CONNECT host:port` 行協定與目標檢查
use bytes::Bytes;
use futures_util::{Sink, Stream};
use std::io;
use std::net::IpAddr;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_util::io::{CopyToBytes, SinkWriter, StreamReader};

/// 通道端點
pub const TUNNEL_PATH: &str = "/agent/v1/tunnel";
/// `CONNECT` 行的長度上限
const MAX_LINE: usize = 300;

/// 把 binary frame 的串流與 sink 組成雙向 byte stream
pub fn ws_io<S, K>(stream: S, sink: K) -> impl AsyncRead + AsyncWrite + Unpin + Send
where
    S: Stream<Item = io::Result<Bytes>> + Send + 'static,
    K: Sink<Bytes, Error = io::Error> + Send + 'static,
{
    let reader = StreamReader::new(Box::pin(stream));
    let writer = SinkWriter::new(CopyToBytes::new(Box::pin(sink)));
    tokio::io::join(reader, writer)
}

/// 讀一行（不含換行）；逐位元組讀取，不會多讀到後面的原始資料
pub async fn read_line<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<String> {
    let mut buf = Vec::with_capacity(64);
    loop {
        let b = r.read_u8().await?;
        if b == b'\n' {
            break;
        }
        if buf.len() >= MAX_LINE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
        buf.push(b);
    }
    String::from_utf8(buf).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not utf-8"))
}

pub async fn write_line<W: AsyncWrite + Unpin>(w: &mut W, line: &str) -> io::Result<()> {
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

/// 控制面端：在新 stream 上要求 agent 連到 `target`，成功才回傳
pub async fn request_connect<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, target: &str) -> io::Result<()> {
    write_line(s, &format!("CONNECT {target}")).await?;
    match read_line(s).await?.as_str() {
        "OK" => Ok(()),
        other => Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            other.strip_prefix("ERR ").unwrap_or(other).to_string(),
        )),
    }
}

/// 解析 `CONNECT host:port`
pub fn parse_connect(line: &str) -> Result<(String, u16), String> {
    let target = line.strip_prefix("CONNECT ").ok_or("expected CONNECT")?;
    let (host, port) = target.rsplit_once(':').ok_or("expected host:port")?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port: u16 = port.parse().map_err(|_| "bad port")?;
    let ok_host = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'));
    if !ok_host || port == 0 {
        return Err("bad target".into());
    }
    Ok((host.to_string(), port))
}

/// 不允許經通道連線的位址：雲端 metadata（169.254.0.0/16、fd00:ec2::254）、未指定位址
pub fn forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast(),
        IpAddr::V6(v6) => {
            v6.is_unspecified() || v6.segments() == [0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254] || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-OPS-09
    #[test]
    fn parses_and_screens_targets() {
        assert_eq!(parse_connect("CONNECT web:3000"), Ok(("web".into(), 3000)));
        assert_eq!(parse_connect("CONNECT [::1]:80"), Ok(("::1".into(), 80)));
        assert!(parse_connect("GET / HTTP/1.1").is_err());
        assert!(parse_connect("CONNECT web").is_err());
        assert!(parse_connect("CONNECT web:0").is_err());
        assert!(parse_connect("CONNECT a b:80").is_err());
        assert!(forbidden_ip("169.254.169.254".parse().unwrap()));
        assert!(forbidden_ip("0.0.0.0".parse().unwrap()));
        assert!(forbidden_ip("fd00:ec2::254".parse().unwrap()));
        assert!(!forbidden_ip("10.0.0.5".parse().unwrap()));
        assert!(!forbidden_ip("127.0.0.1".parse().unwrap()));
    }

    // FR-OPS-09：ws_io 經由 binary frame 往返，CONNECT 協定一行不多讀
    #[tokio::test]
    async fn ws_io_round_trips_and_connect_protocol() {
        use futures_util::SinkExt;
        use tokio_util::sync::PollSender;
        fn rx_stream(rx: tokio::sync::mpsc::Receiver<Bytes>) -> impl Stream<Item = io::Result<Bytes>> {
            futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|b| (Ok(b), rx)) })
        }
        let (a_tx, b_rx) = tokio::sync::mpsc::channel::<Bytes>(8);
        let (b_tx, a_rx) = tokio::sync::mpsc::channel::<Bytes>(8);
        let io_err = |_| io::Error::from(io::ErrorKind::BrokenPipe);
        let mut a = ws_io(rx_stream(a_rx), PollSender::new(a_tx).sink_map_err(io_err));
        let mut b = ws_io(rx_stream(b_rx), PollSender::new(b_tx).sink_map_err(io_err));
        let server = tokio::spawn(async move {
            let line = read_line(&mut b).await.unwrap();
            assert_eq!(parse_connect(&line), Ok(("web".into(), 80)));
            write_line(&mut b, "OK").await.unwrap();
            let mut buf = [0u8; 5];
            b.read_exact(&mut buf).await.unwrap();
            b.write_all(&buf).await.unwrap();
            b.flush().await.unwrap();
        });
        request_connect(&mut a, "web:80").await.unwrap();
        a.write_all(b"hello").await.unwrap();
        a.flush().await.unwrap();
        let mut back = [0u8; 5];
        a.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
        server.await.unwrap();
    }
}
