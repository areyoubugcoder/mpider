// 公开链接与站内路径集中维护，独立域名切换时不需要逐篇修改文章。
export const product = {
  name: 'MPider',
  title: 'MPider｜免费开源的公众号文章批量下载工具',
  description: '免费开源的公众号文章下载工具。保存正文与图片到自己的 Windows 电脑，离线阅读，同时导出 Markdown 与 HTML。了解使用方法、账号风险与开源核查。',
  repository: 'https://github.com/areyoubugcoder/mpider',
  download: 'https://github.com/areyoubugcoder/mpider/releases/latest',
  community: 'https://qm.qq.com/q/s4JavFxA3e',
  demo: 'https://xhslink.cn/o/8c23uHExcT6',
};

export function sitePath(path = ''): string {
  const base = import.meta.env.BASE_URL.replace(/\/+$/, '');
  return `${base}/${path.replace(/^\/+/, '')}`;
}

export function siteUrl(path = ''): string {
  return new URL(sitePath(path), import.meta.env.SITE).href;
}

export function serializeSchema(value: unknown): string {
  return JSON.stringify(value).replace(/</g, '\\u003c');
}

export function plainText(html: string): string {
  return html.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
}

export function formatDate(date: Date): string {
  return new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric', month: 'long', day: 'numeric', timeZone: 'Asia/Taipei',
  }).format(date);
}
