import './styles/globals.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { PublicQuery } from './components/public-query';
import { errorText } from './components/shared';
import { PreferencesProvider } from './lib/preferences';
import { api } from './api';

const adminPage = location.pathname.replace(/\/$/, '') === '/admin';
document.title = adminPage ? 'Chain Panel · 管理控制台' : 'Chain Panel · 代理服务查询';

async function start() {
  let loginError = '';
  if (adminPage && location.hash.startsWith('#fleet-ticket=')) {
    const ticket = new URLSearchParams(location.hash.slice(1)).get('fleet-ticket');
    history.replaceState(null, '', location.pathname + '#dashboard');
    try {
      await api('/federation/login', 'POST', { ticket });
    } catch (e) {
      loginError = `免密登录失败：${errorText(e)}。请从多机总控重新进入或使用密钥登录。`;
    }
  }
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <PreferencesProvider>
        {loginError && (
          <p
            role="alert"
            className="border-b border-destructive/20 bg-destructive/10 px-4 py-2.5 text-center text-xs font-medium text-destructive"
          >
            {loginError}
          </p>
        )}
        {adminPage ? <App /> : <PublicQuery />}
      </PreferencesProvider>
    </StrictMode>,
  );
}
void start();
