import { spawnSync } from 'node:child_process';

// 每次发布都重新生成并核对独立域名产物，避免误上传 Pages 子路径版本。
const env = { ...process.env, SITE_URL: 'https://mpider.followto.us/', ASTRO_TELEMETRY_DISABLED: '1' };
const pnpm = process.env.npm_execpath;
if (!pnpm) throw new Error('请通过 pnpm build:cloudflare 或 pnpm deploy:cloudflare 运行');

for (const script of ['check', 'build', 'verify']) {
  const result = spawnSync(process.execPath, [pnpm, 'run', script], { env, stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
