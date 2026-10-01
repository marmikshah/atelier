import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  // Relative assets work on /atelier/, a custom domain, and local previews.
  base: './',
  server: { host: '127.0.0.1' },
  preview: { host: '127.0.0.1' },
});
