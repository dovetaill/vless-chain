//! Authenticated, per-line SOCKS bridge between the ingress core and its existing egress route.
use crate::gateway::{Lane, relay};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
pub struct Spec {
    pub port: u16,
    pub backend: u16,
    pub password: String,
}

pub async fn serve(
    spec: Spec,
    user: String,
    lane: Arc<Lane>,
    cancel: CancellationToken,
) -> Result<JoinHandle<()>> {
    let listener = TcpListener::bind(("127.0.0.1", spec.port)).await?;
    Ok(tokio::spawn(async move {
        let mut children = JoinSet::new();
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                Some(_) = children.join_next(), if !children.is_empty() => {},
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(value) => value,
                        Err(error) => {
                            eprintln!("Meter TCP listener stopped: line={} port={} error={} errno={:?}", lane.id, spec.port, error, error.raw_os_error());
                            break;
                        }
                    };
                    if children.len() >= 1024 || !lane.available() { continue }
                    let (spec, user, lane) = (spec.clone(), user.clone(), lane.clone());
                    children.spawn(async move {
                        let stopped = lane.cancellation();
                        tokio::select! {
                            _ = stopped.cancelled() => {},
                            _ = connection(stream, &spec, &user, lane) => {},
                        }
                    });
                }
            }
        }
        children.abort_all();
        while children.join_next().await.is_some() {}
    }))
}

enum Session {
    Tcp(TcpStream),
    Udp {
        control: TcpStream,
        socket: UdpSocket,
        backend: UdpSocket,
    },
}

async fn address(stream: &mut TcpStream, kind: u8) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    let count = match kind {
        1 => 4,
        4 => 16,
        3 => {
            let n = stream.read_u8().await?;
            ensure!(n > 0, "SOCKS 域名为空");
            bytes.push(n);
            n as usize
        }
        _ => anyhow::bail!("SOCKS 地址类型无效"),
    };
    let mut tail = vec![0; count + 2];
    stream.read_exact(&mut tail).await?;
    bytes.extend(tail);
    Ok(bytes)
}

async fn handshake(client: &mut TcpStream, spec: &Spec, user: &str, policy: &crate::rules::CompiledPolicy) -> Result<Session> {
    let mut greeting = [0; 2];
    client.read_exact(&mut greeting).await?;
    ensure!(greeting[0] == 5 && greeting[1] > 0, "SOCKS 版本无效");
    let mut methods = vec![0; greeting[1] as usize];
    client.read_exact(&mut methods).await?;
    if !methods.contains(&2) {
        client.write_all(&[5, 255]).await?;
        anyhow::bail!("计量桥需要身份认证");
    }
    client.write_all(&[5, 2]).await?;
    client.read_exact(&mut greeting).await?;
    ensure!(greeting[0] == 1 && greeting[1] > 0, "SOCKS 认证无效");
    let mut username = vec![0; greeting[1] as usize];
    client.read_exact(&mut username).await?;
    let mut password = vec![0; client.read_u8().await? as usize];
    client.read_exact(&mut password).await?;
    let valid = username.ct_eq(user.as_bytes()) & password.ct_eq(spec.password.as_bytes());
    client
        .write_all(&[1, if bool::from(valid) { 0 } else { 1 }])
        .await?;
    ensure!(bool::from(valid), "计量桥身份无效");

    let mut request = [0; 4];
    client.read_exact(&mut request).await?;
    ensure!(
        request[0] == 5 && request[2] == 0 && matches!(request[1], 1 | 3),
        "SOCKS 请求无效"
    );
    let mut destination = address(client, request[3]).await?;
    if request[1] == 1 {
        match policy.destination(request[3], &destination, false).await {
            Ok((kind,bytes)) => { request[3]=kind; destination=bytes; },
            Err(e) => { client.write_all(&[5,2,0,1,0,0,0,0,0,0]).await?; return Err(e); }
        }
    } else { ensure!(policy.policy.udp != "deny", "此代理禁止 UDP"); }

    let mut backend = TcpStream::connect(("127.0.0.1", spec.backend)).await?;
    backend.set_nodelay(true)?;
    backend.write_all(&[5, 1, 2]).await?;
    backend.read_exact(&mut greeting).await?;
    ensure!(greeting == [5, 2], "出站桥不支持认证");
    let mut auth = vec![1, username.len() as u8];
    auth.extend(&username);
    auth.push(password.len() as u8);
    auth.extend(&password);
    backend.write_all(&auth).await?;
    backend.read_exact(&mut greeting).await?;
    ensure!(greeting == [1, 0], "出站桥认证失败");
    backend.write_all(&request).await?;
    backend.write_all(&destination).await?;
    let mut reply = [0; 4];
    backend.read_exact(&mut reply).await?;
    ensure!(reply[0] == 5 && reply[2] == 0, "出站桥响应无效");
    let bound = address(&mut backend, reply[3]).await?;
    if reply[1] != 0 || request[1] == 1 {
        client.write_all(&reply).await?;
        client.write_all(&bound).await?;
        ensure!(reply[1] == 0, "出站桥拒绝连接");
        return Ok(Session::Tcp(backend));
    }
    let ip = match reply[3] {
        1 => IpAddr::V4(Ipv4Addr::new(bound[0], bound[1], bound[2], bound[3])),
        4 => IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&bound[..16])?)),
        _ => anyhow::bail!("出站 UDP 桥必须绑定本机地址"),
    };
    ensure!(
        ip.is_loopback() || ip.is_unspecified(),
        "出站 UDP 桥必须绑定本机地址"
    );
    let port = u16::from_be_bytes(bound[bound.len() - 2..].try_into()?);
    ensure!(port > 0, "出站 UDP 桥端口无效");
    let destination = SocketAddr::new(
        if ip.is_unspecified() {
            Ipv4Addr::LOCALHOST.into()
        } else {
            ip
        },
        port,
    );
    let socket = UdpSocket::bind(("127.0.0.1", 0)).await?;
    let udp = UdpSocket::bind(("127.0.0.1", 0)).await?;
    udp.connect(destination).await?;
    client.write_all(&[5, 0, 0, 1, 127, 0, 0, 1]).await?;
    client
        .write_all(&socket.local_addr()?.port().to_be_bytes())
        .await?;
    Ok(Session::Udp {
        control: backend,
        socket,
        backend: udp,
    })
}

