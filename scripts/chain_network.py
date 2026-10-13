"""Network discovery and an isolated, system-package Nginx HTTPS site."""
import contextlib
import errno
import fnmatch
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.request

from chain_common import atomic_write, output, require, run

SITE_OWNER = '# chain-panel HTTPS managed site v1'


def public_ipv4():
    """Only use assigned NIC addresses, never a proxy/NAT's outbound address."""
    if not shutil.which('ip'):
        install_packages(['iproute2' if shutil.which('apt-get') else 'iproute'])
    addresses = server_addresses()
    ipv4 = sorted(address for address in addresses if ':' not in address)
    if not ipv4:
        return None
    try:
        # This reads the kernel route and sends no packets. A tunnel's private
        # source or a proxy exit is never accepted unless assigned on a NIC.
        routes = json.loads(output(['ip', '-j', '-4', 'route', 'get', '1.1.1.1']))
        for route in routes:
            if route.get('prefsrc') in ipv4:
                return route['prefsrc']
    except (OSError, ValueError, subprocess.CalledProcessError):
        pass
    return ipv4[0]


def server_addresses(ipv4=None):
    addresses = {ipv4} if ipv4 else set()
    if shutil.which('ip'):
        try:
            for interface in json.loads(output(['ip', '-j', 'address', 'show', 'scope', 'global'])):
                if 'UP' not in interface.get('flags', []) or interface.get('ifname') == 'lo':
                    continue
                for item in interface.get('addr_info', []):
                    if item.get('tentative') or item.get('dadfailed'):
                        continue
                    address = ipaddress.ip_address(item['local'])
                    if address.is_global and not address.is_multicast:
                        addresses.add(str(address))
        except (OSError, ValueError, KeyError, subprocess.CalledProcessError):
            pass
    return addresses


def available_port():
    """Probe both protocol families and TCP/UDP; never reuse an occupied port."""
    for _ in range(256):
        port = 10000 + secrets.randbelow(50000)
        try:
            with contextlib.ExitStack() as stack:
                for family, host in ((socket.AF_INET, '0.0.0.0'), (socket.AF_INET6, '::')):
                    for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
                        try:
                            sock = stack.enter_context(socket.socket(family, kind))
                            if family == socket.AF_INET6:
                                sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
                            sock.bind((host, port))
                        except OSError as error:
                            if family == socket.AF_INET6 and error.errno in (errno.EAFNOSUPPORT, errno.EADDRNOTAVAIL):
                                continue
                            raise
                return port
        except OSError:
            continue
    raise ValueError('未找到空闲随机端口，请检查本机端口使用情况。')


def domain_name(value):
    value = value.strip().rstrip('.').lower()
    require(len(value) <= 253 and '.' in value and all(
        re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', label)
        for label in value.split('.')), '请填写域名，例如 panel.example.com，不含协议、端口或路径。')
    try:
        ipaddress.ip_address(value)
    except ValueError:
        return value
    raise ValueError('这里需要域名；使用 IP 访问请将域名留空。')


def domain_addresses(domain):
    """Use public DNS, not /etc/hosts; validate both A and AAAA records."""
    result = set()
    for record_type in ('A', 'AAAA'):
        last_error = None
        for resolver in ('https://dns.google/resolve', 'https://cloudflare-dns.com/dns-query'):
            try:
                request = urllib.request.Request(f'{resolver}?name={domain}&type={record_type}',
                                                 headers={'Accept': 'application/dns-json'})
                with urllib.request.urlopen(request, timeout=8) as response:
                    document = json.loads(response.read(65536))
                require(document.get('Status') in (0, 3), '公共 DNS 查询暂时失败。')
                for record in document.get('Answer', []):
                    if record.get('type') in (1, 28):
                        result.add(str(ipaddress.ip_address(record['data'])))
                last_error = None
                break
            except (OSError, ValueError, KeyError) as error:
                last_error = error
        require(last_error is None, f'无法查询 {domain} 的 {record_type} 记录，请检查服务器 HTTPS 出站网络后重试。')
    return result


def validate_dns(domain, addresses):
    resolved = domain_addresses(domain)
    require(resolved and resolved.issubset(addresses),
            f'域名解析不匹配：{domain} → {", ".join(sorted(resolved)) or "无 A/AAAA 记录"}；'
            f'本机地址：{", ".join(sorted(addresses)) or "未检测到公网地址"}。'
            '请修正 A/AAAA 记录；使用 CDN 代理时先关闭代理，再回车重试。')
    return resolved


