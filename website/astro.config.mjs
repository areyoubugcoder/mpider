import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

// 同时支持项目子路径与独立域名；部署 URL 统一决定 canonical、资源和订阅地址。
const deployment = new URL(process.env.SITE_URL || 'https://areyoubugcoder.github.io/mpider/');
if (deployment.protocol !== 'https:') throw new Error('SITE_URL 必须使用 HTTPS');
const base = deployment.pathname.replace(/\/+$/, '') || '/';

export default defineConfig({
  site: deployment.origin,
  base,
  output: 'static',
  trailingSlash: 'always',
  integrations: [sitemap({ filter: (url) => !url.endsWith('/404/') })],
  server: { host: '127.0.0.1', port: 3000 },
  vite: { server: { strictPort: true }, preview: { strictPort: true } },
});
