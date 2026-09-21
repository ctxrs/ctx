import React, { StrictMode } from 'react';
import { createRoot, hydrateRoot } from 'react-dom/client';
import { App } from './App';
import { loadBrandFonts, revealBrandFontGate } from './lib/brand-fonts';
import './index.css';

void React;

const appElement = document.getElementById('app');

if (!appElement) {
  throw new Error('Missing #app root element');
}

const app = (
  <StrictMode>
    <App pathname={window.location.pathname} />
  </StrictMode>
);

if (appElement.childElementCount === 0) {
  createRoot(appElement).render(app);
} else {
  hydrateRoot(appElement, app);
}

void loadBrandFonts({ includeItalic: true }).finally(revealBrandFontGate);