// SOCKS UDP address/framing bytes are transport overhead, never billable payload.
pub(crate) fn payload_size(packet: &[u8]) -> io::Result<usize> {
    if packet.len() < 4 || packet[..3] != [0, 0, 0] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let offset = match packet[3] {
        1 => 10,
        4 => 22,
        3 if packet.get(4).is_some_and(|n| *n > 0) => 7 + packet[4] as usize,
        _ => return Err(io::ErrorKind::InvalidData.into()),
    };
    packet
        .len()
        .checked_sub(offset)
        .ok_or_else(|| io::ErrorKind::InvalidData.into())
}

async fn connection(mut client: TcpStream, spec: &Spec, user: &str, lane: Arc<Lane>) -> Result<()> {
    client.set_nodelay(true)?;
    let _admission = lane.admit()?;
    let policy = lane.access_policy();
    let session =
        tokio::time::timeout(Duration::from_secs(15), handshake(&mut client, spec, user, &policy)).await??;
    match session {
        Session::Tcp(backend) => relay(client, backend, lane).await?,
        Session::Udp {
            mut control,
            socket,
            backend,
        } => {
            let _connection = lane.connection();
            let mut peer = None;
            let mut input = vec![0; 65536];
            let mut output = vec![0; 65536];
            let mut client_control = [0];
            let mut backend_control = [0];
            loop {
                tokio::select! {
                    _ = client.read(&mut client_control) => break,
                    _ = control.read(&mut backend_control) => break,
                    received = socket.recv_from(&mut input) => {
                        let (n, source) = received?;
                        if !source.ip().is_loopback() || peer.is_some_and(|p| p != source) { continue }
                        let Ok(size) = payload_size(&input[..n]) else { continue };
                        let address_end=n-size;
                        let Ok((kind,destination))=policy.destination(input[3],&input[4..address_end],true).await else {continue};
                        let mut packet=vec![0,0,0,kind];
                        packet.extend(destination);packet.extend_from_slice(&input[address_end..n]);
                        peer = Some(source);
                        let transfer = lane.transfer(size, true, true).await?;
                        backend.send(&packet).await?;
                        drop(transfer);
                    },
                    received = backend.recv(&mut output) => {
                        let n = received?;
                        let Some(peer) = peer else { continue };
                        let Ok(size) = payload_size(&output[..n]) else { continue };
                        let transfer = lane.transfer(size, false, true).await?;
                        socket.send_to(&output[..n], peer).await?;
                        drop(transfer);
                    },
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway;

    fn lane(dir: &std::path::Path) -> Arc<Lane> {
        let spec: gateway::Spec = serde_json::from_value(serde_json::json!({"id":"test","port":1,"backend":2,"kind":"vless","protocol":"vless","network":"tcp","limits":{},"tls":{},"target":""})).unwrap();
        Lane::new(&spec, gateway::Book::open(dir).unwrap())
    }

    #[test]
    fn udp_payload_excludes_addresses_and_rejects_fragments() {
        assert_eq!(
            payload_size(&[0, 0, 0, 1, 127, 0, 0, 1, 0, 53, 1, 2, 3]).unwrap(),
            3
        );
        assert_eq!(
            payload_size(&[0, 0, 0, 3, 1, b'a', 0, 53, 1, 2]).unwrap(),
            2
        );
        assert!(payload_size(&[0, 0, 1, 1, 127, 0, 0, 1, 0, 53, 1]).is_err());
        assert!(payload_size(&[0, 0, 0, 4, 1]).is_err());
    }

    #[tokio::test]
    async fn rejected_credentials_never_reach_backend_or_charge() {
        let dir = tempfile::tempdir().unwrap();
        let lane = lane(dir.path());
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_lane = lane.clone();
        let server = tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            connection(
                client,
                &Spec {
                    port: addr.port(),
                    backend: 1,
                    password: "correct".into(),
                },
                "test",
                server_lane,
            )
            .await
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        client.write_all(&[5, 1, 2]).await.unwrap();
        let mut result = [0; 2];
        client.read_exact(&mut result).await.unwrap();
        assert_eq!(result, [5, 2]);
        client.write_all(b"\x01\x04test\x05wrong").await.unwrap();
        client.read_exact(&mut result).await.unwrap();
        assert_eq!(result, [1, 1]);
        assert!(server.await.unwrap().is_err());
        let traffic = lane.book.snapshot();
        assert_eq!(
            traffic["test"].total_upload + traffic["test"].total_download,
            0
        );
    }

    async fn login(stream: &mut TcpStream) {
        stream.write_all(&[5, 1, 2]).await.unwrap();
        let mut reply = [0; 2];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [5, 2]);
        stream.write_all(b"\x01\x04test\x06secret").await.unwrap();
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [1, 0]);
    }

    #[tokio::test]
    async fn tcp_and_udp_count_payload_once_after_authentication() {
        for command in [1, 3] {
            let dir = tempfile::tempdir().unwrap();
            let lane = lane(dir.path());
            let backend = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let backend_port = backend.local_addr().unwrap().port();
            let backend_task = tokio::spawn(async move {
                let (mut control, _) = backend.accept().await.unwrap();
                let mut greeting = [0; 3];
                control.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1, 2]);
                control.write_all(&[5, 2]).await.unwrap();
                let mut auth = [0; 13];
                control.read_exact(&mut auth).await.unwrap();
                assert_eq!(&auth, b"\x01\x04test\x06secret");
                control.write_all(&[1, 0]).await.unwrap();
                let mut request = [0; 10];
                control.read_exact(&mut request).await.unwrap();
                assert_eq!(request[1], command);
                if command == 1 {
                    control
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
                        .await
                        .unwrap();
                    let mut payload = [0; 4];
                    control.read_exact(&mut payload).await.unwrap();
                    assert_eq!(&payload, b"ping");
                    control.write_all(b"pong").await.unwrap();
                    control.shutdown().await.unwrap();
                } else {
                    let socket = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
                    control
                        .write_all(&[5, 0, 0, 1, 127, 0, 0, 1])
                        .await
                        .unwrap();
                    control
                        .write_all(&socket.local_addr().unwrap().port().to_be_bytes())
                        .await
                        .unwrap();
                    let mut payload = [0; 64];
                    let (n, peer) = socket.recv_from(&mut payload).await.unwrap();
                    socket.send_to(&payload[..n], peer).await.unwrap();
                    assert!(control.read_u8().await.is_err());
                }
            });
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server_lane = lane.clone();
            let meter_task = tokio::spawn(async move {
                let (client, _) = listener.accept().await.unwrap();
                connection(
                    client,
                    &Spec {
                        port: addr.port(),
                        backend: backend_port,
                        password: "secret".into(),
                    },
                    "test",
                    server_lane,
                )
                .await
            });
            let mut client = TcpStream::connect(addr).await.unwrap();
            login(&mut client).await;
            client
                .write_all(&[5, command, 0, 1, 127, 0, 0, 1, 0, 53])
                .await
                .unwrap();
            let mut reply = [0; 10];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(
                (
                    lane.book.snapshot()["test"].total_upload,
                    lane.book.snapshot()["test"].total_download
                ),
                (0, 0)
            );
            if command == 1 {
                client.write_all(b"ping").await.unwrap();
                let mut output = [0; 4];
                client.read_exact(&mut output).await.unwrap();
                assert_eq!(&output, b"pong");
                client.shutdown().await.unwrap();
            } else {
                let socket = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
                socket
                    .connect((
                        "127.0.0.1",
                        u16::from_be_bytes(reply[8..].try_into().unwrap()),
                    ))
                    .await
                    .unwrap();
                let packet = b"\x00\x00\x00\x01\x7f\x00\x00\x01\x00\x35ping";
                socket.send(packet).await.unwrap();
                let mut output = [0; 64];
                let n = tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut output))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(&output[..n], packet);
            }
            drop(client);
            tokio::time::timeout(Duration::from_secs(2), meter_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), backend_task)
                .await
                .unwrap()
                .unwrap();
            let traffic = lane.book.snapshot();
            assert_eq!(
                (traffic["test"].total_upload, traffic["test"].total_download),
                (4, 4)
            );
        }
    }
}
