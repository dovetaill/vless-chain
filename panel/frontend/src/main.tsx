import './styles/globals.css';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { PublicQuery } from './components/public-query';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {location.pathname.replace(/\/$/, '') === '/query' ? <PublicQuery /> : <App />}
  </StrictMode>,
);
