import { svelteTesting } from '@testing-library/svelte/vite';
import { defineConfig, mergeConfig } from 'vitest/config';
import viteConfig from './vite.config.js';

export default mergeConfig(viteConfig, defineConfig({
  plugins: [svelteTesting()],
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test-setup.js'],
  },
}));
