import React, { StrictMode } from 'react';
import { hydrateRoot } from 'react-dom/client';
import { App } from './App';
import { loadBrandFonts, revealBrandFontGate } from './lib/brand-fonts';
import './index.css';

void React;

const appElement = document.getElementById('app');

if (!appElement) {
  throw new Error('Missing #app root element');
}

hydrateRoot(
  appElement,
  <StrictMode>
    <App pathname={window.location.pathname} />
  </StrictMode>,
);

void loadBrandFonts({ includeItalic: true }).finally(revealBrandFontGate);
