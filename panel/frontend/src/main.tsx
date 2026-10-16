import './styles/globals.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { PublicQuery } from './components/public-query';

const adminPage = location.pathname.replace(/\/$/, '') === '/admin';
document.title = adminPage ? 'Chain · 管理控制台' : 'Chain · 代理服务查询';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {adminPage ? <App /> : <PublicQuery />}
  </StrictMode>,
);