def install_packages(packages):
    if not packages:
        return
    print('通过系统软件源安装：' + '、'.join(packages), flush=True)
    if shutil.which('apt-get'):
        run(['apt-get', 'update'])
        run(['apt-get', 'install', '-y', *packages], env={**os.environ, 'DEBIAN_FRONTEND': 'noninteractive'})
    elif shutil.which('dnf'):
        run(['dnf', 'install', '-y', *packages])
    else:
        raise ValueError('自动 HTTPS 需要 apt 或 dnf 软件源中的 nginx、certbot；也可留空域名使用 IP 访问。')


def nginx_site_directory(dump):
    """Select a wildcard include directly inside the main HTTP block."""
    main = re.split(r'^# configuration file ', dump, flags=re.M)[1:2]
    require(main, '无法读取 Nginx 主配置。')
    text = main[0].split('\n', 1)[1]
    lexer = shlex.shlex(text, posix=True, punctuation_chars='{};')
    lexer.whitespace_split = True
    stack, statement = [], []
    for token in lexer:
        # shlex can group punctuation such as ";}".
        tokens = list(token) if token and all(c in '{};' for c in token) else [token]
        for part in tokens:
            if part == '{':
                stack.append(statement[0] if statement else '')
                statement = []
            elif part == '}':
                if stack:
                    stack.pop()
                statement = []
            elif part == ';':
                if stack == ['http'] and len(statement) == 2 and statement[0] == 'include':
                    path = Path(statement[1])
                    if path.is_absolute() and path.name in ('*.conf', '*') and '*' not in str(path.parent):
                        return path.parent
                statement = []
            else:
                statement.append(part)
    raise ValueError('Nginx 的 http 配置中没有可用的独立站点 include；请使用系统默认 conf.d/sites-enabled 布局。')


def conflicting_site(dump, domain, owned):
    for section in re.split(r'^# configuration file ', dump, flags=re.M)[1:]:
        filename, _, body = section.partition('\n')
        if filename.rstrip(':') == str(owned):
            continue
        body = re.sub(r'#[^\n]*', '', body)
        for names in re.findall(r'\bserver_name\s+([^;]+);', body):
            for name in names.split():
                name = name.strip('"\'').lower()
                matches = fnmatch.fnmatchcase(domain, name) or (name.startswith('.') and
                          (domain == name[1:] or domain.endswith(name)))
                if name.startswith('~'):
                    try:
                        matches = bool(re.search(name.lstrip('~*'), domain))
                    except re.error:
                        matches = True
                if matches:
                    return filename.rstrip(':')
    return None


