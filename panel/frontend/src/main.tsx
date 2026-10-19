import './styles/globals.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { PublicQuery } from './components/public-query';
import { api } from './api';

const adminPage = location.pathname.replace(/\/$/, '') === '/admin';
document.title = adminPage ? 'Chain · 管理控制台' : 'Chain · 代理服务查询';

async function start() {
let loginError = '';
if (adminPage && location.hash.startsWith('#fleet-ticket=')) {
  const ticket = new URLSearchParams(location.hash.slice(1)).get('fleet-ticket');
  history.replaceState(null, '', location.pathname + '#dashboard');
  try { await api('/federation/login', 'POST', { ticket }); }
  catch { loginError = '免密登录链接已失效，请从总控重新跳转或手动登录。'; }
}
createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {loginError && <p role="alert">{loginError}</p>}
    {adminPage ? <App /> : <PublicQuery />}
  </StrictMode>,
);
}
void start();
