use crate::management::Channels;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::Duration,
};

pub async fn telegram(channels: &Channels, message: &str) -> Result<()> {
    let response = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none()).build()?
        .post(format!("https://api.telegram.org/bot{}/sendMessage", channels.telegram_token))
        .json(&serde_json::json!({"chat_id":channels.telegram_chat,"text":message.chars().take(4000).collect::<String>()}))
        .send().await.map_err(|_| anyhow::anyhow!("Telegram 请求失败，请检查连接和 Bot 配置"))?;
    ensure!(
        response.status().is_success(),
        "Telegram 拒绝通知（HTTP {}）",
        response.status().as_u16()
    );
    let result: serde_json::Value = response.json().await.context("Telegram 响应无效")?;
    ensure!(result["ok"] == true, "Telegram 未接受通知");
    Ok(())
}

fn reply(stream: &mut impl Read, expected: u16) -> Result<()> {
    let mut total = 0;
    loop {
        let mut line = Vec::new();
        loop {
            let mut byte = [0];
            stream.read_exact(&mut byte)?;
            total += 1;
            ensure!(total < 16384, "SMTP 响应过长");
            line.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        ensure!(line.len() >= 4, "SMTP 响应无效");
        let code = std::str::from_utf8(&line[..3])?.parse::<u16>()?;
        ensure!(code == expected, "SMTP 返回 {code}，预期 {expected}");
        if line[3] == b' ' {
            return Ok(());
        }
        ensure!(line[3] == b'-', "SMTP 响应无效");
    }
}
fn command(stream: &mut (impl Read + Write), text: &str, expected: u16) -> Result<()> {
    stream.write_all(text.as_bytes())?;
    stream.flush()?;
    reply(stream, expected)
}

pub fn email(channels: &Channels, message: &str) -> Result<()> {
    channels.validate()?;
    let address = (channels.smtp_host.as_str(), channels.smtp_port)
        .to_socket_addrs()?
        .next()
        .context("SMTP 主机无地址")?;
    let mut tcp = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    tcp.set_read_timeout(Some(Duration::from_secs(15)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(15)))?;
    if channels.smtp_starttls {
        reply(&mut tcp, 220)?;
        command(&mut tcp, "EHLO chain-panel\r\n", 250)?;
        command(&mut tcp, "STARTTLS\r\n", 220)?;
    }
    let mut tls = openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls())?;
    // Vendored OpenSSL may use a different default trust directory than the host OS.
    for bundle in [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
        "/etc/ssl/cert.pem",
    ] {
        if std::path::Path::new(bundle).is_file() {
            tls.set_ca_file(bundle)?;
            break;
        }
    }
    let tls = tls.build();
    let mut stream = tls.connect(&channels.smtp_host, tcp)?;
    if !channels.smtp_starttls {
        reply(&mut stream, 220)?;
    }
    command(&mut stream, "EHLO chain-panel\r\n", 250)?;
    if !channels.smtp_user.is_empty() {
        let credentials = STANDARD.encode(format!(
            "\0{}\0{}",
            channels.smtp_user, channels.smtp_password
        ));
        command(&mut stream, &format!("AUTH PLAIN {credentials}\r\n"), 235)?;
    }
    command(
        &mut stream,
        &format!("MAIL FROM:<{}>\r\n", channels.mail_from),
        250,
    )?;
    command(
        &mut stream,
        &format!("RCPT TO:<{}>\r\n", channels.mail_to),
        250,
    )?;
    command(&mut stream, "DATA\r\n", 354)?;
    let encoded = STANDARD.encode(message);
    let body = encoded
        .as_bytes()
        .chunks(76)
        .map(|s| std::str::from_utf8(s).unwrap())
        .collect::<Vec<_>>()
        .join("\r\n");
    let subject = STANDARD.encode("Chain Panel 运维通知");
    command(
        &mut stream,
        &format!(
            "From: <{}>\r\nTo: <{}>\r\nSubject: =?UTF-8?B?{subject}?=\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}\r\n.\r\n",
            channels.mail_from, channels.mail_to
        ),
        250,
    )?;
    let _ = command(&mut stream, "QUIT\r\n", 221);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn smtp_multiline_and_failure() {
        assert!(reply(&mut &b"250-example\r\n250 AUTH PLAIN\r\n"[..], 250).is_ok());
        assert!(reply(&mut &b"535 authentication failed\r\n"[..], 235).is_err());
        let channels = Channels {
            mail_from: "a@b\r\nBcc:x@y".into(),
            ..Default::default()
        };
        assert!(channels.validate().is_err());
    }
}
