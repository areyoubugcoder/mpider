// 核对真实产物，阻止部署子路径断链、错误 canonical 或缺失内容进入 Pages。
import assert from 'node:assert/strict';
import { readFile, readdir, access } from 'node:fs/promises';
import path from 'node:path';

const output = path.resolve('dist');
const deployment = new URL(process.env.SITE_URL || 'https://areyoubugcoder.github.io/mpider/');
const prefix = deployment.pathname.replace(/\/+$/, '');
deployment.pathname = `${prefix}/`;
const files = [];
async function walk(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    assert(!entry.isSymbolicLink(), `发布产物不能包含符号链接：${entry.name}`);
    const location = path.join(directory, entry.name);
    if (entry.isDirectory()) await walk(location); else files.push(location);
  }
}
await walk(output);
const pages = files.filter((file) => file.endsWith('.html'));
assert(pages.length >= 4, '缺少首页、博客或文章页面');
const homepage = await readFile(path.join(output, 'index.html'), 'utf8');
assert(homepage.includes('免费开源') && homepage.includes('公众号批量下载'), '首屏内容必须直接存在于 HTML');
const summaries = [...homepage.matchAll(/<summary[^>]*>([\s\S]*?)<\/summary>/g)];
assert.equal(summaries[0]?.[1], '为什么开源？', '为什么开源必须是第一条 FAQ');
assert.equal([...homepage.matchAll(/<details\b[^>]*\bopen(?:\s|>|=)/g)].length, 4, '四条 FAQ 应默认展开');
for (const url of ['https://qm.qq.com/q/s4JavFxA3e', 'https://github.com/areyoubugcoder/mpider',
  'https://github.com/areyoubugcoder/mpider/releases/latest', 'https://xhslink.cn/o/8c23uHExcT6']) {
  assert(homepage.includes(url), `缺少已确认入口：${url}`);
}
assert(!homepage.includes('重播开场') && !homepage.includes('跳过开场'), '不应恢复已移除的开场');

for (const file of pages) {
  const html = await readFile(file, 'utf8');
  const relative = path.relative(output, file).split(path.sep).join('/');
  const pageUrl = new URL(relative === 'index.html' ? '' : relative.replace(/index\.html$/, ''), deployment);
  assert.equal([...html.matchAll(/<h1(?:\s|>)/g)].length, 1, `${relative} 应有且只有一个主标题`);
  assert(html.includes('lang="zh-CN"') && html.includes('name="description"'), `${relative} 缺少基础 SEO 元数据`);
  const tag = html.match(/<link\b(?=[^>]*rel="canonical")[^>]*>/)?.[0];
  const canonical = tag?.match(/href="([^"]+)"/)?.[1];
  assert(canonical, `${relative} 缺少 canonical`);
  const canonicalUrl = new URL(canonical);
  assert.equal(canonicalUrl.origin, deployment.origin, `${relative} canonical 域名不正确`);
  assert(canonicalUrl.pathname.startsWith(`${prefix}/`), `${relative} canonical 丢失部署子路径`);
  assert(!html.includes('/Users/') && !html.includes('localhost:'), `${relative} 含本机地址`);
  for (const match of html.matchAll(/<script\b[^>]*type="application\/ld\+json"[^>]*>([\s\S]*?)<\/script>/g)) JSON.parse(match[1]);
  for (const match of html.matchAll(/\b(?:href|src)="([^"]+)"/g)) {
    const value = match[1];
    if (/^(?:data:|mailto:|tel:|#)/.test(value)) continue;
    const linked = new URL(value.replace(/&amp;/g, '&'), pageUrl);
    if (linked.origin !== deployment.origin) continue;
    assert(linked.pathname.startsWith(`${prefix}/`), `${relative} 的链接丢失子路径：${value}`);
    let local = decodeURIComponent(linked.pathname.slice(prefix.length)).replace(/^\/+/, '');
    if (!local || local.endsWith('/')) local += 'index.html';
    const resolved = path.resolve(output, local);
    assert(resolved.startsWith(`${output}${path.sep}`), '站内路径越界');
    await access(resolved).catch(() => { throw new Error(`${relative} 存在站内断链：${value}`); });
  }
}
for (const filename of ['favicon.svg', 'preview.png', 'robots.txt', 'rss.xml', 'sitemap-index.xml']) await access(path.join(output, filename));
assert((await readFile(path.join(output, 'robots.txt'), 'utf8')).includes(new URL('sitemap-index.xml', deployment).href), 'robots.txt 中的站点地图地址错误');
const feed = await readFile(path.join(output, 'rss.xml'), 'utf8');
assert(feed.includes('<item>') && feed.includes(`${deployment.origin}${prefix}/blog/`), 'RSS 缺少文章');
for (const file of files.filter((name) => /sitemap.*\.xml$/.test(name))) {
  for (const match of (await readFile(file, 'utf8')).matchAll(/<loc>([^<]+)<\/loc>/g)) {
    assert(match[1].startsWith(deployment.href), `站点地图丢失域名或子路径：${match[1]}`);
    assert(!match[1].endsWith('/404/'), '404 页面不应加入站点地图');
  }
}
console.log(`发布检查通过：${pages.length} 个 HTML 页面，四条 FAQ、四个入口、站内链接、结构化数据、RSS 与站点地图均有效。`);