class HttpsSite:
    def __init__(self, domain, port, ipv6=False):
        self.domain, self.port, self.ipv6 = domain, port, ipv6
        self.webroot = Path('/var/lib/chain-acme')
        self.certname = 'chain-panel-' + domain
        self.cert = Path('/etc/letsencrypt/live') / self.certname
        self.site = None

    def prepare(self):
        # Refuse foreign port owners before package installation can start Nginx.
        require(shutil.which('ss'), '需要系统的 ss 命令（iproute2/iproute 包）检测 80/443 端口归属。')
        listeners = output(['ss', '-H', '-ltnp', '( sport = :80 or sport = :443 )'])
        for line in listeners.splitlines():
            require('"nginx"' in line, f'80/443 已被其他服务占用：{line}；请换用 IP 访问或先调整该服务。')
        install_packages([name for name in ('nginx', 'certbot') if not shutil.which(name)])
        details = subprocess.run(['nginx', '-V'], check=True, capture_output=True, text=True).stderr
        require('--conf-path=/etc/nginx/nginx.conf' in details,
                '检测到自定义 Nginx；自动 HTTPS 需要系统软件包的 /etc/nginx/nginx.conf，请留空域名或先调整配置。')
        pid = output(['systemctl', 'show', 'nginx', '--property=MainPID', '--value'])
        if pid.isdigit() and int(pid):
            process = Path('/proc') / pid
            require((process / 'exe').resolve() == Path(shutil.which('nginx')).resolve(),
                    '运行中的 Nginx 与系统命令不是同一程序，拒绝修改其配置。')
            command = shlex.split((process / 'cmdline').read_bytes().decode().replace('\0', ' '))
            if '-c' in command:
                index = command.index('-c') + 1
                require(index < len(command) and command[index] == '/etc/nginx/nginx.conf',
                        '运行中的 Nginx 使用自定义 -c 配置，无法自动接管。')
        run(['nginx', '-t'])
        dump = output(['nginx', '-T'])
        self.site = nginx_site_directory(dump) / 'chain-panel.conf'
        require(not self.site.is_symlink(), f'{self.site} 是已有链接，拒绝覆盖。')
        require(not self.site.exists() or self.site.read_text().startswith(SITE_OWNER + '\n'),
                f'{self.site} 不归本安装器管理，拒绝覆盖。')
        conflict = conflicting_site(dump, self.domain, self.site)
        require(not conflict, f'域名 {self.domain} 已在 {conflict} 配置，请使用独立面板域名。')

    def config(self, tls=False):
        ipv6_http = '    listen [::]:80;\n' if self.ipv6 else ''
        text = (f'{SITE_OWNER}\nserver {{\n    listen 80;\n{ipv6_http}    server_name {self.domain};\n'
                f'    location ^~ /.well-known/acme-challenge/ {{\n        root {self.webroot};\n'
                '        default_type text/plain;\n        try_files $uri =404;\n    }\n')
        text += (f'    location / {{ return 301 https://{self.domain}$request_uri; }}\n' if tls else
                 '    location / { return 404; }\n')
        text += '}\n'
        if tls:
            ipv6_tls = '    listen [::]:443 ssl;\n' if self.ipv6 else ''
            text += (f'server {{\n    listen 443 ssl;\n{ipv6_tls}    server_name {self.domain};\n'
                     f'    ssl_certificate {self.cert}/fullchain.pem;\n'
                     f'    ssl_certificate_key {self.cert}/privkey.pem;\n'
                     '    ssl_protocols TLSv1.2 TLSv1.3;\n    client_max_body_size 2m;\n'
                     f'    location / {{\n        proxy_pass http://127.0.0.1:{self.port};\n'
                     '        proxy_http_version 1.1;\n        proxy_set_header Host $http_host;\n'
                     '        proxy_set_header X-Real-IP $remote_addr;\n'
                     '        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n'
                     '        proxy_set_header X-Forwarded-Proto https;\n'
                     '        proxy_read_timeout 120s;\n    }\n}\n')
        return text

    def reload(self):
        run(['nginx', '-t'])
        active = subprocess.run(['systemctl', 'is-active', '--quiet', 'nginx'], check=False).returncode == 0
        run(['systemctl', 'reload' if active else 'start', 'nginx'])

    def challenge_check(self, timeout=15):
        directory = self.webroot / '.well-known/acme-challenge'
        for path in (self.webroot, self.webroot / '.well-known', directory):
            require(not path.is_symlink(), f'{path} 是已有链接，拒绝接管。')
            path.mkdir(parents=True, exist_ok=True, mode=0o755)
            path.chmod(0o755)  # root's umask 077 must not prevent Nginx traversal.
        challenge = directory / ('chain-check-' + secrets.token_hex(16))
        value = secrets.token_hex(32).encode()
        url = f'http://{self.domain}/.well-known/acme-challenge/{challenge.name}'
        try:
            atomic_write(challenge, value, 0o644)
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            deadline = time.monotonic() + timeout
            # systemctl reload returns after sending HUP, before Nginx's new
            # workers necessarily serve this vhost. Keep the same probe alive
            # while old workers may still return a default site's 404/redirect.
            while True:
                try:
                    with opener.open(url, timeout=max(.1, min(3, deadline - time.monotonic()))) as response:
                        if response.read(1024) == value:
                            return
                        detail = 'HTTP 响应内容与验证文件不匹配，请检查域名是否命中正确站点及代理/CDN 缓存。'
                except urllib.error.HTTPError as error:
                    detail = (f'HTTP {error.code} {error.reason}；请求已到达 HTTP 服务。'
                              '请检查 Nginx 站点是否生效、验证目录权限及代理/CDN 配置。')
                    error.close()
                except OSError as error:
                    reason = error.reason if isinstance(error, urllib.error.URLError) else error
                    detail = (f'连接错误：{reason}；请检查域名解析、本机到域名的路由及 TCP 80 入站规则。'
                              '本机自检不能证明公网可达性，证书签发还会从外部验证。')
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ValueError(f'HTTP 验证路径不可达或内容异常（已等待 {timeout:g} 秒）：{url}\n{detail}')
                time.sleep(min(.5, remaining))
        finally:
            challenge.unlink(missing_ok=True)

    def renewal_paths(self):
        return (Path('/etc/letsencrypt/renewal-hooks/deploy/chain-panel-nginx'),
                Path('/etc/systemd/system/chain-panel-cert-renew.service'),
                Path('/etc/systemd/system/chain-panel-cert-renew.timer'))

    def service_state(self, name):
        return tuple(subprocess.run(['systemctl', action, '--quiet', name], stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL).returncode == 0
                     for action in ('is-active', 'is-enabled'))

    def restore_service(self, name, previous):
        current = self.service_state(name)
        if current[0] != previous[0]:
            run(['systemctl', 'start' if previous[0] else 'stop', name])
        if current[1] != previous[1]:
            run(['systemctl', 'enable' if previous[1] else 'disable', name])

    def check_renewal_paths(self):
        for path in self.renewal_paths():
            require(not path.is_symlink() and (not path.exists() or SITE_OWNER in path.read_text()),
                    f'{path} 不归本安装器管理，拒绝覆盖。')

    def renewal(self):
        # A dedicated timer also works on distributions without a Certbot timer.
        hook, service, timer = self.renewal_paths()
        self.check_renewal_paths()
        nginx = shutil.which('nginx')
        certbot = shutil.which('certbot')
        atomic_write(hook, f'#!/bin/sh\n{SITE_OWNER}\nset -eu\n'
                     f'[ "${{RENEWED_LINEAGE:-}}" = {shlex.quote(str(self.cert))} ] || exit 0\n'
                     f'{shlex.quote(nginx)} -t\nsystemctl reload nginx\n', 0o755)
        atomic_write(service, f'{SITE_OWNER}\n[Unit]\nDescription=Renew Chain Panel HTTPS certificate\n'
                     '[Service]\nType=oneshot\n'
                     f'ExecStart={certbot} renew --quiet --cert-name {self.certname}\n', 0o644)
        atomic_write(timer, f'{SITE_OWNER}\n[Unit]\nDescription=Check Chain Panel certificate twice daily\n'
                     '[Timer]\nOnCalendar=*-*-* 00,12:00:00\nRandomizedDelaySec=3600\nPersistent=true\n'
                     '[Install]\nWantedBy=timers.target\n', 0o644)
        run(['systemctl', 'daemon-reload'])
        run(['systemctl', 'enable', '--now', timer.name])
        run(['systemctl', 'is-active', '--quiet', timer.name])

    @contextlib.contextmanager
    def activate(self):
        nginx_state = self.service_state('nginx')
        timer_state = self.service_state('chain-panel-cert-renew.timer')
        previous, renewal = None, {}
        site_written = False
        renewal_started = False
        try:
            self.prepare()
            self.check_renewal_paths()
            previous = self.site.read_bytes() if self.site.exists() else None
            renewal = {path: (path.read_bytes(), path.stat().st_mode & 0o777) if path.exists() else None
                       for path in self.renewal_paths()}
            site_written = True
            atomic_write(self.site, self.config(), 0o644)
            self.reload()
            self.challenge_check()
            print('正在申请 HTTPS 证书（Let’s Encrypt），并设置自动续期…', flush=True)
            run(['certbot', 'certonly', '--non-interactive', '--agree-tos', '--register-unsafely-without-email',
                 '--webroot', '--webroot-path', self.webroot, '--cert-name', self.certname,
                 '--domain', self.domain, '--keep-until-expiring'])
            require((self.cert / 'fullchain.pem').is_file() and (self.cert / 'privkey.pem').is_file(), '证书文件缺失。')
            atomic_write(self.site, self.config(tls=True), 0o644)
            self.reload()
            renewal_started = True
            self.renewal()
            run(['systemctl', 'enable', 'nginx'])
            yield
        except BaseException:
            if site_written:
                if previous is None:
                    self.site.unlink(missing_ok=True)
                else:
                    atomic_write(self.site, previous, 0o644)
            if renewal_started:
                for path, value in renewal.items():
                    if value is None:
                        path.unlink(missing_ok=True)
                    else:
                        atomic_write(path, value[0], value[1])
                run(['systemctl', 'daemon-reload'], check=False)
            try:
                if site_written and nginx_state[0]:
                    self.reload()
                self.restore_service('nginx', nginx_state)
                if renewal_started:
                    self.restore_service('chain-panel-cert-renew.timer', timer_state)
            except (OSError, subprocess.CalledProcessError) as error:
                print(f'站点/续期文件已恢复，但服务状态恢复失败：{error}；请执行 nginx -t 和 systemctl status nginx 检查。')
            raise
